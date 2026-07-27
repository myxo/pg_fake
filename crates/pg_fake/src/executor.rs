use crate::{
    api::{ColumnMeta, QueryResult},
    catalog::{Catalog, ColumnDef, TableId, TableSchema},
    error::{PgError, Result, SqlState},
    storage::Table,
    txn::{Snapshot, TransactionManager, Xid, visible_version},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast::{
    BinaryOperator, ColumnOption, Expr, GroupByExpr, Ident, ObjectType, SelectItem, SetExpr,
    Statement, TableConstraint, TableFactor, UnaryOperator, Value as AstValue,
};
use std::{cmp::Ordering, collections::BTreeMap};

pub(crate) struct DatabaseState {
    pub catalog: Catalog,
    pub tables: BTreeMap<TableId, Table>,
    pub transactions: TransactionManager,
}
pub(crate) enum ExecutionResult {
    Affected(u64),
    Query(QueryResult),
}
enum Projection<'a> {
    Column(usize),
    Expression(&'a Expr),
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
    let table = state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage");
    let rows = table
        .rows()
        .filter_map(|(_, chain)| visible_version(chain, snapshot, xid))
        .try_fold(Vec::new(), |mut rows, version| -> Result<Vec<Vec<Value>>> {
            if let Some(selection) = &select.selection {
                match evaluate(selection, schema, &version.row)? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(rows),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            rows.push(
                projections
                    .iter()
                    .map(|projection| match projection {
                        Projection::Column(index) => Ok(version.row[*index].clone()),
                        Projection::Expression(expr) => evaluate(expr, schema, &version.row),
                    })
                    .collect::<Result<Vec<_>>>()?,
            );
            Ok(rows)
        })?;
    Ok(ExecutionResult::Query(QueryResult { columns, rows }))
}

fn value(expr: &Expr, base: BaseType) -> Result<Value> {
    let value = literal_value(expr)?;
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

fn literal_value(expr: &Expr) -> Result<Value> {
    match expr {
        Expr::Value(AstValue::Null) => Ok(Value::Null),
        Expr::Value(AstValue::Boolean(value)) => Ok(Value::Bool(*value)),
        Expr::Value(AstValue::SingleQuotedString(value)) => Ok(Value::Text(value.clone())),
        Expr::Value(AstValue::Number(value, _)) if value.contains(['.', 'e', 'E']) => {
            Value::parse(BaseType::Numeric, value)
        }
        Expr::Value(AstValue::Number(value, _)) => Value::parse(BaseType::Int4, value),
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

fn expression_type(expr: &Expr, schema: &TableSchema) -> Result<BaseType> {
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
        Expr::Value(AstValue::Number(_, _)) => Ok(BaseType::Int4),
        Expr::Value(_) => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "literal is not implemented",
        )),
        Expr::Nested(expr) => expression_type(expr, schema),
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
        Expr::BinaryOp { left, op, right } => {
            let left_base = expression_type(left, schema)?;
            let right_base = expression_type(right, schema)?;
            match op {
                BinaryOperator::Plus
                | BinaryOperator::Minus
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Modulo
                    if (numeric(left_base) && left_base == right_base)
                        || (null_expression(left) && numeric(right_base))
                        || (numeric(left_base) && null_expression(right)) =>
                {
                    if null_expression(left) {
                        Ok(right_base)
                    } else {
                        Ok(left_base)
                    }
                }
                BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Gt
                | BinaryOperator::Lt
                | BinaryOperator::GtEq
                | BinaryOperator::LtEq
                    if comparable(left_base, right_base)
                        || null_expression(left)
                        || null_expression(right) =>
                {
                    Ok(BaseType::Bool)
                }
                BinaryOperator::And | BinaryOperator::Or
                    if (left_base == BaseType::Bool || null_expression(left))
                        && (right_base == BaseType::Bool || null_expression(right)) =>
                {
                    Ok(BaseType::Bool)
                }
                _ => Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "operator has incompatible types",
                )),
            }
        }
        Expr::IsNull(_) | Expr::IsNotNull(_) => Ok(BaseType::Bool),
        Expr::IsTrue(expr) | Expr::IsFalse(expr) | Expr::IsUnknown(expr) => {
            let base = expression_type(expr, schema)?;
            if base == BaseType::Bool || null_expression(expr) {
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
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "expression is not implemented",
        )),
    }
}

fn null_expression(expr: &Expr) -> bool {
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
    left == right
        || matches!(
            (left, right),
            (
                BaseType::Text | BaseType::Varchar | BaseType::Bpchar,
                BaseType::Text | BaseType::Varchar | BaseType::Bpchar
            )
        )
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
                return Value::parse(BaseType::Int4, &format!("-{value}"));
            }
            unary(*op, evaluate(expr, schema, row)?)
        }
        Expr::BinaryOp { left, op, right } => {
            let left = evaluate(left, schema, row)?;
            let right = evaluate(right, schema, row)?;
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
            evaluate(expr, schema, row)?,
            Value::Bool(true)
        ))),
        Expr::IsFalse(expr) => Ok(Value::Bool(matches!(
            evaluate(expr, schema, row)?,
            Value::Bool(false)
        ))),
        Expr::IsUnknown(expr) => Ok(Value::Bool(evaluate(expr, schema, row)?.is_null())),
        Expr::IsDistinctFrom(left, right) => distinct(
            evaluate(left, schema, row)?,
            evaluate(right, schema, row)?,
            false,
        ),
        Expr::IsNotDistinctFrom(left, right) => distinct(
            evaluate(left, schema, row)?,
            evaluate(right, schema, row)?,
            true,
        ),
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "expression is not implemented",
        )),
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
    let ordering = match (left, right) {
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
    };
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
