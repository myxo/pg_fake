use crate::{
    api::{ColumnMeta, QueryResult},
    catalog::{Catalog, ColumnDef, TableId, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    storage::Table,
    txn::{
        RowLockKey, RowLockManager, RowLockMode, Snapshot, TransactionManager, TransactionStatus,
        WaitForGraph, Xid, visible_version,
    },
    value::{BaseType, PgType, Value},
};
use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, CastKind, ColumnOption, Delete, Expr, FromTable, Function,
    FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, Ident, LockType, ObjectType,
    SelectItem, SetExpr, Statement, TableConstraint, TableFactor, TableWithJoins, UnaryOperator,
    Value as AstValue,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

pub(crate) struct DatabaseState {
    pub catalog: Catalog,
    pub tables: BTreeMap<TableId, Table>,
    pub transactions: TransactionManager,
    pub row_locks: RowLockManager,
    pub wait_for: WaitForGraph,
}
pub(crate) enum ExecutionResult {
    Affected(u64),
    Query(QueryResult),
}
pub(crate) struct RequiredRowLock {
    pub key: RowLockKey,
    pub mode: RowLockMode,
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
    pub fn new() -> Self {
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

pub(crate) fn dispatch(
    state: &mut DatabaseState,
    statement: &Statement,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<ExecutionResult> {
    match statement {
        Statement::CreateTable(create) => {
            let name = name(&create.name)?;
            if create.if_not_exists && state.catalog.table(&name).is_ok() {
                return Ok(ExecutionResult::Affected(0));
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
                    crate::catalog::Constraint::Check(_) => continue,
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
                name: name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
            })?;
            let id = state
                .catalog
                .create_table(name.clone(), columns, constraints)?;
            let table = state
                .catalog
                .table(&name)
                .expect("created table must exist");
            state.tables.insert(id, Table::new(table.clone()));
            Ok(ExecutionResult::Affected(0))
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
            Ok(ExecutionResult::Affected(affected))
        }
        Statement::Insert(insert) => insert_rows(state, insert, xid, snapshot),
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
            update_rows(state, table, assignments, selection.as_ref(), xid, snapshot)
        }
        Statement::Delete(delete) => delete_rows(state, delete, xid, snapshot),
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
) -> Result<ExecutionResult> {
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
    let table = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage");
    let affected = rows.len() as u64;
    for row in rows {
        if table.unique_conflict(&row, snapshot, xid, &state.transactions, None) {
            return Err(PgError::new(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        table.insert(xid, row);
    }
    Ok(ExecutionResult::Affected(affected))
}

fn update_rows(
    state: &mut DatabaseState,
    update_table: &TableWithJoins,
    assignments: &[sqlparser::ast::Assignment],
    selection: Option<&Expr>,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<ExecutionResult> {
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
    let table = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage");
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
        if table.unique_conflict(&updated, snapshot, xid, &state.transactions, Some(row_id)) {
            return Err(PgError::new(
                SqlState::UniqueViolation,
                format!(
                    "duplicate key value violates unique constraint on {:?}",
                    schema.name
                ),
            ));
        }
        table.update(row_id, version_xmin, xid, updated);
    }
    Ok(ExecutionResult::Affected(affected))
}

fn delete_rows(
    state: &mut DatabaseState,
    delete: &Delete,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<ExecutionResult> {
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
    let table = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage");
    for (row_id, version_xmin) in targets {
        table.tombstone(row_id, version_xmin, xid);
    }
    Ok(ExecutionResult::Affected(affected))
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
) -> Result<ExecutionResult> {
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
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in &select.projection {
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
    Ok(ExecutionResult::Query(QueryResult { columns, rows }))
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
