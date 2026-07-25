use crate::{
    api::{ColumnMeta, QueryResult},
    catalog::{Catalog, ColumnDef, TableId},
    error::{PgError, Result, SqlState},
    storage::Table,
    txn::{Snapshot, TransactionManager, Xid, visible_version},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast::{
    ColumnOption, Expr, GroupByExpr, Ident, ObjectType, SelectItem, SetExpr, Statement,
    TableConstraint, TableFactor, Value as AstValue,
};
use std::collections::BTreeMap;

pub(crate) struct DatabaseState {
    pub catalog: Catalog,
    pub tables: BTreeMap<TableId, Table>,
    pub transactions: TransactionManager,
}
pub(crate) enum ExecutionResult {
    Affected(u64),
    Query(QueryResult),
}
impl DatabaseState {
    pub fn new() -> Self {
        DatabaseState {
            catalog: Catalog::new(),
            tables: BTreeMap::new(),
            transactions: TransactionManager::new(),
        }
    }
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
                let text = column.data_type.to_string();
                let base = BaseType::from_name(text.split('(').next().unwrap().trim()).ok_or_else(
                    || {
                        PgError::new(
                            SqlState::UndefinedObject,
                            format!("type {text} does not exist"),
                        )
                    },
                )?;
                let params = text
                    .split_once('(')
                    .map(|(_, params)| {
                        params
                            .trim_end_matches(')')
                            .split(',')
                            .map(str::trim)
                            .map(str::parse::<i32>)
                            .collect::<std::result::Result<Vec<_>, _>>()
                    })
                    .transpose()
                    .map_err(|_| {
                        PgError::new(
                            SqlState::UndefinedObject,
                            format!("type {text} does not exist"),
                        )
                    })?;
                let data_type = match (base, params.as_deref()) {
                    (BaseType::Varchar | BaseType::Bpchar, Some([length])) => {
                        PgType::with_typmod(base, length + 4)
                    }
                    (BaseType::Numeric, Some([precision, scale])) => {
                        PgType::with_typmod(base, (precision << 16) + scale + 4)
                    }
                    (_, None) => PgType::new(base),
                    _ => {
                        return Err(PgError::new(
                            SqlState::UndefinedObject,
                            format!("type {text} does not exist"),
                        ));
                    }
                };
                let mut nullable = true;
                let mut default = None;
                for option in &column.options {
                    match &option.option {
                        ColumnOption::Null => nullable = true,
                        ColumnOption::NotNull => nullable = false,
                        ColumnOption::Default(expr) => default = Some(expr.clone()),
                        ColumnOption::Unique { is_primary, .. } => {
                            let columns = vec![column.name.value.clone()];
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
                columns.push(ColumnDef {
                    name: identifier_name(&column.name),
                    data_type,
                    nullable,
                    default,
                });
            }
            for constraint in &create.constraints {
                match constraint {
                    TableConstraint::PrimaryKey { columns, .. } => {
                        constraints.push(crate::catalog::Constraint::PrimaryKey(
                            columns.iter().map(|column| column.value.clone()).collect(),
                        ))
                    }
                    TableConstraint::Unique { columns, .. } => {
                        constraints.push(crate::catalog::Constraint::Unique(
                            columns.iter().map(|column| column.value.clone()).collect(),
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
        Statement::Insert(insert) => insert_rows(state, insert, xid),
        Statement::Query(query) => select_rows(state, query, xid, snapshot),
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "statement is not implemented",
        )),
    }
}
fn name(name: &sqlparser::ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "schemas are not implemented",
        ));
    }
    Ok(identifier_name(&name.0[0]))
}

fn identifier_name(identifier: &Ident) -> String {
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
) -> Result<ExecutionResult> {
    let table_name = name(&insert.table_name)?;
    let schema = state.catalog.table(&table_name)?.clone();
    if insert.returning.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "INSERT RETURNING is not implemented",
        ));
    }
    let Some(source) = &insert.source else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "DEFAULT VALUES is not implemented",
        ));
    };
    let SetExpr::Values(values) = source.body.as_ref() else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "INSERT source is not implemented",
        ));
    };
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
    if column_indexes.len() != schema.columns.len() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "omitted INSERT columns are not implemented",
        ));
    }
    let table = state
        .tables
        .get_mut(&schema.id)
        .expect("catalog table must have storage");
    for expressions in &values.rows {
        if expressions.len() != column_indexes.len() {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "INSERT has wrong number of values",
            ));
        }
        let mut row = vec![Value::Null; schema.columns.len()];
        for (expr, index) in expressions.iter().zip(&column_indexes) {
            row[*index] = value(expr, schema.columns[*index].data_type.base)?;
        }
        table.insert(xid, row);
    }
    Ok(ExecutionResult::Affected(values.rows.len() as u64))
}

fn select_rows(
    state: &DatabaseState,
    query: &sqlparser::ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
) -> Result<ExecutionResult> {
    if query.with.is_some()
        || query.order_by.is_some()
        || query.limit.is_some()
        || !query.limit_by.is_empty()
        || query.offset.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
    {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "query clause is not implemented",
        ));
    }
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
        || select.selection.is_some()
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
    let mut indexes = Vec::new();
    for item in &select.projection {
        match item {
            SelectItem::Wildcard(_) => indexes.extend(0..schema.columns.len()),
            SelectItem::UnnamedExpr(Expr::Identifier(column)) => {
                indexes.push(
                    schema
                        .columns
                        .iter()
                        .position(|definition| definition.name == identifier_name(column))
                        .ok_or_else(|| {
                            PgError::new(
                                SqlState::UndefinedColumn,
                                format!("column {:?} does not exist", column.value),
                            )
                        })?,
                );
            }
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "SELECT projection is not implemented",
                ));
            }
        }
    }
    let columns = indexes
        .iter()
        .map(|index| {
            let column = &schema.columns[*index];
            ColumnMeta {
                name: column.name.clone(),
                type_oid: column.data_type.oid(),
                typmod: column.data_type.typmod,
            }
        })
        .collect();
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let rows = table
        .rows()
        .filter_map(|(_, chain)| visible_version(chain, snapshot, xid))
        .map(|version| {
            indexes
                .iter()
                .map(|index| version.row[*index].clone())
                .collect()
        })
        .collect();
    Ok(ExecutionResult::Query(QueryResult { columns, rows }))
}

fn value(expr: &Expr, base: BaseType) -> Result<Value> {
    let value = match expr {
        Expr::Value(AstValue::Null) => Value::Null,
        Expr::Value(AstValue::Boolean(value)) => Value::Bool(*value),
        Expr::Value(AstValue::SingleQuotedString(value)) => Value::Text(value.clone()),
        Expr::Value(AstValue::Number(value, _)) if value.contains(['.', 'e', 'E']) => {
            Value::parse(BaseType::Numeric, value)?
        }
        Expr::Value(AstValue::Number(value, _)) => Value::parse(BaseType::Int4, value)?,
        Expr::Value(_) => {
            return Err(PgError::new(
                SqlState::CannotCoerce,
                "literal has incompatible type",
            ));
        }
        _ => {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "INSERT expressions are not implemented",
            ));
        }
    };
    if value.is_null()
        || value.base_type() == Some(base)
        || matches!(
            (value.base_type(), base),
            (Some(BaseType::Text), BaseType::Varchar | BaseType::Bpchar)
        )
    {
        Ok(value)
    } else {
        Err(PgError::new(
            SqlState::DatatypeMismatch,
            "column has incompatible type",
        ))
    }
}
