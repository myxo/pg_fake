use sqlparser::ast;

use crate::{
    QueryResult, StatementResult,
    error::{PgError, Result, SqlState},
    executor::{
        DatabaseState, StatementExecutionContext,
        expressions::evaluate,
        normalize_relation_name,
        outer_references::{
            NameConflictPolicy, collect_outer_reference_slots, substitute_outer_references,
        },
        query::execute_query,
        resolve_insert_table_name,
        scope::{
            BoundScope, RowScope, bind_from_scope, bind_query_scope, bind_target_scope,
            combine_bound_scopes,
        },
    },
    txn::{Snapshot, Xid},
    value::{BaseType, PgType, Value},
};

struct SubqueryDetector {
    found: bool,
}

impl ast::Visitor for SubqueryDetector {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.found = true;
        std::ops::ControlFlow::Break(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn contains_subquery(expression: &ast::Expr) -> bool {
    let mut detector = SubqueryDetector { found: false };
    let _ = ast::Visit::visit(expression, &mut detector);
    detector.found
}

struct SubqueryMaterializer<'a> {
    state: &'a DatabaseState,
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a StatementExecutionContext,
    error: Option<PgError>,
    defer_unresolved: bool,
    scopes: Vec<BoundScope>,
}

impl SubqueryMaterializer<'_> {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn execute(&self, query: &ast::Query) -> Result<QueryResult> {
        if let Some(result) = self.context.get_prepared_subquery_result(query) {
            return Ok(result);
        }
        let original = query.clone();
        let query = materialize_uncorrelated_subqueries(
            self.state,
            &ast::Statement::Query(Box::new(query.clone())),
            self.xid,
            self.snapshot,
            self.context,
        )?;
        let ast::Statement::Query(query) = query else {
            unreachable!("subquery statement remains a query");
        };
        let StatementResult::Query(result) =
            execute_query(self.state, &query, self.xid, self.snapshot, self.context)?
        else {
            unreachable!("subquery execution returns query rows");
        };
        if self.context.prepares_subquery_results() {
            self.context
                .set_prepared_subquery_result(&original, result.clone());
        }
        Ok(result)
    }
}

impl ast::VisitorMut for SubqueryMaterializer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let scope = match query.body.as_ref() {
            ast::SetExpr::Select(select) => bind_query_scope(&self.state.catalog, select),
            _ => Ok(BoundScope {
                columns: Vec::new(),
            }),
        }
        .unwrap_or(BoundScope {
            columns: Vec::new(),
        });
        self.scopes.push(scope);
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expr: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if !matches!(
            expr,
            ast::Expr::AnyOp { right, .. } | ast::Expr::AllOp { right, .. }
                if matches!(right.as_ref(), ast::Expr::Subquery(_))
        ) && !matches!(
            expr,
            ast::Expr::Subquery(_) | ast::Expr::Exists { .. } | ast::Expr::InSubquery { .. }
        ) {
            return std::ops::ControlFlow::Continue(());
        }
        let original = expr.clone();
        let correlation_candidate = original.clone();
        let result = (|| match original {
            ast::Expr::AnyOp {
                left,
                compare_op,
                right,
                is_some,
            } => {
                let ast::Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(ast::Expr::AnyOp {
                    left,
                    compare_op,
                    right: Box::new(ast::Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| {
                                crate::analyzer::create_typed_literal(row[0].clone(), data_type)
                            })
                            .collect(),
                    )),
                    is_some,
                }))
            }
            ast::Expr::AllOp {
                left,
                compare_op,
                right,
            } => {
                let ast::Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(ast::Expr::AllOp {
                    left,
                    compare_op,
                    right: Box::new(ast::Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| {
                                crate::analyzer::create_typed_literal(row[0].clone(), data_type)
                            })
                            .collect(),
                    )),
                }))
            }
            ast::Expr::Subquery(query) => {
                let result = self.execute(&query)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery must return only one column",
                    ));
                }
                if result.rows.len() > 1 {
                    return Err(PgError::create(
                        SqlState::CardinalityViolation,
                        "more than one row returned by a subquery used as an expression",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(crate::analyzer::create_typed_literal(
                    result
                        .rows
                        .into_iter()
                        .next()
                        .map(|row| row[0].clone())
                        .unwrap_or(Value::Null),
                    data_type,
                )))
            }
            ast::Expr::Exists { subquery, negated } => {
                Ok(Some(crate::analyzer::create_typed_literal(
                    Value::Bool(self.execute(&subquery)?.rows.is_empty() == negated),
                    PgType::create(BaseType::Bool),
                )))
            }
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let result = self.execute(&subquery)?;
                let left_width = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if result.columns.len() != left_width {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let types = result
                    .columns
                    .iter()
                    .map(|column| {
                        PgType::create_with_typmod(
                            BaseType::resolve_oid(column.type_oid)
                                .expect("query result type OID is supported"),
                            column.typmod,
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(Some(ast::Expr::InList {
                    expr,
                    list: result
                        .rows
                        .into_iter()
                        .map(|row| {
                            let fields = row
                                .into_iter()
                                .zip(&types)
                                .map(|(value, data_type)| {
                                    crate::analyzer::create_typed_literal(value, *data_type)
                                })
                                .collect::<Vec<_>>();
                            if fields.len() == 1 {
                                fields.into_iter().next().expect("row has one field")
                            } else {
                                ast::Expr::Tuple(fields)
                            }
                        })
                        .collect(),
                    negated,
                }))
            }
            _ => Ok(None),
        })();
        match result {
            Ok(Some(value)) => *expr = value,
            Ok(None) => {}
            Err(error)
                if self.defer_unresolved
                    && matches!(
                        error.sqlstate,
                        SqlState::UndefinedColumn | SqlState::UndefinedTable
                    ) =>
            {
                match self.scopes.last().map(|outer| {
                    collect_outer_reference_slots(
                        &self.state.catalog,
                        &correlation_candidate,
                        outer,
                    )
                    .map(|slots| !slots.is_empty())
                }) {
                    Some(Ok(true)) => {}
                    Some(Err(scope_error)) => self.error = Some(scope_error),
                    _ => self.error = Some(error),
                }
            }
            Err(error) => self.error = Some(error),
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn materialize_uncorrelated_subqueries(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<ast::Statement> {
    let scope = match statement {
        ast::Statement::Insert(insert) => {
            let schema = state
                .catalog
                .require_named_table(&resolve_insert_table_name(&insert.table)?)?;
            Some(bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            ))
        }
        ast::Statement::Update(update) => {
            let ast::TableFactor::Table { name, alias, .. } = &update.table.relation else {
                return Ok(statement.clone());
            };
            let schema = state
                .catalog
                .require_named_table(&normalize_relation_name(name)?)?;
            let from = match &update.from {
                None => &[][..],
                Some(ast::UpdateTableFromKind::AfterSet(from)) => from.as_slice(),
                Some(ast::UpdateTableFromKind::BeforeSet(_)) => &[][..],
            };
            Some(combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, from)?,
            ))
        }
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(statement.clone());
            };
            let Some(ast::TableWithJoins {
                relation: ast::TableFactor::Table { name, alias, .. },
                ..
            }) = from.first()
            else {
                return Ok(statement.clone());
            };
            let schema = state
                .catalog
                .require_named_table(&normalize_relation_name(name)?)?;
            Some(combine_bound_scopes(
                bind_target_scope(schema, alias.as_ref().map(|alias| &alias.name)),
                bind_from_scope(&state.catalog, delete.using.as_deref().unwrap_or_default())?,
            ))
        }
        _ => None,
    };
    let mut statement = statement.clone();
    materialize_subqueries(
        state,
        &mut statement,
        xid,
        snapshot,
        context,
        true,
        scope.into_iter().collect(),
    )?;
    Ok(statement)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn materialize_subqueries<V: ast::VisitMut>(
    state: &DatabaseState,
    value: &mut V,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    defer_unresolved: bool,
    scopes: Vec<BoundScope>,
) -> Result<()> {
    let mut materializer = SubqueryMaterializer {
        state,
        xid,
        snapshot,
        context,
        error: None,
        defer_unresolved,
        scopes,
    };
    let _ = value.visit(&mut materializer);
    if let Some(error) = materializer.error {
        return Err(error);
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_query_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Value> {
    if !contains_subquery(expression) {
        return evaluate(expression, RowScope::Bound(scope), row, context);
    }
    let mut expression = expression.clone();
    substitute_outer_references(
        &state.catalog,
        &mut expression,
        scope,
        row,
        Vec::new(),
        NameConflictPolicy::PreferInner,
    )?;
    materialize_subqueries(
        state,
        &mut expression,
        xid,
        snapshot,
        context,
        false,
        Vec::new(),
    )?;
    evaluate(&expression, RowScope::Bound(scope), row, context)
}
