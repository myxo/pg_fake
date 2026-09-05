use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use super::*;
use crate::catalog::{FunctionSchema, TriggerSchema};

fn extract_function_body(create: &ast::CreateFunction) -> Result<String> {
    let Some(ast::CreateFunctionBody::AsBeforeOptions {
        body: ast::Expr::Value(body),
        link_symbol: None,
    }) = &create.function_body
    else {
        return Err(PgError::create(
            SqlState::InvalidFunctionDefinition,
            "trigger function body must be a string literal",
        ));
    };
    body.clone().into_string().ok_or_else(|| {
        PgError::create(
            SqlState::InvalidFunctionDefinition,
            "trigger function body must be a string literal",
        )
    })
}

fn validate_trigger_statements(statements: &[ast::PlPgSqlStatement]) -> Result<()> {
    for statement in statements {
        match statement {
            ast::PlPgSqlStatement::Assignment { target, .. }
                if target.0.len() == 2
                    && target.0[0]
                        .as_ident()
                        .is_some_and(|identifier| normalize_identifier(identifier) == "new") => {}
            ast::PlPgSqlStatement::If {
                branches,
                else_statements,
            } => {
                for branch in branches {
                    validate_trigger_statements(&branch.statements)?;
                }
                if let Some(statements) = else_statements {
                    validate_trigger_statements(statements)?;
                }
            }
            ast::PlPgSqlStatement::Return(ast::Expr::Identifier(identifier))
                if normalize_identifier(identifier) == "new" => {}
            ast::PlPgSqlStatement::Return(ast::Expr::Value(value))
                if matches!(value.value, ast::Value::Null) => {}
            _ => return reject_unsupported("trigger function statement is not implemented"),
        }
    }
    Ok(())
}

pub(super) fn execute_create_function(
    state: &mut DatabaseState,
    create: &ast::CreateFunction,
) -> Result<StatementResult> {
    if create.or_alter
        || create.temporary
        || create.if_not_exists
        || !matches!(create.args.as_deref(), Some([]))
        || !matches!(
            create.return_type,
            Some(ast::FunctionReturnType::DataType(ast::DataType::Trigger))
        )
        || !create
            .language
            .as_ref()
            .is_some_and(|language| language.value.eq_ignore_ascii_case("plpgsql"))
        || create.using.is_some()
        || create.behavior.is_some()
        || create.called_on_null.is_some()
        || create.parallel.is_some()
        || create.security.is_some()
        || !create.set_params.is_empty()
        || create.determinism_specifier.is_some()
        || create.options.is_some()
        || create.remote_connection.is_some()
    {
        return reject_unsupported("function definition is not implemented");
    }
    let body = extract_function_body(create)?;
    let mut parser = Parser::new(&PostgreSqlDialect {})
        .try_with_sql(&body)
        .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))?;
    let body = parser
        .parse_plpgsql()
        .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))?;
    if !body.declarations.is_empty() {
        return reject_unsupported("trigger function declarations are not implemented");
    }
    validate_trigger_statements(&body.statements)?;
    let name = normalize_relation_name(&create.name)?;
    let name = state.catalog.resolve_function_name(&name)?;
    state
        .catalog
        .create_or_replace_function(name, create.clone(), body, create.or_replace)?;
    Ok(StatementResult::Affected(0))
}

pub(super) fn execute_drop_function(
    state: &mut DatabaseState,
    drop: &ast::DropFunction,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<StatementResult> {
    let cascade = matches!(drop.drop_behavior, Some(ast::DropBehavior::Cascade));
    for description in &drop.func_desc {
        if !matches!(description.args.as_deref(), Some([])) {
            return reject_unsupported("function arguments are not implemented");
        }
        let name = normalize_relation_name(&description.name)?;
        let function = match state.catalog.require_named_function(&name) {
            Ok(function) => function.clone(),
            Err(error) if drop.if_exists && error.sqlstate == SqlState::UndefinedFunction => {
                continue;
            }
            Err(error) => return Err(error),
        };
        let dependent_tables = state
            .catalog
            .iterate_tables()
            .filter(|table| {
                table
                    .triggers
                    .iter()
                    .any(|trigger| trigger.function_id == function.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let current_dependencies = dependent_tables
            .iter()
            .map(|table| table.id)
            .collect::<BTreeSet<_>>();
        let hidden_dependencies = state
            .catalog_history
            .find_trigger_dependencies(function.id, xid, *snapshot, &state.transactions)
            .into_iter()
            .any(|table| !current_dependencies.contains(&table));
        if hidden_dependencies {
            if cascade {
                return reject_unsupported(
                    "dropping triggers owned by another temporary session is not implemented",
                );
            }
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop function {}() because other objects depend on it",
                    function.name
                ),
            ));
        }
        if !dependent_tables.is_empty() && !cascade {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop function {}() because other objects depend on it",
                    function.name
                ),
            ));
        }
        for mut table in dependent_tables {
            table
                .triggers
                .retain(|trigger| trigger.function_id != function.id);
            state.catalog.replace_table(table)?;
        }
        state.catalog.drop_function(&function);
    }
    Ok(StatementResult::Affected(0))
}

pub(super) fn execute_create_trigger(
    state: &mut DatabaseState,
    create: &ast::CreateTrigger,
) -> Result<StatementResult> {
    if create
        .events
        .iter()
        .enumerate()
        .any(|(index, event)| create.events[..index].contains(event))
    {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "trigger event specified more than once",
        ));
    }
    if create.or_alter
        || create.temporary
        || create.or_replace
        || create.is_constraint
        || create.period != Some(ast::TriggerPeriod::Before)
        || !create.period_before_table
        || create.events.is_empty()
        || create.events.len() > 2
        || create.events.iter().any(|event| match event {
            ast::TriggerEvent::Insert => false,
            ast::TriggerEvent::Update(columns) => !columns.is_empty(),
            ast::TriggerEvent::Delete | ast::TriggerEvent::Truncate => true,
        })
        || !matches!(
            create.trigger_object,
            Some(ast::TriggerObjectKind::ForEach(ast::TriggerObject::Row))
        )
        || create.referenced_table_name.is_some()
        || !create.referencing.is_empty()
        || create.condition.is_some()
        || create.statements.is_some()
        || create.characteristics.is_some()
    {
        return reject_unsupported("trigger definition is not implemented");
    }
    let Some(ast::TriggerExecBody {
        exec_type: ast::TriggerExecBodyType::Function,
        func_desc,
    }) = &create.exec_body
    else {
        return reject_unsupported("trigger execution body is not implemented");
    };
    if !matches!(func_desc.args.as_deref(), Some([])) {
        return reject_unsupported("trigger function arguments are not implemented");
    }
    let function_name = normalize_relation_name(&func_desc.name)?;
    let function = state
        .catalog
        .require_named_function(&function_name)?
        .clone();
    let name = normalize_unqualified_object_name(&create.name)?;
    let table_name = normalize_relation_name(&create.table_name)?;
    let mut table = state.catalog.require_named_table(&table_name)?.clone();
    if state.catalog.get_schema_name(function.schema_id) == crate::catalog::TEMP_SCHEMA
        && state.catalog.get_schema_name(table.schema_id) != crate::catalog::TEMP_SCHEMA
    {
        return reject_unsupported("permanent triggers cannot use temporary functions");
    }
    if state.catalog.get_schema_name(function.schema_id) != crate::catalog::TEMP_SCHEMA
        && state.catalog.get_schema_name(table.schema_id) == crate::catalog::TEMP_SCHEMA
    {
        return reject_unsupported("temporary triggers cannot use permanent functions");
    }
    if table.triggers.iter().any(|trigger| trigger.name == name) {
        return Err(PgError::create(
            SqlState::DuplicateObject,
            format!(
                "trigger {name:?} for relation {:?} already exists",
                table.name
            ),
        ));
    }
    table.triggers.push(TriggerSchema {
        id: state.catalog.allocate_trigger_id(),
        name,
        function_id: function.id,
        definition: create.clone(),
    });
    table
        .triggers
        .sort_by(|left, right| left.name.cmp(&right.name));
    state.catalog.replace_table(table)?;
    Ok(StatementResult::Affected(0))
}

pub(super) fn execute_drop_trigger(
    state: &mut DatabaseState,
    drop: &ast::DropTrigger,
) -> Result<StatementResult> {
    let Some(table_name) = &drop.table_name else {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "DROP TRIGGER requires ON table",
        ));
    };
    let table_name = normalize_relation_name(table_name)?;
    let mut table = match state.catalog.require_named_table(&table_name) {
        Ok(table) => table.clone(),
        Err(error) if drop.if_exists && error.sqlstate == SqlState::UndefinedTable => {
            return Ok(StatementResult::Affected(0));
        }
        Err(error) => return Err(error),
    };
    let name = normalize_unqualified_object_name(&drop.trigger_name)?;
    let Some(index) = table
        .triggers
        .iter()
        .position(|trigger| trigger.name == name)
    else {
        if drop.if_exists {
            return Ok(StatementResult::Affected(0));
        }
        return Err(PgError::create(
            SqlState::UndefinedObject,
            format!("trigger {name:?} for table {:?} does not exist", table.name),
        ));
    };
    table.triggers.remove(index);
    state.catalog.replace_table(table)?;
    Ok(StatementResult::Affected(0))
}

pub(super) fn require_function<'a>(
    state: &'a DatabaseState,
    trigger: &TriggerSchema,
) -> Result<&'a FunctionSchema> {
    state.catalog.require_function_by_id(trigger.function_id)
}

#[derive(Clone, Copy)]
pub(super) enum TriggerEventKind {
    Insert,
    Update,
}

enum TriggerReturn {
    Continue,
    Skip,
}

fn create_new_scope(schema: &TableSchema) -> BoundScope {
    let mut scope = bind_target_scope(schema, None);
    for column in &mut scope.columns {
        column.qualifier = "new".into();
        column.unqualified = false;
    }
    scope
}

fn evaluate_trigger_expression(
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    evaluate(expression, RowScope::Bound(scope), row, context)
}

pub(crate) fn coerce_procedural_value(
    value: Value,
    source: BaseType,
    target: PgType,
) -> Result<Value> {
    if coercion::can_cast(source, target.base, CastContext::Assignment) {
        coercion::coerce(value, source, target, CastContext::Assignment)
    } else {
        coercion::coerce_unknown(
            &value.format_postgres_text(),
            target,
            CastContext::Assignment,
        )
    }
}

fn execute_trigger_statements(
    statements: &[ast::PlPgSqlStatement],
    scope: &BoundScope,
    row: &mut Vec<Value>,
    context: &StatementExecutionContext,
) -> Result<Option<TriggerReturn>> {
    for statement in statements {
        match statement {
            ast::PlPgSqlStatement::Assignment { target, value } => {
                let identifiers = target
                    .0
                    .iter()
                    .map(|part| {
                        part.as_ident().cloned().ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedColumn,
                                "trigger assignment target is not a column",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let (index, target_type) = RowScope::Bound(scope).resolve_column(&identifiers)?;
                row[index] = if let Some(text) = extract_unknown_string_literal(value) {
                    coercion::coerce_unknown(text, target_type, CastContext::Assignment)?
                } else {
                    coerce_procedural_value(
                        evaluate_trigger_expression(value, scope, row, context)?,
                        infer_expression_type(value, RowScope::Bound(scope))?,
                        target_type,
                    )?
                };
            }
            ast::PlPgSqlStatement::If {
                branches,
                else_statements,
            } => {
                let mut selected = None;
                for branch in branches {
                    match evaluate_trigger_expression(&branch.condition, scope, row, context)? {
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
                let selected = selected.or(else_statements.as_deref());
                if let Some(statements) = selected
                    && let Some(result) =
                        execute_trigger_statements(statements, scope, row, context)?
                {
                    return Ok(Some(result));
                }
            }
            ast::PlPgSqlStatement::Return(ast::Expr::Identifier(identifier))
                if normalize_identifier(identifier) == "new" =>
            {
                return Ok(Some(TriggerReturn::Continue));
            }
            ast::PlPgSqlStatement::Return(ast::Expr::Value(value))
                if matches!(value.value, ast::Value::Null) =>
            {
                return Ok(Some(TriggerReturn::Skip));
            }
            _ => return reject_unsupported("trigger function statement is not implemented"),
        }
    }
    Ok(None)
}

pub(super) fn execute_before_row_triggers(
    state: &DatabaseState,
    schema: &TableSchema,
    event: TriggerEventKind,
    mut row: Vec<Value>,
    context: &StatementExecutionContext,
) -> Result<Option<Vec<Value>>> {
    let scope = create_new_scope(schema);
    for trigger in &schema.triggers {
        let fires = trigger.definition.events.iter().any(|configured| {
            matches!(
                (event, configured),
                (TriggerEventKind::Insert, ast::TriggerEvent::Insert)
                    | (TriggerEventKind::Update, ast::TriggerEvent::Update(_))
            )
        });
        if !fires {
            continue;
        }
        let function = require_function(state, trigger)?;
        match execute_trigger_statements(&function.body.statements, &scope, &mut row, context)? {
            Some(TriggerReturn::Continue) => {}
            Some(TriggerReturn::Skip) => return Ok(None),
            None => {
                return Err(PgError::create(
                    SqlState::FunctionExecutedNoReturnStatement,
                    format!(
                        "control reached end of trigger function {}()",
                        function.name
                    ),
                ));
            }
        }
    }
    Ok(Some(row))
}
