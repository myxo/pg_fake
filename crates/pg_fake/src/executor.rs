use bigdecimal::ToPrimitive;

use crate::{
    api::{ColumnMeta, QueryResult, StatementResult},
    catalog::{Catalog, ColumnDef, ForeignKey, ForeignKeyAction, TableId, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    storage::{RowId, Table},
    txn::{
        RowLockKey, RowLockManager, RowLockMode, Snapshot, TransactionManager, TransactionStatus,
        WaitForGraph, Xid, visible_version,
    },
    value::{BaseType, PgType, Value},
};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, CastKind, ColumnOption, DateTimeField, Delete, Expr,
    FromTable, Function, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, Ident,
    LockType, ObjectType, ReferentialAction, SelectItem, SetExpr, Statement, TableConstraint,
    TableFactor, TableWithJoins, UnaryOperator, Value as AstValue,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

pub(crate) struct DatabaseState {
    pub(crate) catalog: Catalog,
    pub(crate) tables: BTreeMap<TableId, Table>,
    pub(crate) transactions: TransactionManager,
    pub(crate) row_locks: RowLockManager,
    pub(crate) wait_for: WaitForGraph,
}
pub(crate) struct RequiredRowLock {
    pub(crate) key: RowLockKey,
    pub(crate) mode: RowLockMode,
}

pub(crate) fn query_columns(
    state: &DatabaseState,
    statement: &Statement,
) -> Result<Vec<ColumnMeta>> {
    let Statement::Query(query) = statement else {
        return Ok(Vec::new());
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(Vec::new());
    };
    let Some(from) = select.from.first() else {
        return Ok(Vec::new());
    };
    let TableFactor::Table { name: table, .. } = &from.relation else {
        return Ok(Vec::new());
    };
    let schema = state.catalog.table(&name(table)?)?;
    projections_and_columns(&select.projection, schema).map(|(_, columns)| columns)
}
enum Projection<'a> {
    Column(usize),
    Expression(&'a Expr),
}
enum OrderKey<'a> {
    Output(usize),
    Expression(&'a Expr),
}
enum RowCountClause {
    Limit,
    Offset,
}
struct OrderSpec<'a> {
    key: OrderKey<'a>,
    ascending: bool,
    nulls_first: bool,
}
struct OrderedRow {
    values: Vec<Value>,
    keys: Vec<Value>,
}
impl DatabaseState {
    pub(crate) fn new() -> Self {
        DatabaseState {
            catalog: Catalog::new(),
            tables: BTreeMap::new(),
            transactions: TransactionManager::new(),
            row_locks: RowLockManager::new(),
            wait_for: WaitForGraph::new(),
        }
    }
}

pub(crate) fn required_row_locks(
    state: &DatabaseState,
    statement: &Statement,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<Vec<RequiredRowLock>> {
    if let Statement::Insert(insert) = statement {
        return required_insert_foreign_key_locks(state, insert, xid, snapshot);
    }
    let (schema, selection, mode) = match statement {
        Statement::Update {
            table, selection, ..
        } => {
            if !table.joins.is_empty() {
                return Ok(Vec::new());
            }
            let TableFactor::Table {
                name: table_name,
                args: None,
                ..
            } = &table.relation
            else {
                return Ok(Vec::new());
            };
            (
                state.catalog.table(&name(table_name)?)?,
                selection.as_ref(),
                RowLockMode::Update,
            )
        }
        Statement::Delete(delete) => {
            let FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(Vec::new());
            };
            if from.len() != 1 || !from[0].joins.is_empty() {
                return Ok(Vec::new());
            }
            let TableFactor::Table {
                name: table_name,
                args: None,
                ..
            } = &from[0].relation
            else {
                return Ok(Vec::new());
            };
            (
                state.catalog.table(&name(table_name)?)?,
                delete.selection.as_ref(),
                RowLockMode::Update,
            )
        }
        Statement::Query(query) => {
            let Some(mode) = select_lock_mode(query)? else {
                return Ok(Vec::new());
            };
            let SetExpr::Select(select) = query.body.as_ref() else {
                return Ok(Vec::new());
            };
            if select.from.len() != 1 || !select.from[0].joins.is_empty() {
                return Ok(Vec::new());
            }
            let TableFactor::Table {
                name: table_name,
                args: None,
                ..
            } = &select.from[0].relation
            else {
                return Ok(Vec::new());
            };
            (
                state.catalog.table(&name(table_name)?)?,
                select.selection.as_ref(),
                mode,
            )
        }
        _ => return Ok(Vec::new()),
    };
    if let Some(selection) = selection {
        let base = expression_type(selection, schema)?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Ok(Vec::new());
        }
    }
    state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .rows()
        .try_fold(Vec::new(), |mut locks, (row_id, chain)| {
            let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
                return Ok(locks);
            };
            if let Some(selection) = selection {
                match evaluate(selection, schema, &version.row)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(locks),
                    _ => return Ok(locks),
                }
            }
            if version.xmax.is_some_and(|xmax| {
                xmax != xid
                    && matches!(
                        state.transactions.status(xmax),
                        Some(TransactionStatus::Committed(commit_seq))
                            if commit_seq > snapshot.commit_seq
                    )
            }) {
                return Err(PgError::new(
                    SqlState::SerializationFailure,
                    "could not serialize access due to concurrent update",
                ));
            }
            locks.push(RequiredRowLock {
                key: RowLockKey {
                    table_id: schema.id,
                    row_id,
                },
                mode,
            });
            Ok(locks)
        })
}

fn required_insert_foreign_key_locks(
    state: &DatabaseState,
    insert: &sqlparser::ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<Vec<RequiredRowLock>> {
    let schema = state.catalog.table(&name(&insert.table_name)?)?;
    let Some(source) = &insert.source else {
        return Ok(Vec::new());
    };
    let SetExpr::Values(values) = source.body.as_ref() else {
        return Ok(Vec::new());
    };
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|column| {
                schema
                    .columns
                    .iter()
                    .position(|definition| definition.name == identifier_name(column))
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", column.value),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut locks = Vec::new();
    for expressions in &values.rows {
        if expressions.len() != column_indexes.len() {
            continue;
        }
        let mut row = schema
            .columns
            .iter()
            .map(column_default)
            .collect::<Result<Vec<_>>>()?;
        for (expression, index) in expressions.iter().zip(&column_indexes) {
            row[*index] = if default_expression(expression) {
                column_default(&schema.columns[*index])?
            } else {
                expression_value(
                    expression,
                    schema.columns[*index].data_type,
                    &constant_schema(),
                    &[],
                )?
            };
        }
        for constraint in &schema.constraints {
            let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
                continue;
            };
            let local = foreign_key_column_indexes(schema, &foreign_key.columns)?;
            let key = local
                .iter()
                .map(|index| row[*index].clone())
                .collect::<Vec<_>>();
            if key.iter().any(Value::is_null) {
                continue;
            }
            let foreign_schema = state.catalog.table(&foreign_key.foreign_table)?;
            let referred = if foreign_key.referred_columns.is_empty() {
                foreign_schema
                    .constraints
                    .iter()
                    .find_map(|constraint| match constraint {
                        crate::catalog::Constraint::PrimaryKey(columns) => Some(columns.clone()),
                        _ => None,
                    })
                    .expect("foreign key definition was validated")
            } else {
                foreign_key.referred_columns.clone()
            };
            let referred = foreign_key_column_indexes(foreign_schema, &referred)?;
            for (row_id, chain) in state
                .tables
                .get(&foreign_schema.id)
                .expect("catalog table must have storage")
                .rows()
            {
                let Some(version) = visible_version(chain, snapshot, xid, &state.transactions)
                else {
                    continue;
                };
                let foreign_key = referred
                    .iter()
                    .map(|index| version.row[*index].clone())
                    .collect::<Vec<_>>();
                if key_matches(&key, &foreign_key)? {
                    locks.push(RequiredRowLock {
                        key: RowLockKey {
                            table_id: foreign_schema.id,
                            row_id,
                        },
                        mode: RowLockMode::Share,
                    });
                }
            }
        }
    }
    Ok(locks)
}

pub(crate) fn dispatch(
    state: &mut DatabaseState,
    statement: &Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
) -> Result<StatementResult> {
    match statement {
        Statement::CreateTable(create) => {
            let table_name = name(&create.name)?;
            if create.if_not_exists && state.catalog.table(&table_name).is_ok() {
                return Ok(StatementResult::Affected(0));
            }
            let mut columns = Vec::new();
            let mut constraints = Vec::new();
            for column in &create.columns {
                let data_type = coercion::type_from_ast(&column.data_type)?;
                let mut nullable = true;
                let mut default = None;
                for option in &column.options {
                    match &option.option {
                        ColumnOption::Null => nullable = true,
                        ColumnOption::NotNull => nullable = false,
                        ColumnOption::Default(expr) => default = Some(expr.clone()),
                        ColumnOption::Unique { is_primary, .. } => {
                            let columns = vec![identifier_name(&column.name)];
                            constraints.push(if *is_primary {
                                crate::catalog::Constraint::PrimaryKey(columns)
                            } else {
                                crate::catalog::Constraint::Unique(columns)
                            });
                        }
                        ColumnOption::Check(expr) => constraints
                            .push(crate::catalog::Constraint::Check(Box::new(expr.clone()))),
                        ColumnOption::ForeignKey {
                            foreign_table,
                            referred_columns,
                            on_delete,
                            on_update,
                            characteristics,
                        } => {
                            let (name, match_full) = foreign_key_name(
                                option.name.as_ref(),
                                format!("{}_{}_fkey", table_name, identifier_name(&column.name)),
                            );
                            constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                                name,
                                columns: vec![identifier_name(&column.name)],
                                foreign_table: crate::executor::name(foreign_table)?,
                                referred_columns: referred_columns
                                    .iter()
                                    .map(identifier_name)
                                    .collect(),
                                on_delete: foreign_key_action(*on_delete),
                                on_update: foreign_key_action(*on_update),
                                deferrable: characteristics.is_some_and(|characteristics| {
                                    characteristics.deferrable.unwrap_or(false)
                                }),
                                initially_deferred: characteristics.is_some_and(
                                    |characteristics| {
                                        characteristics.initially
                                            == Some(sqlparser::ast::DeferrableInitial::Deferred)
                                    },
                                ),
                                match_full,
                            }))
                        }
                        option => {
                            return Err(PgError::new(
                                SqlState::FeatureNotSupported,
                                format!("column option is not implemented: {option}"),
                            ));
                        }
                    }
                }
                let column = ColumnDef {
                    name: identifier_name(&column.name),
                    data_type,
                    nullable,
                    default,
                };
                if column.default.is_some() {
                    column_default(&column)?;
                }
                columns.push(column);
            }
            for constraint in &create.constraints {
                match constraint {
                    TableConstraint::PrimaryKey { columns, .. } => {
                        constraints.push(crate::catalog::Constraint::PrimaryKey(
                            columns.iter().map(identifier_name).collect(),
                        ))
                    }
                    TableConstraint::Unique { columns, .. } => {
                        constraints.push(crate::catalog::Constraint::Unique(
                            columns.iter().map(identifier_name).collect(),
                        ))
                    }
                    TableConstraint::Check { expr, .. } => {
                        constraints.push(crate::catalog::Constraint::Check(expr.clone()))
                    }
                    TableConstraint::ForeignKey {
                        name: constraint_name,
                        columns: foreign_columns,
                        foreign_table,
                        referred_columns,
                        on_delete,
                        on_update,
                        characteristics,
                    } => {
                        let (name, match_full) = foreign_key_name(
                            constraint_name.as_ref(),
                            format!("{}_fkey", table_name),
                        );
                        constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                            name,
                            columns: foreign_columns.iter().map(identifier_name).collect(),
                            foreign_table: crate::executor::name(foreign_table)?,
                            referred_columns: referred_columns
                                .iter()
                                .map(identifier_name)
                                .collect(),
                            on_delete: foreign_key_action(*on_delete),
                            on_update: foreign_key_action(*on_update),
                            deferrable: characteristics.is_some_and(|characteristics| {
                                characteristics.deferrable.unwrap_or(false)
                            }),
                            initially_deferred: characteristics.is_some_and(|characteristics| {
                                characteristics.initially
                                    == Some(sqlparser::ast::DeferrableInitial::Deferred)
                            }),
                            match_full,
                        }))
                    }
                    constraint => {
                        return Err(PgError::new(
                            SqlState::FeatureNotSupported,
                            format!("table constraint is not implemented: {constraint}"),
                        ));
                    }
                }
            }
            for constraint in &constraints {
                let (constraint_columns, primary_key) = match constraint {
                    crate::catalog::Constraint::PrimaryKey(columns) => (columns, true),
                    crate::catalog::Constraint::Unique(columns) => (columns, false),
                    crate::catalog::Constraint::Check(_)
                    | crate::catalog::Constraint::ForeignKey(_) => continue,
                };
                for name in constraint_columns {
                    let column = columns
                        .iter_mut()
                        .find(|column| column.name == *name)
                        .ok_or_else(|| {
                            PgError::new(
                                SqlState::UndefinedColumn,
                                format!("column {name:?} does not exist"),
                            )
                        })?;
                    if primary_key {
                        column.nullable = false;
                    }
                }
            }
            validate_check_constraint_types(&TableSchema {
                id: TableId(0),
                name: table_name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
            })?;
            let proposed = TableSchema {
                id: TableId(0),
                name: table_name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
            };
            validate_foreign_key_definitions(&state.catalog, &proposed)?;
            let id = state
                .catalog
                .create_table(table_name.clone(), columns, constraints)?;
            let table = state
                .catalog
                .table(&table_name)
                .expect("created table must exist");
            state.tables.insert(id, Table::new(table.clone()));
            Ok(StatementResult::Affected(0))
        }
        Statement::Drop {
            object_type: ObjectType::Table,
            names,
            if_exists,
            ..
        } => {
            let mut affected = 0;
            for object in names {
                let table_name = name(object)?;
                match state.catalog.drop_table(&table_name) {
                    Ok(schema) => {
                        state.tables.remove(&schema.id);
                        affected += 1;
                    }
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedTable => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(StatementResult::Affected(affected))
        }
        Statement::Insert(insert) => insert_rows(
            state,
            insert,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        ),
        Statement::Update {
            table,
            assignments,
            from,
            selection,
            returning,
            or,
        } => {
            if from.is_some() || returning.is_some() || or.is_some() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "UPDATE feature is not implemented",
                ));
            }
            update_rows(
                state,
                table,
                assignments,
                selection.as_ref(),
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
            )
        }
        Statement::Delete(delete) => delete_rows(
            state,
            delete,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        ),
        Statement::Query(query) => select_rows(state, query, xid, snapshot),
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "statement is not implemented",
        )),
    }
}
pub(crate) fn name(name: &sqlparser::ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "schemas are not implemented",
        ));
    }
    Ok(identifier_name(&name.0[0]))
}

pub(crate) fn identifier_name(identifier: &Ident) -> String {
    if identifier.quote_style.is_some() {
        identifier.value.clone()
    } else {
        identifier.value.to_ascii_lowercase()
    }
}

fn insert_rows(
    state: &mut DatabaseState,
    insert: &sqlparser::ast::Insert,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
) -> Result<StatementResult> {
    let table_name = name(&insert.table_name)?;
    let schema = state.catalog.table(&table_name)?.clone();
    if insert.returning.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "INSERT RETURNING is not implemented",
        ));
    }
    let column_indexes = if insert.columns.is_empty() {
        (0..schema.columns.len()).collect::<Vec<_>>()
    } else {
        insert
            .columns
            .iter()
            .map(|name| {
                schema
                    .columns
                    .iter()
                    .position(|column| column.name == identifier_name(name))
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", name.value),
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?
    };
    let provided = column_indexes.iter().copied().collect::<BTreeSet<_>>();
    let build_row = |expressions: &[Expr]| -> Result<Vec<Value>> {
        if expressions.len() != column_indexes.len() {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "INSERT has wrong number of values",
            ));
        }
        let mut row = vec![Value::Null; schema.columns.len()];
        for (index, column) in schema.columns.iter().enumerate() {
            if !provided.contains(&index) {
                row[index] = column_default(column)?;
            }
        }
        let constants = constant_schema();
        for (expr, index) in expressions.iter().zip(&column_indexes) {
            row[*index] = if default_expression(expr) {
                column_default(&schema.columns[*index])?
            } else {
                expression_value(expr, schema.columns[*index].data_type, &constants, &[])?
            };
        }
        validate_not_null(&schema, &row)?;
        validate_check_constraints(&schema, &row)?;
        Ok(row)
    };
    let rows = if let Some(source) = &insert.source {
        let SetExpr::Values(values) = source.body.as_ref() else {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "INSERT source is not implemented",
            ));
        };
        values
            .rows
            .iter()
            .map(|expressions| build_row(expressions))
            .collect::<Result<Vec<_>>>()?
    } else {
        assert!(insert.columns.is_empty());
        schema
            .columns
            .iter()
            .map(column_default)
            .collect::<Result<Vec<_>>>()
            .and_then(|row| {
                validate_not_null(&schema, &row)?;
                validate_check_constraints(&schema, &row)?;
                Ok(vec![row])
            })?
    };
    let affected = rows.len() as u64;
    for row in rows {
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .unique_conflict(&row, snapshot, xid, &state.transactions, None)
        {
            return Err(PgError::new(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .insert(xid, row.clone());
        validate_row_foreign_keys(
            state,
            &schema,
            &row,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
    }
    Ok(StatementResult::Affected(affected))
}

fn update_rows(
    state: &mut DatabaseState,
    update_table: &TableWithJoins,
    assignments: &[sqlparser::ast::Assignment],
    selection: Option<&Expr>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
) -> Result<StatementResult> {
    if !update_table.joins.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "UPDATE joins are not implemented",
        ));
    }
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = &update_table.relation
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "UPDATE target is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "UPDATE table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?.clone();
    if let Some(selection) = selection {
        let base = expression_type(selection, &schema)?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let mut assigned = BTreeSet::new();
    let assignments = assignments
        .iter()
        .map(|assignment| {
            let AssignmentTarget::ColumnName(column) = &assignment.target else {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "UPDATE tuple assignment is not implemented",
                ));
            };
            let column_name = name(column)?;
            let index = schema
                .columns
                .iter()
                .position(|definition| definition.name == column_name)
                .ok_or_else(|| {
                    PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {column_name:?} does not exist"),
                    )
                })?;
            if !assigned.insert(index) {
                return Err(PgError::new(
                    SqlState::SyntaxError,
                    "multiple assignments to the same column",
                ));
            }
            if !default_expression(&assignment.value)
                && !null_expression(&assignment.value)
                && unknown_string(&assignment.value).is_none()
                && !coercion::can_cast(
                    expression_type(&assignment.value, &schema)?,
                    schema.columns[index].data_type.base,
                    CastContext::Assignment,
                )
            {
                return Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "column has incompatible type",
                ));
            }
            Ok((index, &assignment.value))
        })
        .collect::<Result<Vec<_>>>()?;
    let targets = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .rows()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
                return Ok(targets);
            };
            if let Some(selection) = selection {
                match evaluate(selection, &schema, &version.row)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(targets),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            targets.push((row_id, version.xmin, version.row.clone()));
            Ok(targets)
        })?;
    let affected = targets.len() as u64;
    for (row_id, version_xmin, row) in targets {
        let mut updated = row.clone();
        for (index, expression) in &assignments {
            let target = schema.columns[*index].data_type;
            updated[*index] = if default_expression(expression) {
                column_default(&schema.columns[*index])?
            } else {
                expression_value(expression, target, &schema, &row)?
            };
        }
        validate_not_null(&schema, &updated)?;
        validate_check_constraints(&schema, &updated)?;
        if state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .unique_conflict(&updated, snapshot, xid, &state.transactions, Some(row_id))
        {
            return Err(PgError::new(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .update(row_id, version_xmin, xid, updated.clone());
        validate_row_foreign_keys(
            state,
            &schema,
            &updated,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
        )?;
        apply_parent_actions(
            state,
            &schema,
            &row,
            Some(&updated),
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &mut BTreeSet::new(),
        )?;
    }
    Ok(StatementResult::Affected(affected))
}

fn delete_rows(
    state: &mut DatabaseState,
    delete: &Delete,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
) -> Result<StatementResult> {
    if !delete.tables.is_empty()
        || delete.using.is_some()
        || delete.returning.is_some()
        || !delete.order_by.is_empty()
        || delete.limit.is_some()
    {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE feature is not implemented",
        ));
    }
    let FromTable::WithFromKeyword(from) = &delete.from else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE without FROM is not implemented",
        ));
    };
    if from.len() != 1 || !from[0].joins.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE joins are not implemented",
        ));
    }
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = &from[0].relation
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE target is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DELETE table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?.clone();
    if let Some(selection) = &delete.selection {
        let base = expression_type(selection, &schema)?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let targets = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .rows()
        .try_fold(Vec::new(), |mut targets, (row_id, chain)| {
            let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
                return Ok(targets);
            };
            if let Some(selection) = &delete.selection {
                match evaluate(selection, &schema, &version.row)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(targets),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            targets.push((row_id, version.xmin));
            Ok(targets)
        })?;
    let affected = targets.len() as u64;
    for (row_id, version_xmin) in targets {
        let row = state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .rows()
            .find_map(|(candidate, chain)| {
                (candidate == row_id).then(|| {
                    visible_version(chain, snapshot, xid, &state.transactions)
                        .map(|version| version.row.clone())
                })
            })
            .flatten()
            .expect("target row must remain visible");
        apply_parent_actions(
            state,
            &schema,
            &row,
            None,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            &mut BTreeSet::new(),
        )?;
        state
            .tables
            .get_mut(&schema.id)
            .expect("catalog table must have storage")
            .tombstone(row_id, version_xmin, xid);
    }
    Ok(StatementResult::Affected(affected))
}

fn foreign_key_action(action: Option<ReferentialAction>) -> ForeignKeyAction {
    match action.unwrap_or(ReferentialAction::NoAction) {
        ReferentialAction::NoAction => ForeignKeyAction::NoAction,
        ReferentialAction::Restrict => ForeignKeyAction::Restrict,
        ReferentialAction::Cascade => ForeignKeyAction::Cascade,
        ReferentialAction::SetNull => ForeignKeyAction::SetNull,
        ReferentialAction::SetDefault => ForeignKeyAction::SetDefault,
    }
}

fn foreign_key_name(name: Option<&Ident>, default: String) -> (String, bool) {
    const MATCH_FULL: &str = "__pg_fake_match_full__";
    let name = name.map(identifier_name).unwrap_or_default();
    let Some(name) = name.strip_prefix(MATCH_FULL) else {
        return (if name.is_empty() { default } else { name }, false);
    };
    (
        if name.is_empty() {
            default
        } else {
            name.into()
        },
        true,
    )
}

fn foreign_key_is_deferred(
    foreign_key: &ForeignKey,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
) -> bool {
    foreign_key.deferrable
        && (foreign_key.initially_deferred
            || defer_all
            || deferred_constraints.contains(&foreign_key.name))
}

fn foreign_key_column_indexes(schema: &TableSchema, columns: &[String]) -> Result<Vec<usize>> {
    columns
        .iter()
        .map(|name| {
            schema
                .columns
                .iter()
                .position(|column| column.name == *name)
                .ok_or_else(|| {
                    PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {name:?} does not exist"),
                    )
                })
        })
        .collect()
}

fn key_matches(left: &[Value], right: &[Value]) -> Result<bool> {
    left.iter()
        .zip(right)
        .try_fold(true, |matches, (left, right)| {
            if !matches {
                return Ok(false);
            }
            Ok(matches!(
                comparison(&BinaryOperator::Eq, left, right)?,
                Value::Bool(true)
            ))
        })
}

fn validate_foreign_key_definitions(catalog: &Catalog, schema: &TableSchema) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            continue;
        };
        if foreign_key.initially_deferred && !foreign_key.deferrable {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "constraint cannot be initially deferred because it is not deferrable",
            ));
        }
        let referred = if foreign_key.foreign_table == schema.name {
            schema
        } else {
            catalog.table(&foreign_key.foreign_table)?
        };
        let referred_columns = if foreign_key.referred_columns.is_empty() {
            referred
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey(columns) => Some(columns.clone()),
                    _ => None,
                })
                .ok_or_else(|| {
                    PgError::new(
                        SqlState::InvalidColumnReference,
                        "there is no primary key for referenced table",
                    )
                })?
        } else {
            foreign_key.referred_columns.clone()
        };
        let local_indexes = foreign_key_column_indexes(schema, &foreign_key.columns)?;
        let referred_indexes = foreign_key_column_indexes(referred, &referred_columns)?;
        if local_indexes.len() != referred_indexes.len() {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "number of referencing and referenced columns for foreign key disagree",
            ));
        }
        if !referred.constraints.iter().any(|constraint| matches!(constraint,
            crate::catalog::Constraint::PrimaryKey(columns) | crate::catalog::Constraint::Unique(columns) if *columns == referred_columns
        )) {
            return Err(PgError::new(SqlState::InvalidColumnReference, "there is no unique constraint matching given keys for referenced table"));
        }
        for (local, referred_index) in local_indexes.iter().zip(referred_indexes) {
            if schema.columns[*local].data_type.base
                != referred.columns[referred_index].data_type.base
            {
                return Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "foreign key constraint cannot be implemented",
                ));
            }
        }
    }
    Ok(())
}

fn validate_row_foreign_keys(
    state: &DatabaseState,
    schema: &TableSchema,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            continue;
        };
        if foreign_key_is_deferred(foreign_key, deferred_constraints, defer_all) {
            continue;
        }
        let local_indexes = foreign_key_column_indexes(schema, &foreign_key.columns)?;
        let key = local_indexes
            .iter()
            .map(|index| row[*index].clone())
            .collect::<Vec<_>>();
        if key.iter().any(Value::is_null) {
            if foreign_key.match_full && !key.iter().all(Value::is_null) {
                return Err(PgError::new(
                    SqlState::ForeignKeyViolation,
                    format!(
                        "insert or update on table {:?} violates foreign key constraint {:?}",
                        schema.name, foreign_key.name
                    ),
                ));
            }
            continue;
        }
        let foreign_schema = state.catalog.table(&foreign_key.foreign_table)?;
        let referred_columns = if foreign_key.referred_columns.is_empty() {
            foreign_schema
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey(columns) => Some(columns.clone()),
                    _ => None,
                })
                .expect("foreign key definition was validated")
        } else {
            foreign_key.referred_columns.clone()
        };
        let referred_indexes = foreign_key_column_indexes(foreign_schema, &referred_columns)?;
        let found = state
            .tables
            .get(&foreign_schema.id)
            .expect("catalog table must have storage")
            .rows()
            .try_fold(false, |found, (_, chain)| {
                if found {
                    return Ok(true);
                }
                let Some(version) = visible_version(chain, snapshot, xid, &state.transactions)
                else {
                    return Ok(false);
                };
                key_matches(
                    &key,
                    &referred_indexes
                        .iter()
                        .map(|index| version.row[*index].clone())
                        .collect::<Vec<_>>(),
                )
            })?;
        if !found {
            return Err(PgError::new(
                SqlState::ForeignKeyViolation,
                format!(
                    "insert or update on table {:?} violates foreign key constraint {:?}",
                    schema.name, foreign_key.name
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_deferred_foreign_keys(state: &DatabaseState, xid: Xid) -> Result<()> {
    let snapshot = Snapshot::new(&state.transactions);
    for schema in state.catalog.tables() {
        let mut schema = schema.clone();
        for constraint in &mut schema.constraints {
            if let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint {
                foreign_key.initially_deferred = false;
            }
        }
        let table = state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage");
        for (_, chain) in table.rows() {
            if let Some(version) = visible_version(chain, &snapshot, xid, &state.transactions) {
                validate_row_foreign_keys(
                    state,
                    &schema,
                    &version.row,
                    xid,
                    &snapshot,
                    &BTreeSet::new(),
                    false,
                )?;
            }
        }
    }
    Ok(())
}

fn apply_parent_actions(
    state: &mut DatabaseState,
    parent_schema: &TableSchema,
    old_row: &[Value],
    new_row: Option<&[Value]>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    visited: &mut BTreeSet<(TableId, RowId)>,
) -> Result<()> {
    let foreign_keys = state
        .catalog
        .tables()
        .flat_map(|schema| {
            schema
                .constraints
                .iter()
                .filter_map(move |constraint| match constraint {
                    crate::catalog::Constraint::ForeignKey(foreign_key)
                        if foreign_key.foreign_table == parent_schema.name =>
                    {
                        Some((schema.clone(), foreign_key.clone()))
                    }
                    _ => None,
                })
        })
        .collect::<Vec<_>>();
    for (child_schema, foreign_key) in foreign_keys {
        let referred_columns = if foreign_key.referred_columns.is_empty() {
            parent_schema
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    crate::catalog::Constraint::PrimaryKey(columns) => Some(columns.clone()),
                    _ => None,
                })
                .expect("foreign key definition was validated")
        } else {
            foreign_key.referred_columns.clone()
        };
        let parent_indexes = foreign_key_column_indexes(parent_schema, &referred_columns)?;
        let old_key = parent_indexes
            .iter()
            .map(|index| old_row[*index].clone())
            .collect::<Vec<_>>();
        let new_key = new_row.map(|row| {
            parent_indexes
                .iter()
                .map(|index| row[*index].clone())
                .collect::<Vec<_>>()
        });
        if new_key.as_ref().is_some_and(|key| {
            key_matches(&old_key, key).expect("matching compatible foreign key values must work")
        }) {
            continue;
        }
        let child_indexes = foreign_key_column_indexes(&child_schema, &foreign_key.columns)?;
        let children = state
            .tables
            .get(&child_schema.id)
            .expect("catalog table must have storage")
            .rows()
            .try_fold(Vec::new(), |mut children, (row_id, chain)| {
                let Some(version) = visible_version(chain, snapshot, xid, &state.transactions)
                else {
                    return Ok(children);
                };
                let key = child_indexes
                    .iter()
                    .map(|index| version.row[*index].clone())
                    .collect::<Vec<_>>();
                if !key.iter().any(Value::is_null) && key_matches(&key, &old_key)? {
                    children.push((row_id, version.xmin, version.row.clone()));
                }
                Ok(children)
            })?;
        for (row_id, version_xmin, row) in children {
            if !visited.insert((child_schema.id, row_id)) {
                continue;
            }
            let action = if new_row.is_some() {
                foreign_key.on_update
            } else {
                foreign_key.on_delete
            };
            if matches!(action, ForeignKeyAction::NoAction)
                && foreign_key_is_deferred(&foreign_key, deferred_constraints, defer_all)
            {
                continue;
            }
            match action {
                ForeignKeyAction::NoAction | ForeignKeyAction::Restrict => {
                    return Err(PgError::new(
                        SqlState::ForeignKeyViolation,
                        format!(
                            "update or delete on table {:?} violates foreign key constraint {:?} on table {:?}",
                            parent_schema.name, foreign_key.name, child_schema.name
                        ),
                    ));
                }
                ForeignKeyAction::Cascade if new_row.is_none() => {
                    apply_parent_actions(
                        state,
                        &child_schema,
                        &row,
                        None,
                        xid,
                        snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                    )?;
                    state
                        .tables
                        .get_mut(&child_schema.id)
                        .expect("catalog table must have storage")
                        .tombstone(row_id, version_xmin, xid);
                }
                ForeignKeyAction::Cascade => {
                    let mut updated = row.clone();
                    for (child, value) in child_indexes
                        .iter()
                        .zip(new_key.as_ref().expect("update has a new key"))
                    {
                        updated[*child] = value.clone();
                    }
                    update_cascaded_row(
                        state,
                        &child_schema,
                        row_id,
                        version_xmin,
                        &row,
                        updated,
                        xid,
                        snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                    )?;
                }
                ForeignKeyAction::SetNull => {
                    let mut updated = row.clone();
                    for child in &child_indexes {
                        updated[*child] = Value::Null;
                    }
                    update_cascaded_row(
                        state,
                        &child_schema,
                        row_id,
                        version_xmin,
                        &row,
                        updated,
                        xid,
                        snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                    )?;
                }
                ForeignKeyAction::SetDefault => {
                    let mut updated = row.clone();
                    for child in &child_indexes {
                        updated[*child] = column_default(&child_schema.columns[*child])?;
                    }
                    update_cascaded_row(
                        state,
                        &child_schema,
                        row_id,
                        version_xmin,
                        &row,
                        updated,
                        xid,
                        snapshot,
                        deferred_constraints,
                        defer_all,
                        visited,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn update_cascaded_row(
    state: &mut DatabaseState,
    schema: &TableSchema,
    row_id: RowId,
    version_xmin: Xid,
    old_row: &[Value],
    updated: Vec<Value>,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<String>,
    defer_all: bool,
    visited: &mut BTreeSet<(TableId, RowId)>,
) -> Result<()> {
    validate_not_null(schema, &updated)?;
    validate_check_constraints(schema, &updated)?;
    if state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .unique_conflict(&updated, snapshot, xid, &state.transactions, Some(row_id))
    {
        return Err(PgError::new(
            SqlState::UniqueViolation,
            format!(
                "duplicate key value violates unique constraint on {:?}",
                schema.name
            ),
        ));
    }
    state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage")
        .update(row_id, version_xmin, xid, updated.clone());
    validate_row_foreign_keys(
        state,
        schema,
        &updated,
        xid,
        snapshot,
        deferred_constraints,
        defer_all,
    )?;
    apply_parent_actions(
        state,
        schema,
        old_row,
        Some(&updated),
        xid,
        snapshot,
        deferred_constraints,
        defer_all,
        visited,
    )?;
    Ok(())
}

fn select_lock_mode(query: &sqlparser::ast::Query) -> Result<Option<RowLockMode>> {
    if query.locks.len() > 1 {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "multiple row-lock clauses are not implemented",
        ));
    }
    let Some(lock) = query.locks.first() else {
        return Ok(None);
    };
    if lock.of.is_some() || lock.nonblock.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "row-lock clause variant is not implemented",
        ));
    }
    Ok(Some(match lock.lock_type {
        LockType::Share => RowLockMode::Share,
        LockType::Update => RowLockMode::Update,
    }))
}

fn select_rows(
    state: &DatabaseState,
    query: &sqlparser::ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<StatementResult> {
    if query.with.is_some() || !query.limit_by.is_empty() || query.fetch.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "query clause is not implemented",
        ));
    }
    select_lock_mode(query)?;
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "query source is not implemented",
        ));
    };
    let GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "GROUP BY is not implemented",
        ));
    };
    if select.distinct.is_some()
        || select.into.is_some()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || select.having.is_some()
        || select.from.len() != 1
    {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "SELECT feature is not implemented",
        ));
    }
    let from = &select.from[0];
    if !from.joins.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "joins are not implemented",
        ));
    }
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = &from.relation
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "FROM source is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?;
    let limit = query
        .limit
        .as_ref()
        .map(|limit| row_count(limit, RowCountClause::Limit))
        .transpose()?
        .flatten();
    let offset = query
        .offset
        .as_ref()
        .map(|offset| row_count(&offset.value, RowCountClause::Offset))
        .transpose()?
        .flatten()
        .unwrap_or(0);
    if let Some(selection) = &select.selection {
        let base = expression_type(selection, schema)?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (projections, columns) = projections_and_columns(&select.projection, schema)?;
    let order_specs = query
        .order_by
        .as_ref()
        .map(|order_by| {
            if order_by.interpolate.is_some() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "ORDER BY INTERPOLATE is not implemented",
                ));
            }
            order_by
                .exprs
                .iter()
                .map(|order| {
                    if order.with_fill.is_some() {
                        return Err(PgError::new(
                            SqlState::FeatureNotSupported,
                            "ORDER BY WITH FILL is not implemented",
                        ));
                    }
                    let key = if let Expr::Value(AstValue::Number(position, _)) = &order.expr
                        && !position.contains(['.', 'e', 'E'])
                    {
                        let position = position.parse::<usize>().map_err(|_| {
                            PgError::new(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            )
                        })?;
                        if position == 0 || position > projections.len() {
                            return Err(PgError::new(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            ));
                        }
                        OrderKey::Output(position - 1)
                    } else {
                        expression_type(&order.expr, schema)?;
                        OrderKey::Expression(&order.expr)
                    };
                    let ascending = order.asc.unwrap_or(true);
                    Ok(OrderSpec {
                        key,
                        ascending,
                        nulls_first: order.nulls_first.unwrap_or(!ascending),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let mut rows = table
        .rows()
        .filter_map(|(_, chain)| visible_version(chain, snapshot, xid, &state.transactions))
        .try_fold(Vec::new(), |mut rows, version| -> Result<Vec<OrderedRow>> {
            if let Some(selection) = &select.selection {
                match evaluate(selection, schema, &version.row)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(rows),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            let values = projections
                .iter()
                .map(|projection| match projection {
                    Projection::Column(index) => Ok(version.row[*index].clone()),
                    Projection::Expression(expr) => evaluate(expr, schema, &version.row),
                })
                .collect::<Result<Vec<_>>>()?;
            let keys = order_specs
                .iter()
                .map(|order| match order.key {
                    OrderKey::Output(index) => Ok(values[index].clone()),
                    OrderKey::Expression(expression) => evaluate(expression, schema, &version.row),
                })
                .collect::<Result<Vec<_>>>()?;
            rows.push(OrderedRow { values, keys });
            Ok(rows)
        })?;
    rows.sort_by(|left, right| {
        order_specs
            .iter()
            .zip(left.keys.iter().zip(&right.keys))
            .find_map(|(spec, (left, right))| {
                let ordering = match (left, right) {
                    (Value::Null, Value::Null) => Ordering::Equal,
                    (Value::Null, _) => {
                        if spec.nulls_first {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (_, Value::Null) => {
                        if spec.nulls_first {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    _ => {
                        let ordering = value_ordering(left, right)
                            .expect("ORDER BY expression type was checked");
                        if spec.ascending {
                            ordering
                        } else {
                            ordering.reverse()
                        }
                    }
                };
                (ordering != Ordering::Equal).then_some(ordering)
            })
            .unwrap_or(Ordering::Equal)
    });
    let rows = rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| row.values)
        .collect();
    Ok(StatementResult::Query(QueryResult { columns, rows }))
}

fn projections_and_columns<'a>(
    projection: &'a [SelectItem],
    schema: &TableSchema,
) -> Result<(Vec<Projection<'a>>, Vec<ColumnMeta>)> {
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        match item {
            SelectItem::Wildcard(_) => {
                for (index, column) in schema.columns.iter().enumerate() {
                    projections.push(Projection::Column(index));
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            SelectItem::UnnamedExpr(Expr::Identifier(column)) => {
                let index = column_index(schema, column)?;
                let column = &schema.columns[index];
                projections.push(Projection::Column(index));
                columns.push(ColumnMeta {
                    name: column.name.clone(),
                    type_oid: column.data_type.oid(),
                    typmod: column.data_type.typmod,
                });
            }
            SelectItem::UnnamedExpr(expr) => {
                let data_type = expression_type(expr, schema)?;
                projections.push(Projection::Expression(expr));
                columns.push(ColumnMeta {
                    name: "?column?".into(),
                    type_oid: data_type.oid(),
                    typmod: PgType::NO_TYPEMOD,
                });
            }
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "SELECT projection is not implemented",
                ));
            }
        }
    }
    Ok((projections, columns))
}

fn row_count(expr: &Expr, clause: RowCountClause) -> Result<Option<usize>> {
    if matches!(clause, RowCountClause::Limit)
        && matches!(expr, Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("all"))
    {
        return Ok(None);
    }
    let schema = constant_schema();
    let value = evaluate_as(expr, BaseType::Int8, CastContext::Implicit, &schema, &[]).map_err(
        |error| {
            if error.sqlstate == SqlState::CannotCoerce {
                PgError::new(
                    SqlState::DatatypeMismatch,
                    match clause {
                        RowCountClause::Limit => "argument of LIMIT must be type bigint",
                        RowCountClause::Offset => "argument of OFFSET must be type bigint",
                    },
                )
            } else {
                error
            }
        },
    )?;
    match value {
        Value::Null => Ok(None),
        Value::Int8(value) if value >= 0 => Ok(Some(usize::try_from(value).unwrap_or(usize::MAX))),
        Value::Int8(_) => Err(PgError::new(
            match clause {
                RowCountClause::Limit => SqlState::InvalidRowCountInLimitClause,
                RowCountClause::Offset => SqlState::InvalidRowCountInResultOffsetClause,
            },
            match clause {
                RowCountClause::Limit => "LIMIT must not be negative",
                RowCountClause::Offset => "OFFSET must not be negative",
            },
        )),
        _ => unreachable!("row count was coerced to bigint"),
    }
}

fn expression_value(
    expr: &Expr,
    target: PgType,
    schema: &TableSchema,
    row: &[Value],
) -> Result<Value> {
    if let Some(text) = unknown_string(expr) {
        coercion::coerce_unknown(text, target, CastContext::Assignment)
    } else {
        coercion::coerce(
            evaluate(expr, schema, row)?,
            expression_type(expr, schema)?,
            target,
            CastContext::Assignment,
        )
    }
}

fn column_default(column: &ColumnDef) -> Result<Value> {
    let Some(expr) = &column.default else {
        return Ok(Value::Null);
    };
    expression_value(expr, column.data_type, &constant_schema(), &[]).map_err(|error| {
        if error.sqlstate == SqlState::UndefinedColumn {
            PgError::new(
                SqlState::FeatureNotSupported,
                "cannot use column reference in DEFAULT expression",
            )
        } else {
            error
        }
    })
}

fn validate_not_null(schema: &TableSchema, row: &[Value]) -> Result<()> {
    if let Some(column) = schema
        .columns
        .iter()
        .zip(row)
        .find_map(|(column, value)| (!column.nullable && value.is_null()).then_some(column))
    {
        return Err(PgError::new(
            SqlState::NotNullViolation,
            format!(
                "null value in column {:?} of relation {:?} violates not-null constraint",
                column.name, schema.name
            ),
        ));
    }
    Ok(())
}

fn validate_check_constraint_types(schema: &TableSchema) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check(expression) = constraint else {
            continue;
        };
        let base = expression_type(expression, schema)?;
        if base != BaseType::Bool
            && !null_expression(expression)
            && unknown_string(expression).is_none()
        {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "CHECK constraint must be a boolean expression",
            ));
        }
    }
    Ok(())
}

fn validate_check_constraints(schema: &TableSchema, row: &[Value]) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check(expression) = constraint else {
            continue;
        };
        match evaluate_as(
            expression,
            BaseType::Bool,
            CastContext::Implicit,
            schema,
            row,
        )? {
            Value::Bool(true) | Value::Null => {}
            Value::Bool(false) => {
                return Err(PgError::new(
                    SqlState::CheckViolation,
                    format!(
                        "new row for relation {:?} violates check constraint",
                        schema.name
                    ),
                ));
            }
            _ => unreachable!("CHECK expression was type-checked"),
        }
    }
    Ok(())
}

fn default_expression(expr: &Expr) -> bool {
    matches!(expr, Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("default"))
}

pub(crate) fn constant_schema() -> TableSchema {
    TableSchema {
        id: TableId(0),
        name: String::new(),
        columns: Vec::new(),
        constraints: Vec::new(),
    }
}

fn literal_value(expr: &Expr) -> Result<Value> {
    match expr {
        Expr::Value(AstValue::Null) => Ok(Value::Null),
        Expr::Value(AstValue::Boolean(value)) => Ok(Value::Bool(*value)),
        Expr::Value(AstValue::SingleQuotedString(value)) => Ok(Value::Text(value.clone())),
        Expr::Value(AstValue::Number(value, _)) if value.contains(['.', 'e', 'E']) => {
            Value::parse(BaseType::Numeric, value)
        }
        Expr::Value(AstValue::Number(value, _)) => integer_literal(value),
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => literal_value(expr),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } if matches!(expr.as_ref(), Expr::Value(AstValue::Number(value, _)) if !value.contains(['.', 'e', 'E'])) =>
        {
            let Expr::Value(AstValue::Number(value, _)) = expr.as_ref() else {
                unreachable!("integer literal pattern was checked")
            };
            integer_literal(&format!("-{value}"))
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } if matches!(expr.as_ref(), Expr::Value(AstValue::Number(_, _))) => {
            unary(UnaryOperator::Minus, literal_value(expr)?)
        }
        Expr::Nested(expr) => literal_value(expr),
        Expr::Value(_) => Err(PgError::new(
            SqlState::CannotCoerce,
            "literal has incompatible type",
        )),
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "expression is not implemented",
        )),
    }
}

fn integer_literal(value: &str) -> Result<Value> {
    if let Ok(value) = value.parse::<i32>() {
        return Ok(Value::Int4(value));
    }
    if let Ok(value) = value.parse::<i64>() {
        return Ok(Value::Int8(value));
    }
    Value::parse(BaseType::Numeric, value)
}

fn unknown_string(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Value(AstValue::SingleQuotedString(value)) => Some(value),
        Expr::Nested(expr) => unknown_string(expr),
        _ => None,
    }
}

fn column_index(schema: &TableSchema, column: &Ident) -> Result<usize> {
    schema
        .columns
        .iter()
        .position(|definition| definition.name == identifier_name(column))
        .ok_or_else(|| {
            PgError::new(
                SqlState::UndefinedColumn,
                format!("column {:?} does not exist", column.value),
            )
        })
}

pub(crate) fn expression_type(expr: &Expr, schema: &TableSchema) -> Result<BaseType> {
    match expr {
        Expr::Identifier(column) => {
            Ok(schema.columns[column_index(schema, column)?].data_type.base)
        }
        Expr::Value(AstValue::Null) => Ok(BaseType::Text),
        Expr::Value(AstValue::Boolean(_)) => Ok(BaseType::Bool),
        Expr::Value(AstValue::SingleQuotedString(_)) => Ok(BaseType::Text),
        Expr::Value(AstValue::Number(value, _)) if value.contains(['.', 'e', 'E']) => {
            Ok(BaseType::Numeric)
        }
        Expr::Value(AstValue::Number(value, _)) => Ok(integer_literal(value)?
            .base_type()
            .expect("numeric literal is not null")),
        Expr::Value(_) => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "literal is not implemented",
        )),
        Expr::Nested(expr) => expression_type(expr, schema),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } if matches!(expr.as_ref(), Expr::Value(AstValue::Number(value, _)) if !value.contains(['.', 'e', 'E'])) =>
        {
            let Expr::Value(AstValue::Number(value, _)) = expr.as_ref() else {
                unreachable!("integer literal pattern was checked")
            };
            Ok(integer_literal(&format!("-{value}"))?
                .base_type()
                .expect("integer literal is not null"))
        }
        Expr::UnaryOp { op, expr } => {
            let base = expression_type(expr, schema)?;
            if matches!(op, UnaryOperator::Plus | UnaryOperator::Minus) && numeric(base) {
                Ok(base)
            } else if matches!(op, UnaryOperator::Not)
                && (base == BaseType::Bool || null_expression(expr))
            {
                Ok(BaseType::Bool)
            } else {
                Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "operator has incompatible type",
                ))
            }
        }
        Expr::BinaryOp { left, op, right } => match op {
            BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Modulo => {
                let left_type = expression_type(left, schema)?;
                let right_type = expression_type(right, schema)?;
                if matches!(left_type, BaseType::Interval)
                    || matches!(right_type, BaseType::Interval)
                {
                    return interval_arithmetic_type(op, left_type, right_type);
                }
                let base = expression_common_type(left, right, schema)?;
                if numeric(base) {
                    Ok(base)
                } else {
                    Err(PgError::new(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ))
                }
            }
            BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Gt
            | BinaryOperator::Lt
            | BinaryOperator::GtEq
            | BinaryOperator::LtEq => {
                expression_common_type(left, right, schema)?;
                Ok(BaseType::Bool)
            }
            BinaryOperator::And | BinaryOperator::Or => {
                let left_base = expression_type(left, schema)?;
                let right_base = expression_type(right, schema)?;
                if (left_base == BaseType::Bool
                    || null_expression(left)
                    || unknown_string(left).is_some())
                    && (right_base == BaseType::Bool
                        || null_expression(right)
                        || unknown_string(right).is_some())
                {
                    Ok(BaseType::Bool)
                } else {
                    Err(PgError::new(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ))
                }
            }
            _ => Err(PgError::new(
                SqlState::DatatypeMismatch,
                "operator has incompatible types",
            )),
        },
        Expr::IsNull(_) | Expr::IsNotNull(_) => Ok(BaseType::Bool),
        Expr::IsTrue(expr) | Expr::IsFalse(expr) | Expr::IsUnknown(expr) => {
            let base = expression_type(expr, schema)?;
            if base == BaseType::Bool || null_expression(expr) || unknown_string(expr).is_some() {
                Ok(BaseType::Bool)
            } else {
                Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "operator has incompatible type",
                ))
            }
        }
        Expr::IsDistinctFrom(left, right) | Expr::IsNotDistinctFrom(left, right) => {
            let left_base = expression_type(left, schema)?;
            let right_base = expression_type(right, schema)?;
            if comparable(left_base, right_base) || null_expression(left) || null_expression(right)
            {
                Ok(BaseType::Bool)
            } else {
                Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "operator has incompatible types",
                ))
            }
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            assert_eq!(conditions.len(), results.len());
            if let Some(operand) = operand {
                for condition in conditions {
                    expression_common_type(operand, condition, schema).map_err(|_| {
                        PgError::new(SqlState::DatatypeMismatch, "CASE types are incompatible")
                    })?;
                }
            } else {
                for condition in conditions {
                    let base = expression_type(condition, schema)?;
                    if base != BaseType::Bool && !null_expression(condition) {
                        return Err(PgError::new(
                            SqlState::DatatypeMismatch,
                            "CASE condition must be boolean",
                        ));
                    }
                }
            }
            common_expression_type(
                results
                    .iter()
                    .chain(else_result.as_deref())
                    .collect::<Vec<_>>()
                    .as_slice(),
                schema,
            )
        }
        Expr::Function(function) => function_type(function, schema),
        Expr::Cast {
            kind,
            expr,
            data_type,
            format,
        } => {
            if !matches!(kind, CastKind::Cast | CastKind::DoubleColon) || format.is_some() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "cast variant is not implemented",
                ));
            }
            let target = coercion::type_from_ast(data_type)?;
            if unknown_string(expr).is_none()
                && !null_expression(expr)
                && !coercion::can_cast(
                    expression_type(expr, schema)?,
                    target.base,
                    CastContext::Explicit,
                )
            {
                return Err(PgError::new(SqlState::CannotCoerce, "types cannot be cast"));
            }
            Ok(target.base)
        }
        Expr::Extract { expr, .. } => {
            let base = expression_type(expr, schema)?;
            if matches!(
                base,
                BaseType::Date | BaseType::Time | BaseType::Timestamp | BaseType::TimestampTz
            ) || null_expression(expr)
            {
                Ok(BaseType::Numeric)
            } else {
                Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "extract source must be a temporal value",
                ))
            }
        }
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "expression is not implemented",
        )),
    }
}

pub(crate) fn null_expression(expr: &Expr) -> bool {
    match expr {
        Expr::Value(AstValue::Null) => true,
        Expr::Nested(expr) => null_expression(expr),
        _ => false,
    }
}

fn numeric(base: BaseType) -> bool {
    matches!(
        base,
        BaseType::Int2
            | BaseType::Int4
            | BaseType::Int8
            | BaseType::Float4
            | BaseType::Float8
            | BaseType::Numeric
    )
}

fn comparable(left: BaseType, right: BaseType) -> bool {
    coercion::common_type(left, right).is_some()
}

fn expression_common_type(left: &Expr, right: &Expr, schema: &TableSchema) -> Result<BaseType> {
    if null_expression(left) && null_expression(right)
        || unknown_string(left).is_some() && unknown_string(right).is_some()
    {
        return Ok(BaseType::Text);
    }
    if null_expression(left) || unknown_string(left).is_some() {
        return expression_type(right, schema);
    }
    if null_expression(right) || unknown_string(right).is_some() {
        return expression_type(left, schema);
    }
    coercion::common_type(
        expression_type(left, schema)?,
        expression_type(right, schema)?,
    )
    .ok_or_else(|| {
        PgError::new(
            SqlState::DatatypeMismatch,
            "expressions have incompatible types",
        )
    })
}

fn common_expression_type(expressions: &[&Expr], schema: &TableSchema) -> Result<BaseType> {
    let mut result = None;
    for expression in expressions {
        if null_expression(expression) || unknown_string(expression).is_some() {
            continue;
        }
        let base = expression_type(expression, schema)?;
        result = Some(match result {
            None => base,
            Some(current) if current == base => current,
            Some(current) if coercion::common_type(current, base).is_some() => {
                coercion::common_type(current, base).expect("common type was checked")
            }
            Some(_) => {
                return Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "expressions have incompatible types",
                ));
            }
        });
    }
    Ok(result.unwrap_or(BaseType::Text))
}

fn function_arguments(function: &Function) -> Result<Vec<&Expr>> {
    if function.uses_odbc_syntax
        || !matches!(function.parameters, FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
    {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "function feature is not implemented",
        ));
    }
    let FunctionArguments::List(arguments) = &function.args else {
        return Err(PgError::new(
            SqlState::UndefinedFunction,
            "function signature does not exist",
        ));
    };
    if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "function argument feature is not implemented",
        ));
    }
    arguments
        .args
        .iter()
        .map(|argument| match argument {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => Ok(expression),
            _ => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "function argument is not implemented",
            )),
        })
        .collect()
}

fn function_type(function: &Function, schema: &TableSchema) -> Result<BaseType> {
    let function_name = name(&function.name)?;
    let arguments = function_arguments(function)?;
    let signature_error = || {
        PgError::new(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )
    };
    match function_name.as_str() {
        "coalesce" | "greatest" | "least" if !arguments.is_empty() => {
            common_expression_type(&arguments, schema)
        }
        "nullif" if arguments.len() == 2 => common_expression_type(&arguments, schema),
        "length" | "lower" | "upper" if arguments.len() == 1 => {
            let base = expression_type(arguments[0], schema)?;
            if !null_expression(arguments[0])
                && !matches!(base, BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
            {
                return Err(signature_error());
            }
            Ok(if function_name == "length" {
                BaseType::Int4
            } else {
                BaseType::Text
            })
        }
        "abs" if arguments.len() == 1 => {
            if unknown_string(arguments[0]).is_some() {
                return Ok(BaseType::Float8);
            }
            let base = expression_type(arguments[0], schema)?;
            if !null_expression(arguments[0]) && !numeric(base) {
                return Err(signature_error());
            }
            Ok(base)
        }
        "gen_random_uuid" | "uuidv4" if arguments.is_empty() => Ok(BaseType::Uuid),
        "coalesce" | "nullif" | "greatest" | "least" | "length" | "lower" | "upper" | "abs" => {
            Err(signature_error())
        }
        _ => Err(PgError::new(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )),
    }
}

fn evaluate(expr: &Expr, schema: &TableSchema, row: &[Value]) -> Result<Value> {
    match expr {
        Expr::Identifier(column) => Ok(row[column_index(schema, column)?].clone()),
        Expr::Value(_) => literal_value(expr),
        Expr::Nested(expr) => evaluate(expr, schema, row),
        Expr::UnaryOp { op, expr } => {
            if matches!(op, UnaryOperator::Minus)
                && let Expr::Value(AstValue::Number(value, _)) = expr.as_ref()
                && !value.contains(['.', 'e', 'E'])
            {
                return integer_literal(&format!("-{value}"));
            }
            unary(*op, evaluate(expr, schema, row)?)
        }
        Expr::BinaryOp { left, op, right } => {
            let left_type = expression_type(left, schema)?;
            let right_type = expression_type(right, schema)?;
            if matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
            ) && (left_type == BaseType::Interval || right_type == BaseType::Interval)
            {
                let left = evaluate(left, schema, row)?;
                let right = evaluate(right, schema, row)?;
                if left.is_null() || right.is_null() {
                    return Ok(Value::Null);
                }
                return temporal_arithmetic(op, left, right);
            }
            let target = if matches!(op, BinaryOperator::And | BinaryOperator::Or) {
                BaseType::Bool
            } else {
                expression_common_type(left, right, schema)?
            };
            let left = evaluate_as(left, target, CastContext::Implicit, schema, row)?;
            let right = evaluate_as(right, target, CastContext::Implicit, schema, row)?;
            match op {
                BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo => {
                    if left.is_null() || right.is_null() {
                        Ok(Value::Null)
                    } else {
                        arithmetic(op, left, right)
                    }
                }
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Gt
                | BinaryOperator::Lt
                | BinaryOperator::GtEq
                | BinaryOperator::LtEq => {
                    if left.is_null() || right.is_null() {
                        Ok(Value::Null)
                    } else {
                        comparison(op, &left, &right)
                    }
                }
                BinaryOperator::And | BinaryOperator::Or => boolean_binary(op, left, right),
                _ => Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "operator is not implemented",
                )),
            }
        }
        Expr::IsNull(expr) => Ok(Value::Bool(evaluate(expr, schema, row)?.is_null())),
        Expr::IsNotNull(expr) => Ok(Value::Bool(!evaluate(expr, schema, row)?.is_null())),
        Expr::IsTrue(expr) => Ok(Value::Bool(matches!(
            evaluate_as(expr, BaseType::Bool, CastContext::Implicit, schema, row)?,
            Value::Bool(true)
        ))),
        Expr::IsFalse(expr) => Ok(Value::Bool(matches!(
            evaluate_as(expr, BaseType::Bool, CastContext::Implicit, schema, row)?,
            Value::Bool(false)
        ))),
        Expr::IsUnknown(expr) => Ok(Value::Bool(
            evaluate_as(expr, BaseType::Bool, CastContext::Implicit, schema, row)?.is_null(),
        )),
        Expr::IsDistinctFrom(left, right) | Expr::IsNotDistinctFrom(left, right) => {
            let target = expression_common_type(left, right, schema)?;
            distinct(
                evaluate_as(left, target, CastContext::Implicit, schema, row)?,
                evaluate_as(right, target, CastContext::Implicit, schema, row)?,
                matches!(expr, Expr::IsNotDistinctFrom(_, _)),
            )
        }
        Expr::Case {
            operand,
            conditions,
            results,
            else_result,
        } => {
            let result_type = common_expression_type(
                results
                    .iter()
                    .chain(else_result.as_deref())
                    .collect::<Vec<_>>()
                    .as_slice(),
                schema,
            )?;
            let operand = operand.as_deref();
            for (condition, result) in conditions.iter().zip(results) {
                let matches = if let Some(operand) = &operand {
                    let target = expression_common_type(operand, condition, schema)?;
                    let operand = evaluate_as(operand, target, CastContext::Implicit, schema, row)?;
                    let condition =
                        evaluate_as(condition, target, CastContext::Implicit, schema, row)?;
                    if operand.is_null() || condition.is_null() {
                        false
                    } else {
                        matches!(
                            comparison(&BinaryOperator::Eq, &operand, &condition)?,
                            Value::Bool(true)
                        )
                    }
                } else {
                    matches!(evaluate(condition, schema, row)?, Value::Bool(true))
                };
                if matches {
                    return evaluate_as(result, result_type, CastContext::Implicit, schema, row);
                }
            }
            match else_result {
                Some(result) => {
                    evaluate_as(result, result_type, CastContext::Implicit, schema, row)
                }
                None => Ok(Value::Null),
            }
        }
        Expr::Function(function) => evaluate_function(function, schema, row),
        Expr::Cast {
            kind,
            expr,
            data_type,
            format,
        } => {
            if !matches!(kind, CastKind::Cast | CastKind::DoubleColon) || format.is_some() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "cast variant is not implemented",
                ));
            }
            let target = coercion::type_from_ast(data_type)?;
            if let Some(text) = unknown_string(expr) {
                coercion::coerce_unknown(text, target, CastContext::Explicit)
            } else {
                coercion::coerce(
                    evaluate(expr, schema, row)?,
                    expression_type(expr, schema)?,
                    target,
                    CastContext::Explicit,
                )
            }
        }
        Expr::Extract { field, expr, .. } => {
            extract_value(field.clone(), evaluate(expr, schema, row)?)
        }
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "expression is not implemented",
        )),
    }
}

fn evaluate_as(
    expression: &Expr,
    target: BaseType,
    context: CastContext,
    schema: &TableSchema,
    row: &[Value],
) -> Result<Value> {
    if let Some(text) = unknown_string(expression) {
        coercion::coerce_unknown(text, PgType::new(target), context)
    } else {
        let source = expression_type(expression, schema)?;
        coercion::coerce(
            evaluate(expression, schema, row)?,
            source,
            PgType::new(target),
            context,
        )
    }
}

fn evaluate_function(function: &Function, schema: &TableSchema, row: &[Value]) -> Result<Value> {
    function_type(function, schema)?;
    let function_name = name(&function.name)?;
    let arguments = function_arguments(function)?;
    let result_type = function_type(function, schema)?;
    match function_name.as_str() {
        "gen_random_uuid" | "uuidv4" => Ok(Value::Uuid(uuid::Uuid::new_v4())),
        "coalesce" => {
            for argument in arguments {
                let value = evaluate_as(argument, result_type, CastContext::Implicit, schema, row)?;
                if !value.is_null() {
                    return Ok(value);
                }
            }
            Ok(Value::Null)
        }
        "nullif" => {
            let left = evaluate_as(
                arguments[0],
                result_type,
                CastContext::Implicit,
                schema,
                row,
            )?;
            if left.is_null() {
                return Ok(Value::Null);
            }
            let right = evaluate_as(
                arguments[1],
                result_type,
                CastContext::Implicit,
                schema,
                row,
            )?;
            if !right.is_null()
                && matches!(
                    comparison(&BinaryOperator::Eq, &left, &right)?,
                    Value::Bool(true)
                )
            {
                Ok(Value::Null)
            } else {
                Ok(left)
            }
        }
        "greatest" | "least" => {
            let mut selected = None;
            for argument in arguments {
                let value = evaluate_as(argument, result_type, CastContext::Implicit, schema, row)?;
                if value.is_null() {
                    continue;
                }
                selected = Some(match selected {
                    None => value,
                    Some(current) => {
                        let operator = if function_name == "greatest" {
                            BinaryOperator::Gt
                        } else {
                            BinaryOperator::Lt
                        };
                        if matches!(comparison(&operator, &value, &current)?, Value::Bool(true)) {
                            value
                        } else {
                            current
                        }
                    }
                });
            }
            Ok(selected.unwrap_or(Value::Null))
        }
        "length" => match evaluate(arguments[0], schema, row)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Int4(
                i32::try_from(value.chars().count()).expect("text length must fit in int4"),
            )),
            _ => unreachable!("length argument was type-checked"),
        },
        "lower" => match evaluate(arguments[0], schema, row)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Text(value.to_lowercase())),
            _ => unreachable!("lower argument was type-checked"),
        },
        "upper" => match evaluate(arguments[0], schema, row)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Text(value.to_uppercase())),
            _ => unreachable!("upper argument was type-checked"),
        },
        "abs" => match evaluate(arguments[0], schema, row)? {
            Value::Null => Ok(Value::Null),
            Value::Int2(value) => value.checked_abs().map(Value::Int2).ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "smallint out of range")
            }),
            Value::Int4(value) => value.checked_abs().map(Value::Int4).ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "integer out of range")
            }),
            Value::Int8(value) => value.checked_abs().map(Value::Int8).ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "bigint out of range")
            }),
            Value::Float4(value) => Ok(Value::Float4(value.abs())),
            Value::Float8(value) => Ok(Value::Float8(value.abs())),
            Value::Numeric(value) => Ok(Value::Numeric(value.abs())),
            _ => unreachable!("abs argument was type-checked"),
        },
        _ => unreachable!("function name was type-checked"),
    }
}

fn unary(operator: UnaryOperator, value: Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    match (operator, value) {
        (UnaryOperator::Plus, value @ (Value::Int2(_) | Value::Int4(_) | Value::Int8(_))) => {
            Ok(value)
        }
        (
            UnaryOperator::Plus,
            value @ (Value::Float4(_) | Value::Float8(_) | Value::Numeric(_)),
        ) => Ok(value),
        (UnaryOperator::Minus, Value::Int2(value)) => value
            .checked_neg()
            .map(Value::Int2)
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "smallint out of range")),
        (UnaryOperator::Minus, Value::Int4(value)) => value
            .checked_neg()
            .map(Value::Int4)
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "integer out of range")),
        (UnaryOperator::Minus, Value::Int8(value)) => value
            .checked_neg()
            .map(Value::Int8)
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "bigint out of range")),
        (UnaryOperator::Minus, Value::Float4(value)) => Ok(Value::Float4(-value)),
        (UnaryOperator::Minus, Value::Float8(value)) => Ok(Value::Float8(-value)),
        (UnaryOperator::Minus, Value::Numeric(value)) => Ok(Value::Numeric(-value)),
        (UnaryOperator::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible type",
        )),
    }
}

fn boolean_binary(operator: &BinaryOperator, left: Value, right: Value) -> Result<Value> {
    match (operator, left, right) {
        (BinaryOperator::And, Value::Bool(false), _)
        | (BinaryOperator::And, _, Value::Bool(false)) => Ok(Value::Bool(false)),
        (BinaryOperator::And, Value::Bool(true), value)
        | (BinaryOperator::And, value, Value::Bool(true)) => Ok(value),
        (BinaryOperator::And, Value::Null, Value::Null) => Ok(Value::Null),
        (BinaryOperator::Or, Value::Bool(true), _) | (BinaryOperator::Or, _, Value::Bool(true)) => {
            Ok(Value::Bool(true))
        }
        (BinaryOperator::Or, Value::Bool(false), value)
        | (BinaryOperator::Or, value, Value::Bool(false)) => Ok(value),
        (BinaryOperator::Or, Value::Null, Value::Null) => Ok(Value::Null),
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

fn distinct(left: Value, right: Value, equal: bool) -> Result<Value> {
    match (&left, &right) {
        (Value::Null, Value::Null) => Ok(Value::Bool(equal)),
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Bool(!equal)),
        _ => match comparison(&BinaryOperator::Eq, &left, &right)? {
            Value::Bool(value) => Ok(Value::Bool(value == equal)),
            _ => unreachable!("comparison always returns a boolean"),
        },
    }
}

fn arithmetic(operator: &BinaryOperator, left: Value, right: Value) -> Result<Value> {
    macro_rules! integer {
        ($left:expr, $right:expr, $variant:ident, $name:literal) => {{
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && $right == 0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            let value = match operator {
                BinaryOperator::Plus => $left.checked_add($right),
                BinaryOperator::Minus => $left.checked_sub($right),
                BinaryOperator::Multiply => $left.checked_mul($right),
                BinaryOperator::Divide => $left.checked_div($right),
                BinaryOperator::Modulo => $left.checked_rem($right),
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            value.map(Value::$variant).ok_or_else(|| {
                PgError::new(
                    SqlState::NumericValueOutOfRange,
                    concat!($name, " out of range"),
                )
            })
        }};
    }

    match (left, right) {
        (Value::Int2(left), Value::Int2(right)) => integer!(left, right, Int2, "smallint"),
        (Value::Int4(left), Value::Int4(right)) => integer!(left, right, Int4, "integer"),
        (Value::Int8(left), Value::Int8(right)) => integer!(left, right, Int8, "bigint"),
        (Value::Float4(left), Value::Float4(right)) => {
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && right == 0.0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            let value = match operator {
                BinaryOperator::Plus => left + right,
                BinaryOperator::Minus => left - right,
                BinaryOperator::Multiply => left * right,
                BinaryOperator::Divide => left / right,
                BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            if value.is_infinite() && left.is_finite() && right.is_finite() {
                Err(PgError::new(
                    SqlState::NumericValueOutOfRange,
                    "real out of range",
                ))
            } else {
                Ok(Value::Float4(value))
            }
        }
        (Value::Float8(left), Value::Float8(right)) => {
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && right == 0.0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            let value = match operator {
                BinaryOperator::Plus => left + right,
                BinaryOperator::Minus => left - right,
                BinaryOperator::Multiply => left * right,
                BinaryOperator::Divide => left / right,
                BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            if value.is_infinite() && left.is_finite() && right.is_finite() {
                Err(PgError::new(
                    SqlState::NumericValueOutOfRange,
                    "double precision out of range",
                ))
            } else {
                Ok(Value::Float8(value))
            }
        }
        (Value::Numeric(left), Value::Numeric(right)) => {
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && right == 0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            Ok(Value::Numeric(match operator {
                BinaryOperator::Plus => left + right,
                BinaryOperator::Minus => left - right,
                BinaryOperator::Multiply => left * right,
                BinaryOperator::Divide => left / right,
                BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            }))
        }
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

fn interval_arithmetic_type(
    operator: &BinaryOperator,
    left: BaseType,
    right: BaseType,
) -> Result<BaseType> {
    let numeric = |value| {
        matches!(
            value,
            BaseType::Int2
                | BaseType::Int4
                | BaseType::Int8
                | BaseType::Float4
                | BaseType::Float8
                | BaseType::Numeric
        )
    };
    match (operator, left, right) {
        (BinaryOperator::Plus | BinaryOperator::Minus, BaseType::Interval, BaseType::Interval) => {
            Ok(BaseType::Interval)
        }
        (
            BinaryOperator::Plus,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
            BaseType::Interval,
        )
        | (
            BinaryOperator::Plus,
            BaseType::Interval,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
        )
        | (
            BinaryOperator::Minus,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
            BaseType::Interval,
        ) => Ok(if left == BaseType::Interval {
            right
        } else {
            left
        }),
        (BinaryOperator::Multiply, BaseType::Interval, right) if numeric(right) => {
            Ok(BaseType::Interval)
        }
        (BinaryOperator::Multiply, left, BaseType::Interval) if numeric(left) => {
            Ok(BaseType::Interval)
        }
        (BinaryOperator::Divide, BaseType::Interval, right) if numeric(right) => {
            Ok(BaseType::Interval)
        }
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

fn temporal_arithmetic(operator: &BinaryOperator, left: Value, right: Value) -> Result<Value> {
    use chrono::{Days, Months, TimeDelta};
    fn interval_scale(
        value: crate::value::PgInterval,
        factor: f64,
    ) -> Result<crate::value::PgInterval> {
        if !factor.is_finite() {
            return Err(PgError::new(
                SqlState::NumericValueOutOfRange,
                "interval out of range",
            ));
        }
        Ok(crate::value::PgInterval {
            months: (f64::from(value.months) * factor).round() as i32,
            days: (f64::from(value.days) * factor).round() as i32,
            micros: (value.micros as f64 * factor).round() as i64,
        })
    }
    fn signed_interval(
        mut interval: crate::value::PgInterval,
        negative: bool,
    ) -> crate::value::PgInterval {
        if negative {
            interval.months = -interval.months;
            interval.days = -interval.days;
            interval.micros = -interval.micros;
        }
        interval
    }
    fn add_naive(
        mut value: chrono::NaiveDateTime,
        interval: crate::value::PgInterval,
    ) -> Result<chrono::NaiveDateTime> {
        if interval.months != 0 {
            value = if interval.months > 0 {
                value.checked_add_months(Months::new(interval.months as u32))
            } else {
                value.checked_sub_months(Months::new(interval.months.unsigned_abs()))
            }
            .ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })?;
        }
        if interval.days != 0 {
            value = if interval.days > 0 {
                value.checked_add_days(Days::new(interval.days as u64))
            } else {
                value.checked_sub_days(Days::new(interval.days.unsigned_abs() as u64))
            }
            .ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })?;
        }
        value
            .checked_add_signed(TimeDelta::microseconds(interval.micros))
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "timestamp out of range"))
    }
    match (operator, left, right) {
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::Interval(left),
            Value::Interval(right),
        ) => {
            let right = signed_interval(right, matches!(operator, BinaryOperator::Minus));
            Ok(Value::Interval(crate::value::PgInterval {
                months: left.months.checked_add(right.months).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                days: left.days.checked_add(right.days).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                micros: left.micros.checked_add(right.micros).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
            }))
        }
        (BinaryOperator::Multiply | BinaryOperator::Divide, Value::Interval(value), number) => {
            let factor = match number {
                Value::Int2(v) => f64::from(v),
                Value::Int4(v) => f64::from(v),
                Value::Int8(v) => v as f64,
                Value::Float4(v) => f64::from(v),
                Value::Float8(v) => v,
                Value::Numeric(v) => v.to_f64().ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                _ => {
                    return Err(PgError::new(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ));
                }
            };
            if matches!(operator, BinaryOperator::Divide) && factor == 0.0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            interval_scale(
                value,
                if matches!(operator, BinaryOperator::Divide) {
                    1.0 / factor
                } else {
                    factor
                },
            )
            .map(Value::Interval)
        }
        (BinaryOperator::Multiply, number, Value::Interval(value)) => {
            temporal_arithmetic(operator, Value::Interval(value), number)
        }
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::Date(crate::value::PgDate::Finite(date)),
            Value::Interval(interval),
        ) => {
            let value = add_naive(
                date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
                signed_interval(interval, matches!(operator, BinaryOperator::Minus)),
            )?;
            Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(value)))
        }
        (BinaryOperator::Plus, Value::Interval(interval), value @ Value::Date(_)) => {
            temporal_arithmetic(operator, value, Value::Interval(interval))
        }
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::Timestamp(crate::value::PgTimestamp::Finite(value)),
            Value::Interval(interval),
        ) => Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(
            add_naive(
                value,
                signed_interval(interval, matches!(operator, BinaryOperator::Minus)),
            )?,
        ))),
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)),
            Value::Interval(interval),
        ) => Ok(Value::TimestampTz(crate::value::PgTimestampTz::Finite(
            add_naive(
                value.naive_utc(),
                signed_interval(interval, matches!(operator, BinaryOperator::Minus)),
            )?
            .and_utc(),
        ))),
        (BinaryOperator::Plus, Value::Interval(interval), value @ Value::Timestamp(_))
        | (BinaryOperator::Plus, Value::Interval(interval), value @ Value::TimestampTz(_)) => {
            temporal_arithmetic(operator, value, Value::Interval(interval))
        }
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

fn comparison(operator: &BinaryOperator, left: &Value, right: &Value) -> Result<Value> {
    let ordering = value_ordering(left, right)?;
    Ok(Value::Bool(match operator {
        BinaryOperator::Eq => ordering == Ordering::Equal,
        BinaryOperator::NotEq => ordering != Ordering::Equal,
        BinaryOperator::Gt => ordering == Ordering::Greater,
        BinaryOperator::Lt => ordering == Ordering::Less,
        BinaryOperator::GtEq => ordering != Ordering::Less,
        BinaryOperator::LtEq => ordering != Ordering::Greater,
        _ => unreachable!("comparison operator was checked by caller"),
    }))
}

fn extract_value(field: DateTimeField, value: Value) -> Result<Value> {
    use chrono::{Datelike, Timelike};
    let value = match value {
        Value::Null => return Ok(Value::Null),
        Value::Date(crate::value::PgDate::Finite(value)) => match field {
            DateTimeField::Year => value.year() as i64,
            DateTimeField::Month => i64::from(value.month()),
            DateTimeField::Day => i64::from(value.day()),
            DateTimeField::Dow => i64::from(value.weekday().num_days_from_sunday()),
            DateTimeField::Doy => i64::from(value.ordinal()),
            DateTimeField::Epoch => i64::from(value.num_days_from_ce()) * 86_400,
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "date part is not implemented",
                ));
            }
        },
        Value::Time(crate::value::PgTime(value)) => match field {
            DateTimeField::Hour => value / 3_600_000_000,
            DateTimeField::Minute => value / 60_000_000 % 60,
            DateTimeField::Second => value / 1_000_000 % 60,
            DateTimeField::Microsecond | DateTimeField::Microseconds => value % 1_000_000,
            DateTimeField::Epoch => value / 1_000_000,
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "date part is not implemented",
                ));
            }
        },
        Value::Date(crate::value::PgDate::Infinity | crate::value::PgDate::NegInfinity) => {
            return Err(PgError::new(
                SqlState::NumericValueOutOfRange,
                "cannot extract from infinite date",
            ));
        }
        Value::Timestamp(crate::value::PgTimestamp::Finite(value)) => match field {
            DateTimeField::Year => value.year() as i64,
            DateTimeField::Month => i64::from(value.month()),
            DateTimeField::Day => i64::from(value.day()),
            DateTimeField::Hour => i64::from(value.hour()),
            DateTimeField::Minute => i64::from(value.minute()),
            DateTimeField::Second => i64::from(value.second()),
            DateTimeField::Microsecond | DateTimeField::Microseconds => {
                i64::from(value.nanosecond() / 1_000)
            }
            DateTimeField::Epoch => value.and_utc().timestamp(),
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "date part is not implemented",
                ));
            }
        },
        Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)) => match field {
            DateTimeField::Year => value.year() as i64,
            DateTimeField::Month => i64::from(value.month()),
            DateTimeField::Day => i64::from(value.day()),
            DateTimeField::Hour => i64::from(value.hour()),
            DateTimeField::Minute => i64::from(value.minute()),
            DateTimeField::Second => i64::from(value.second()),
            DateTimeField::Microsecond | DateTimeField::Microseconds => {
                i64::from(value.nanosecond() / 1_000)
            }
            DateTimeField::Epoch => value.timestamp(),
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "date part is not implemented",
                ));
            }
        },
        Value::Timestamp(
            crate::value::PgTimestamp::Infinity | crate::value::PgTimestamp::NegInfinity,
        )
        | Value::TimestampTz(
            crate::value::PgTimestampTz::Infinity | crate::value::PgTimestampTz::NegInfinity,
        ) => {
            return Err(PgError::new(
                SqlState::NumericValueOutOfRange,
                "cannot extract from infinite timestamp",
            ));
        }
        _ => {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "extract source must be date or time",
            ));
        }
    };
    Ok(Value::Numeric(value.into()))
}

fn value_ordering(left: &Value, right: &Value) -> Result<Ordering> {
    Ok(match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
        (Value::Int2(left), Value::Int2(right)) => left.cmp(right),
        (Value::Int4(left), Value::Int4(right)) => left.cmp(right),
        (Value::Int8(left), Value::Int8(right)) => left.cmp(right),
        (Value::Float4(left), Value::Float4(right)) => float4_ordering(*left, *right),
        (Value::Float8(left), Value::Float8(right)) => float8_ordering(*left, *right),
        (Value::Numeric(left), Value::Numeric(right)) => left.cmp(right),
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        (Value::Bytea(left), Value::Bytea(right)) => left.cmp(right),
        (Value::Uuid(left), Value::Uuid(right)) => left.cmp(right),
        (Value::Date(left), Value::Date(right)) => left.cmp(right),
        (Value::Time(left), Value::Time(right)) => left.cmp(right),
        (Value::Timestamp(left), Value::Timestamp(right)) => left.cmp(right),
        (Value::TimestampTz(left), Value::TimestampTz(right)) => left.cmp(right),
        (Value::Interval(left), Value::Interval(right)) => left.cmp(right),
        _ => {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "operator has incompatible types",
            ));
        }
    })
}

fn float4_ordering(left: f32, right: f32) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .expect("finite floats are comparable"),
    }
}

fn float8_ordering(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .expect("finite floats are comparable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_ordering_covers_all_phase_one_types() {
        let pairs = [
            (Value::Bool(false), Value::Bool(true)),
            (Value::Int2(1), Value::Int2(2)),
            (Value::Int4(1), Value::Int4(2)),
            (Value::Int8(1), Value::Int8(2)),
            (Value::Float4(1.0), Value::Float4(2.0)),
            (Value::Float8(1.0), Value::Float8(2.0)),
            (
                Value::Numeric("1".parse().unwrap()),
                Value::Numeric("2".parse().unwrap()),
            ),
            (Value::Text("a".into()), Value::Text("b".into())),
            (Value::Bytea(vec![1]), Value::Bytea(vec![2])),
        ];

        for (lower, higher) in pairs {
            assert_eq!(value_ordering(&lower, &higher).unwrap(), Ordering::Less);
            assert_eq!(value_ordering(&higher, &lower).unwrap(), Ordering::Greater);
        }

        assert_eq!(
            value_ordering(&Value::Float4(f32::NAN), &Value::Float4(1.0)).unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            value_ordering(&Value::Float8(f64::NAN), &Value::Float8(1.0)).unwrap(),
            Ordering::Greater
        );
    }
}
