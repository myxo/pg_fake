use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use sqlparser::ast::{self, VisitMut as _};

use crate::{
    analyzer,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor, parser,
    txn::Snapshot,
    value::{BaseType, PgType, Value},
};

use super::{Session, SessionTransactionState, StatementResult};

#[derive(Clone)]
struct ProceduralLocal {
    data_type: PgType,
    value: Value,
}

#[derive(Clone, Copy)]
pub(super) struct DoBlockContext {
    pub(super) deadline: Option<Instant>,
    pub(super) statement_timestamp: chrono::DateTime<chrono::Utc>,
}

struct ProceduralLocalSubstituter<'a> {
    locals: &'a BTreeMap<String, ProceduralLocal>,
    output_aliases: Vec<BTreeSet<String>>,
    protected_order_identifiers: Vec<bool>,
    group_by_depth: usize,
    group_expression_depth: usize,
}

impl ast::VisitorMut for ProceduralLocalSubstituter<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.output_aliases.push(match query.body.as_ref() {
            ast::SetExpr::Select(select) => select
                .projection
                .iter()
                .filter_map(|item| match item {
                    ast::SelectItem::ExprWithAlias { alias, .. } => {
                        Some(executor::normalize_identifier(alias))
                    }
                    _ => None,
                })
                .collect(),
            _ => BTreeSet::new(),
        });
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.output_aliases
            .pop()
            .expect("visited query pushed output aliases");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_order_by_expr(
        &mut self,
        order_by: &mut ast::OrderByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.protected_order_identifiers.push(
            matches!(&order_by.expr, ast::Expr::Identifier(identifier)
                if self.output_aliases.last().is_some_and(|aliases| aliases.contains(&executor::normalize_identifier(identifier)))),
        );
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_order_by_expr(
        &mut self,
        _order_by: &mut ast::OrderByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.protected_order_identifiers
            .pop()
            .expect("visited ORDER BY expression pushed alias protection");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_group_by(
        &mut self,
        _group_by: &mut ast::GroupByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.group_by_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_group_by(
        &mut self,
        _group_by: &mut ast::GroupByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.group_by_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let protected_group_identifier =
            self.group_by_depth != 0 && self.group_expression_depth == 0;
        if self.group_by_depth != 0 {
            self.group_expression_depth += 1;
        }
        let ast::Expr::Identifier(identifier) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let name = executor::normalize_identifier(identifier);
        if (self.protected_order_identifiers.last() == Some(&true) || protected_group_identifier)
            && self
                .output_aliases
                .last()
                .is_some_and(|aliases| aliases.contains(&name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let Some(local) = self.locals.get(&name) else {
            return std::ops::ControlFlow::Continue(());
        };
        *expression = analyzer::create_typed_literal(local.value.clone(), local.data_type);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_expr(
        &mut self,
        _expression: &mut ast::Expr,
    ) -> std::ops::ControlFlow<Self::Break> {
        if self.group_by_depth != 0 {
            self.group_expression_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

fn substitute_procedural_statement_locals(
    statement: &mut ast::Statement,
    locals: &BTreeMap<String, ProceduralLocal>,
) {
    let mut substituter = ProceduralLocalSubstituter {
        locals,
        output_aliases: Vec::new(),
        protected_order_identifiers: Vec::new(),
        group_by_depth: 0,
        group_expression_depth: 0,
    };
    let _ = statement.visit(&mut substituter);
}

fn format_procedural_exception(format: &str, arguments: &[Value]) -> Result<String> {
    let mut result = String::new();
    let mut arguments = arguments.iter();
    let mut characters = format.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            result.push(character);
            continue;
        }
        if characters.clone().next() == Some('%') {
            characters.next();
            result.push('%');
            continue;
        }
        let argument = arguments.next().ok_or_else(|| {
            PgError::create(
                SqlState::SyntaxError,
                "too few parameters specified for RAISE",
            )
        })?;
        if argument.is_null() {
            result.push_str("<NULL>");
        } else {
            result.push_str(&argument.format_postgres_text());
        }
    }
    if arguments.next().is_some() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "too many parameters specified for RAISE",
        ));
    }
    Ok(result)
}

fn validate_procedural_raise_arity(statements: &[ast::PlPgSqlStatement]) -> Result<()> {
    for statement in statements {
        match statement {
            ast::PlPgSqlStatement::If {
                branches,
                else_statements,
            } => {
                for branch in branches {
                    validate_procedural_raise_arity(&branch.statements)?;
                }
                if let Some(statements) = else_statements {
                    validate_procedural_raise_arity(statements)?;
                }
            }
            ast::PlPgSqlStatement::RaiseException {
                format, arguments, ..
            } => {
                let format = format.clone().into_string().ok_or_else(|| {
                    PgError::create(SqlState::SyntaxError, "RAISE format must be a string")
                })?;
                format_procedural_exception(&format, &vec![Value::Null; arguments.len()])?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_procedural_targets(
    statements: &[ast::PlPgSqlStatement],
    locals: &BTreeSet<String>,
) -> Result<()> {
    for statement in statements {
        match statement {
            ast::PlPgSqlStatement::Assignment { target, .. } => {
                let [ast::ObjectNamePart::Identifier(identifier)] = target.0.as_slice() else {
                    return reject_unsupported("DO assignment target is not implemented");
                };
                let name = executor::normalize_identifier(identifier);
                if !locals.contains(&name) {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        format!("variable {name:?} does not exist"),
                    ));
                }
            }
            ast::PlPgSqlStatement::GetRowCount { target } => {
                let name = executor::normalize_identifier(target);
                if !locals.contains(&name) {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        format!("variable {name:?} does not exist"),
                    ));
                }
            }
            ast::PlPgSqlStatement::Sql(statement) => {
                if let ast::Statement::Query(query) = statement.as_ref()
                    && let ast::SetExpr::Select(select) = query.body.as_ref()
                    && let Some(into) = &select.into
                {
                    for target in &into.targets {
                        let ast::Expr::Identifier(identifier) = target else {
                            return reject_unsupported("SELECT INTO target is not implemented");
                        };
                        let name = executor::normalize_identifier(identifier);
                        if !locals.contains(&name) {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                format!("variable {name:?} does not exist"),
                            ));
                        }
                    }
                }
            }
            ast::PlPgSqlStatement::If {
                branches,
                else_statements,
            } => {
                for branch in branches {
                    validate_procedural_targets(&branch.statements, locals)?;
                }
                if let Some(statements) = else_statements {
                    validate_procedural_targets(statements, locals)?;
                }
            }
            ast::PlPgSqlStatement::Return(_) => {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "cannot return a value from an anonymous block",
                ));
            }
            ast::PlPgSqlStatement::RaiseException { .. } => {}
        }
    }
    Ok(())
}

fn does_procedural_query_return_rows(expression: &ast::SetExpr) -> bool {
    match expression {
        ast::SetExpr::Insert(statement)
        | ast::SetExpr::Update(statement)
        | ast::SetExpr::Delete(statement) => does_procedural_statement_return_rows(statement),
        ast::SetExpr::Query(query) => does_procedural_query_return_rows(&query.body),
        _ => true,
    }
}

fn does_procedural_statement_return_rows(statement: &ast::Statement) -> bool {
    match statement {
        ast::Statement::Query(query) => does_procedural_query_return_rows(&query.body),
        ast::Statement::Insert(insert) => insert.returning.is_some(),
        ast::Statement::Update(update) => update.returning.is_some(),
        ast::Statement::Delete(delete) => delete.returning.is_some(),
        _ => false,
    }
}

impl Session {
    fn substitute_scoped_procedural_locals(
        &self,
        statement: &mut ast::Statement,
        locals: &BTreeMap<String, ProceduralLocal>,
    ) -> Result<()> {
        let scope = executor::create_value_scope(
            locals
                .iter()
                .map(|(name, local)| (name.clone(), local.data_type)),
        );
        let row = locals
            .values()
            .map(|local| local.value.clone())
            .collect::<Vec<_>>();
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let transaction = match self.transaction {
            Some(SessionTransactionState::Active(transaction)) => transaction,
            _ => unreachable!("procedural SQL executes in an active transaction"),
        };
        let snapshot = transaction
            .snapshot
            .unwrap_or_else(|| Snapshot::create(&state.transactions))
            .use_command(crate::txn::CommandId(transaction.next_command_id));
        state.load_catalog(
            Some(transaction.xid),
            snapshot,
            Some(self.temporary_schema_id),
        );
        executor::substitute_procedural_references(&state, statement, &scope, &row)
    }

    fn evaluate_procedural_expression(
        &mut self,
        expression: &ast::Expr,
        locals: &BTreeMap<String, ProceduralLocal>,
        procedural: DoBlockContext,
    ) -> Result<Value> {
        let mut statements = parser::parse(&format!("SELECT {expression}"))?;
        let mut statement = statements
            .pop()
            .expect("generated expression query contains one statement");
        assert!(
            statements.is_empty(),
            "generated expression query is singular"
        );
        self.substitute_scoped_procedural_locals(&mut statement, locals)?;
        let StatementResult::Query(query) =
            self.execute_statement(&statement, None, None, Some(procedural))?
        else {
            unreachable!("generated expression query returns rows")
        };
        Ok(query.rows[0][0].clone())
    }

    fn coerce_procedural_expression(
        &mut self,
        expression: &ast::Expr,
        target: PgType,
        locals: &BTreeMap<String, ProceduralLocal>,
        procedural: DoBlockContext,
    ) -> Result<Value> {
        if let Some(text) = executor::extract_unknown_string_literal(expression) {
            return coercion::coerce_unknown(text, target, CastContext::Assignment);
        }
        let value = self.evaluate_procedural_expression(expression, locals, procedural)?;
        let Some(source) = value.get_base_type() else {
            return Ok(Value::Null);
        };
        executor::coerce_procedural_value(value, source, target)
    }

    fn execute_procedural_sql(
        &mut self,
        statement: &ast::Statement,
        locals: &mut BTreeMap<String, ProceduralLocal>,
        row_count: &mut u64,
        procedural: DoBlockContext,
    ) -> Result<()> {
        let mut statement = statement.clone();
        let into = match &mut statement {
            ast::Statement::Query(query) => match query.body.as_mut() {
                ast::SetExpr::Select(select) => select.into.take(),
                _ => None,
            },
            _ => None,
        };
        let query_with_ctes =
            matches!(&statement, ast::Statement::Query(query) if query.with.is_some());
        let uses_scoped_substitution = !query_with_ctes;
        if into.is_none() && does_procedural_statement_return_rows(&statement) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "query has no destination for result data",
            ));
        }
        if uses_scoped_substitution {
            self.substitute_scoped_procedural_locals(&mut statement, locals)?;
        } else if query_with_ctes {
            let (mut expanded, mut mutations) = {
                let state = self.db.state.lock().expect("database mutex is poisoned");
                executor::expand_ctes_for_analysis(&statement, &state)?
            };
            self.substitute_scoped_procedural_locals(expanded.to_mut(), locals)?;
            for mutation in &mut mutations {
                self.substitute_scoped_procedural_locals(mutation, locals)?;
            }
            substitute_procedural_statement_locals(&mut statement, locals);
        }
        if into.is_some() {
            let ast::Statement::Query(query) = &mut statement else {
                unreachable!("SELECT INTO is represented by a query")
            };
            let current_limit = match &query.limit_clause {
                Some(ast::LimitClause::LimitOffset { limit, .. }) => limit.clone(),
                Some(ast::LimitClause::OffsetCommaLimit { .. }) => {
                    return reject_unsupported("SELECT INTO limit form is not implemented");
                }
                None => None,
            };
            let limit = match current_limit {
                Some(limit) => {
                    let value =
                        self.evaluate_procedural_expression(&limit, &BTreeMap::new(), procedural)?;
                    let value = match value.get_base_type() {
                        Some(source) => executor::coerce_procedural_value(
                            value,
                            source,
                            PgType::create(BaseType::Int8),
                        )?,
                        None => Value::Null,
                    };
                    match value {
                        Value::Int2(value) => Value::Int2(value.min(1)),
                        Value::Int4(value) => Value::Int4(value.min(1)),
                        Value::Int8(value) => Value::Int8(value.min(1)),
                        Value::Null => Value::Int8(1),
                        _ => {
                            return Err(PgError::create(
                                SqlState::DatatypeMismatch,
                                "LIMIT must be an integer",
                            ));
                        }
                    }
                }
                None => Value::Int8(1),
            };
            match &mut query.limit_clause {
                Some(ast::LimitClause::LimitOffset { limit: target, .. }) => {
                    *target = Some(analyzer::create_typed_literal(
                        limit.clone(),
                        PgType::create(
                            limit
                                .get_base_type()
                                .expect("SELECT INTO limit is a typed integer"),
                        ),
                    ));
                }
                None => {
                    query.limit_clause = Some(ast::LimitClause::LimitOffset {
                        limit: Some(analyzer::create_typed_literal(
                            Value::Int8(1),
                            PgType::create(BaseType::Int8),
                        )),
                        offset: None,
                        limit_by: Vec::new(),
                    });
                }
                Some(ast::LimitClause::OffsetCommaLimit { .. }) => unreachable!(),
            }
        }
        let result = self.execute_statement(&statement, None, None, Some(procedural))?;
        let Some(into) = into else {
            *row_count = match &result {
                StatementResult::Affected(affected) => *affected,
                StatementResult::Query(query) => query.rows.len() as u64,
            };
            return Ok(());
        };
        if into.temporary || into.unlogged || into.table {
            return reject_unsupported("SELECT INTO table is not implemented in PL/pgSQL");
        }
        let StatementResult::Query(query) = result else {
            unreachable!("SELECT returns a query result")
        };
        *row_count = u64::from(!query.rows.is_empty());
        for (index, target) in into.targets.iter().enumerate() {
            let ast::Expr::Identifier(identifier) = target else {
                return reject_unsupported("SELECT INTO target is not implemented");
            };
            let name = executor::normalize_identifier(identifier);
            let local = locals.get_mut(&name).ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedColumn,
                    format!("variable {name:?} does not exist"),
                )
            })?;
            let value = query
                .rows
                .first()
                .and_then(|row| row.get(index))
                .cloned()
                .unwrap_or(Value::Null);
            local.value = if value.is_null() {
                Value::Null
            } else {
                let source = BaseType::resolve_oid(
                    query
                        .columns
                        .get(index)
                        .expect("a non-NULL SELECT INTO value has column metadata")
                        .type_oid,
                )
                .expect("query results use supported PostgreSQL types");
                executor::coerce_procedural_value(value, source, local.data_type)?
            };
        }
        Ok(())
    }

    fn execute_procedural_statements(
        &mut self,
        statements: &[ast::PlPgSqlStatement],
        locals: &mut BTreeMap<String, ProceduralLocal>,
        row_count: &mut u64,
        procedural: DoBlockContext,
    ) -> Result<()> {
        for statement in statements {
            match statement {
                ast::PlPgSqlStatement::Sql(statement) => {
                    self.execute_procedural_sql(statement, locals, row_count, procedural)?;
                }
                ast::PlPgSqlStatement::GetRowCount { target } => {
                    let name = executor::normalize_identifier(target);
                    let local = locals.get_mut(&name).ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("variable {name:?} does not exist"),
                        )
                    })?;
                    local.value = executor::coerce_procedural_value(
                        Value::Int8(*row_count as i64),
                        BaseType::Int8,
                        local.data_type,
                    )?;
                }
                ast::PlPgSqlStatement::If {
                    branches,
                    else_statements,
                } => {
                    let mut selected = None;
                    for branch in branches {
                        match self.evaluate_procedural_expression(
                            &branch.condition,
                            locals,
                            procedural,
                        )? {
                            Value::Bool(true) => {
                                selected = Some(branch.statements.as_slice());
                                break;
                            }
                            Value::Bool(false) | Value::Null => {}
                            _ => {
                                return Err(PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "IF condition must be type boolean",
                                ));
                            }
                        }
                    }
                    if let Some(statements) = selected.or(else_statements.as_deref()) {
                        self.execute_procedural_statements(
                            statements, locals, row_count, procedural,
                        )?;
                    }
                }
                ast::PlPgSqlStatement::RaiseException {
                    format,
                    arguments,
                    hint,
                } => {
                    let format = format.clone().into_string().ok_or_else(|| {
                        PgError::create(SqlState::SyntaxError, "RAISE format must be a string")
                    })?;
                    let arguments = arguments
                        .iter()
                        .map(|argument| {
                            self.evaluate_procedural_expression(argument, locals, procedural)
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let message = format_procedural_exception(&format, &arguments)?;
                    let mut error = PgError::create(SqlState::RaiseException, message);
                    if let Some(hint) = hint {
                        let hint = self.evaluate_procedural_expression(hint, locals, procedural)?;
                        if hint.is_null() {
                            return self.abort_with_error(PgError::create(
                                SqlState::NullValueNotAllowed,
                                "RAISE statement option cannot be null",
                            ));
                        }
                        error.hint = Some(hint.format_postgres_text());
                    }
                    return self.abort_with_error(error);
                }
                ast::PlPgSqlStatement::Assignment { target, value } => {
                    let [ast::ObjectNamePart::Identifier(identifier)] = target.0.as_slice() else {
                        return reject_unsupported("DO assignment target is not implemented");
                    };
                    let name = executor::normalize_identifier(identifier);
                    let target_type = locals
                        .get(&name)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedColumn,
                                format!("variable {name:?} does not exist"),
                            )
                        })?
                        .data_type;
                    let value =
                        self.coerce_procedural_expression(value, target_type, locals, procedural)?;
                    locals.get_mut(&name).expect("required local exists").value = value;
                }
                ast::PlPgSqlStatement::Return(_) => {
                    return reject_unsupported("DO statement is not implemented");
                }
            }
        }
        Ok(())
    }

    pub(super) fn execute_do_block(
        &mut self,
        statement: &ast::DoStatement,
        procedural: DoBlockContext,
    ) -> Result<StatementResult> {
        if let Some(language) = &statement.language
            && !language.value.eq_ignore_ascii_case("plpgsql")
        {
            return self.abort_with_error(if language.value.eq_ignore_ascii_case("sql") {
                PgError::create(
                    SqlState::FeatureNotSupported,
                    "language does not support inline code execution",
                )
            } else {
                PgError::create(
                    SqlState::UndefinedObject,
                    format!("language {:?} does not exist", language.value),
                )
            });
        }
        let body = statement.body.clone().into_string().ok_or_else(|| {
            PgError::create(SqlState::SyntaxError, "DO body must be a string literal")
        })?;
        let mut parser = sqlparser::parser::Parser::new(&sqlparser::dialect::PostgreSqlDialect {})
            .try_with_sql(&body)
            .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))?;
        let block = parser
            .parse_plpgsql()
            .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))?;
        validate_procedural_raise_arity(&block.statements)?;
        let local_names = block
            .declarations
            .iter()
            .map(|declaration| executor::normalize_identifier(&declaration.name))
            .collect::<BTreeSet<_>>();
        validate_procedural_targets(&block.statements, &local_names)?;
        let mut locals = BTreeMap::new();
        for declaration in block.declarations {
            let data_type = coercion::convert_ast_data_type(&declaration.data_type)?;
            if !matches!(data_type.base, BaseType::Int8 | BaseType::Text) {
                return reject_unsupported("DO variable type is not implemented");
            }
            let name = executor::normalize_identifier(&declaration.name);
            if locals.contains_key(&name) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    format!("duplicate declaration of variable {name:?}"),
                ));
            }
            let value = match declaration.initializer {
                Some(expression) => {
                    self.coerce_procedural_expression(&expression, data_type, &locals, procedural)?
                }
                None => Value::Null,
            };
            locals.insert(name, ProceduralLocal { data_type, value });
        }
        let mut row_count = 0;
        self.execute_procedural_statements(
            &block.statements,
            &mut locals,
            &mut row_count,
            procedural,
        )?;
        Ok(StatementResult::Affected(0))
    }
}
