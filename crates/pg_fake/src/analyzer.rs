//! Parameter analysis and binding for prepared statements.

use std::ops::ControlFlow;

use sqlparser::ast::{
    AssignmentTarget, BinaryOperator, CastKind, DataType, Expr, FromTable, FunctionArg,
    FunctionArgExpr, FunctionArguments, LimitClause, OrderByKind, SelectItem, SetExpr, Statement,
    TableFactor, Value as AstValue, VisitMut, VisitorMut, visit_expressions, visit_expressions_mut,
};

use crate::{
    catalog::{Catalog, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    executor,
    value::{BaseType, PgType, Value},
};

pub(crate) fn parameter_types(statement: &Statement, catalog: &Catalog) -> Result<Vec<BaseType>> {
    let mut types = vec![None; parameter_count(statement)?];
    match statement {
        Statement::Insert(insert) => {
            let schema = catalog.table(&executor::insert_table_name(&insert.table)?)?;
            let columns = if insert.columns.is_empty() {
                (0..schema.columns.len()).collect::<Vec<_>>()
            } else {
                insert
                    .columns
                    .iter()
                    .map(|name| {
                        let name = executor::name(name)?;
                        schema
                            .columns
                            .iter()
                            .position(|column| column.name == name)
                            .ok_or_else(|| {
                                PgError::new(
                                    SqlState::UndefinedColumn,
                                    format!("column {:?} does not exist", name),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            if let Some(source) = &insert.source
                && let SetExpr::Values(values) = source.body.as_ref()
            {
                for row in &values.rows {
                    if row.len() != columns.len() {
                        return Err(PgError::new(
                            SqlState::SyntaxError,
                            "INSERT has wrong number of values",
                        ));
                    }
                    for (expression, column) in row.iter().zip(&columns) {
                        infer_expr(
                            expression,
                            executor::RowScope::Table(&executor::constant_schema()),
                            Some(schema.columns[*column].data_type.base),
                            &mut types,
                        )?;
                    }
                }
            }
        }
        Statement::Update(update) => {
            let schema = table_schema(&update.table.relation, catalog)?;
            for assignment in &update.assignments {
                let AssignmentTarget::ColumnName(name) = &assignment.target else {
                    continue;
                };
                let name = executor::name(name)?;
                let column = schema
                    .columns
                    .iter()
                    .find(|column| column.name == name)
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {name:?} does not exist"),
                        )
                    })?;
                infer_expr(
                    &assignment.value,
                    executor::RowScope::Table(schema),
                    Some(column.data_type.base),
                    &mut types,
                )?;
            }
            if let Some(selection) = &update.selection {
                infer_expr(
                    selection,
                    executor::RowScope::Table(schema),
                    Some(BaseType::Bool),
                    &mut types,
                )?;
            }
        }
        Statement::Delete(delete) => {
            let FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(finalize(types));
            };
            if let Some(first) = from.first() {
                let schema = table_schema(&first.relation, catalog)?;
                if let Some(selection) = &delete.selection {
                    infer_expr(
                        selection,
                        executor::RowScope::Table(schema),
                        Some(BaseType::Bool),
                        &mut types,
                    )?;
                }
            }
        }
        Statement::Query(query) => {
            let SetExpr::Select(select) = query.body.as_ref() else {
                return Ok(finalize(types));
            };
            let bound = executor::bind_query_scope(catalog, select)?;
            let schema = executor::RowScope::Bound(&bound);
            if let Some(selection) = &select.selection {
                infer_expr(selection, schema, Some(BaseType::Bool), &mut types)?;
            }
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(expression)
                    | SelectItem::ExprWithAlias {
                        expr: expression, ..
                    } => {
                        infer_expr(expression, schema, None, &mut types)?;
                    }
                    _ => {}
                }
            }
            if let Some(order_by) = &query.order_by
                && let OrderByKind::Expressions(orders) = &order_by.kind
            {
                for order in orders {
                    if !projection_alias(&order.expr, &select.projection) {
                        infer_expr(&order.expr, schema, None, &mut types)?;
                    }
                }
            }
            if let Some(LimitClause::LimitOffset { limit, offset, .. }) = &query.limit_clause {
                if let Some(limit) = limit {
                    infer_expr(limit, schema, Some(BaseType::Int8), &mut types)?;
                }
                if let Some(offset) = offset {
                    infer_expr(&offset.value, schema, Some(BaseType::Int8), &mut types)?;
                }
            }
        }
        _ => {}
    }
    let types = finalize(types);
    let bound = bind(statement, &types, &vec![Value::Null; types.len()])?;
    validate_statement(&bound, catalog)?;
    Ok(types)
}

pub(crate) fn describe_subqueries(statement: &Statement, catalog: &Catalog) -> Result<Statement> {
    let mut statement = statement.clone();
    if let Statement::Query(query) = &statement
        && let SetExpr::Select(select) = query.body.as_ref()
    {
        let outer = executor::bind_query_scope(catalog, select)?;
        let mut describer = ScopedSubqueryDescriber {
            catalog,
            outer: &outer,
            error: None,
        };
        let _ = statement.visit(&mut describer);
        return describer.error.map_or(Ok(statement), Err);
    }
    let mut error = None;
    let _ = visit_expressions_mut(&mut statement, |expression| {
        if error.is_some() {
            return ControlFlow::Break(());
        }
        let result = match expression {
            Expr::Subquery(query) => {
                executor::query_output_columns(catalog, query).and_then(|columns| {
                    if columns.len() != 1 {
                        return Err(PgError::new(
                            SqlState::SyntaxError,
                            "subquery must return only one column",
                        ));
                    }
                    Ok(typed_literal(Value::Null, columns[0].1.base))
                })
            }
            Expr::Exists { .. } => Ok(typed_literal(Value::Bool(false), BaseType::Bool)),
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => executor::query_output_columns(catalog, subquery).and_then(|columns| {
                let left_width = match expr.as_ref() {
                    Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if columns.len() != left_width {
                    return Err(PgError::new(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let fields = columns
                    .into_iter()
                    .map(|(_, data_type)| typed_literal(Value::Null, data_type.base))
                    .collect::<Vec<_>>();
                Ok(Expr::InList {
                    expr: expr.clone(),
                    list: vec![if fields.len() == 1 {
                        fields.into_iter().next().expect("subquery has one column")
                    } else {
                        Expr::Tuple(fields)
                    }],
                    negated: *negated,
                })
            }),
            _ => return ControlFlow::Continue(()),
        };
        match result {
            Ok(value) => *expression = value,
            Err(describe_error) => error = Some(describe_error),
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(statement), Err)
}

struct ScopedSubqueryDescriber<'a> {
    catalog: &'a Catalog,
    outer: &'a executor::BoundScope,
    error: Option<PgError>,
}

impl VisitorMut for ScopedSubqueryDescriber<'_> {
    type Break = ();

    fn pre_visit_expr(&mut self, expression: &mut Expr) -> ControlFlow<Self::Break> {
        if self.error.is_some() {
            return ControlFlow::Break(());
        }
        if !matches!(
            expression,
            Expr::Subquery(_)
                | Expr::Exists { .. }
                | Expr::InSubquery { .. }
                | Expr::AnyOp { .. }
                | Expr::AllOp { .. }
        ) {
            return ControlFlow::Continue(());
        }
        match executor::describe_expression_subqueries(self.catalog, expression, self.outer) {
            Ok(described) => *expression = described,
            Err(error) => {
                self.error = Some(error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

pub(crate) fn bind(
    statement: &Statement,
    parameter_types: &[BaseType],
    values: &[Value],
) -> Result<Statement> {
    if values.len() != parameter_types.len() {
        return Err(PgError::new(
            SqlState::ProtocolViolation,
            format!(
                "bind message supplies {} parameters, but prepared statement requires {}",
                values.len(),
                parameter_types.len()
            ),
        ));
    }
    let mut statement = statement.clone();
    let mut error = None;
    let _ = visit_expressions_mut(&mut statement, |expression| {
        let Expr::Value(value) = expression else {
            return ControlFlow::Continue(());
        };
        let AstValue::Placeholder(placeholder) = &value.value else {
            return ControlFlow::Continue(());
        };
        let index = match placeholder_index(placeholder) {
            Ok(index) => index,
            Err(bind_error) => {
                error = Some(bind_error);
                return ControlFlow::Break(());
            }
        };
        let target = parameter_types[index];
        match coerce_parameter(values[index].clone(), target) {
            Ok(value) => *expression = typed_literal(value, target),
            Err(bind_error) => {
                error = Some(bind_error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(statement), Err)
}

fn parameter_count(statement: &Statement) -> Result<usize> {
    let mut maximum = 0;
    let mut error = None;
    let _ = visit_expressions(statement, |expression| {
        let Expr::Value(value) = expression else {
            return ControlFlow::Continue(());
        };
        let AstValue::Placeholder(placeholder) = &value.value else {
            return ControlFlow::Continue(());
        };
        match placeholder_index(placeholder) {
            Ok(index) => maximum = maximum.max(index + 1),
            Err(parameter_error) => {
                error = Some(parameter_error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(maximum), Err)
}

fn placeholder_index(placeholder: &str) -> Result<usize> {
    let index = placeholder
        .strip_prefix('$')
        .and_then(|index| index.parse::<usize>().ok())
        .filter(|index| *index > 0)
        .ok_or_else(|| {
            PgError::new(
                SqlState::UndefinedParameter,
                format!("there is no parameter {placeholder}"),
            )
        })?;
    Ok(index - 1)
}

fn table_schema<'a>(factor: &TableFactor, catalog: &'a Catalog) -> Result<&'a TableSchema> {
    let TableFactor::Table {
        name, args: None, ..
    } = factor
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "table source is not implemented",
        ));
    };
    catalog.table(&executor::name(name)?)
}

fn infer_expr(
    expression: &Expr,
    schema: executor::RowScope<'_>,
    expected: Option<BaseType>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    constrain(expression, expected, types)?;
    let mut error = None;
    let _ = visit_expressions(expression, |expression| {
        let result = match expression {
            Expr::Identifier(identifier)
                if identifier.quote_style.is_none()
                    && identifier.value.eq_ignore_ascii_case("default") =>
            {
                Ok(())
            }
            Expr::Identifier(_) => executor::expression_type(expression, schema).map(|_| ()),
            Expr::Nested(inner) => constrain(inner, expected, types),
            Expr::Cast {
                expr, data_type, ..
            } => coercion::type_from_ast(data_type)
                .and_then(|target| constrain(expr, Some(target.base), types)),
            Expr::UnaryOp { op, expr } => constrain(
                expr,
                matches!(op, sqlparser::ast::UnaryOperator::Not).then_some(BaseType::Bool),
                types,
            ),
            Expr::BinaryOp { left, op, right } => {
                let boolean = matches!(op, BinaryOperator::And | BinaryOperator::Or);
                let left_expected = if boolean {
                    Some(BaseType::Bool)
                } else {
                    executor::expression_type(right, schema).ok()
                };
                let right_expected = if boolean {
                    Some(BaseType::Bool)
                } else {
                    executor::expression_type(left, schema).ok()
                };
                constrain(left, left_expected, types)
                    .and_then(|()| constrain(right, right_expected, types))
            }
            Expr::InList { expr, list, .. } => (|| {
                let left = match expr.as_ref() {
                    Expr::Tuple(fields) => fields.as_slice(),
                    expr => std::slice::from_ref(expr),
                };
                for candidate in list {
                    let right = match candidate {
                        Expr::Tuple(fields) => fields.as_slice(),
                        candidate => std::slice::from_ref(candidate),
                    };
                    if left.len() != right.len() {
                        return Err(PgError::new(
                            SqlState::SyntaxError,
                            "subquery has too many columns",
                        ));
                    }
                    for (left, right) in left.iter().zip(right) {
                        constrain(left, executor::expression_type(right, schema).ok(), types)?;
                        constrain(right, executor::expression_type(left, schema).ok(), types)?;
                    }
                }
                Ok(())
            })(),
            Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
                constrain(left, executor::expression_type(right, schema).ok(), types).and_then(
                    |()| constrain(right, executor::expression_type(left, schema).ok(), types),
                )
            }
            Expr::IsTrue(inner)
            | Expr::IsFalse(inner)
            | Expr::IsUnknown(inner)
            | Expr::IsNotTrue(inner)
            | Expr::IsNotFalse(inner)
            | Expr::IsNotUnknown(inner) => constrain(inner, Some(BaseType::Bool), types),
            Expr::Function(function) => infer_function(function, schema, types),
            _ => Ok(()),
        };
        if let Err(infer_error) = result {
            error = Some(infer_error);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    error.map_or(Ok(()), Err)
}

fn infer_function(
    function: &sqlparser::ast::Function,
    schema: executor::RowScope<'_>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let FunctionArguments::List(list) = &function.args else {
        return Ok(());
    };
    let arguments = list
        .args
        .iter()
        .filter_map(|argument| match argument {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => Some(expression),
            _ => None,
        })
        .collect::<Vec<_>>();
    let name = executor::name(&function.name)?;
    let expected = match name.as_str() {
        "length" | "lower" | "upper" => Some(BaseType::Text),
        _ => arguments
            .iter()
            .find_map(|argument| executor::expression_type(argument, schema).ok()),
    };
    for argument in arguments {
        constrain(argument, expected, types)?;
    }
    Ok(())
}

fn constrain(
    expression: &Expr,
    expected: Option<BaseType>,
    types: &mut [Option<BaseType>],
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let expression = match expression {
        Expr::Nested(inner) => inner.as_ref(),
        expression => expression,
    };
    let Expr::Value(value) = expression else {
        return Ok(());
    };
    let AstValue::Placeholder(placeholder) = &value.value else {
        return Ok(());
    };
    let index = placeholder_index(placeholder)?;
    let slot = &mut types[index];
    if let Some(previous) = *slot
        && previous != expected
    {
        let Some(common) = coercion::common_type(previous, expected) else {
            return Err(PgError::new(
                SqlState::AmbiguousParameter,
                format!("inconsistent types deduced for parameter {placeholder}"),
            ));
        };
        *slot = Some(common);
    } else {
        *slot = Some(expected);
    }
    Ok(())
}

fn finalize(types: Vec<Option<BaseType>>) -> Vec<BaseType> {
    types
        .into_iter()
        .map(|data_type| data_type.unwrap_or(BaseType::Text))
        .collect()
}

fn validate_statement(statement: &Statement, catalog: &Catalog) -> Result<()> {
    match statement {
        Statement::Insert(insert) => {
            let schema = catalog.table(&executor::insert_table_name(&insert.table)?)?;
            let columns = if insert.columns.is_empty() {
                (0..schema.columns.len()).collect::<Vec<_>>()
            } else {
                insert
                    .columns
                    .iter()
                    .map(|name| {
                        let name = executor::name(name)?;
                        schema
                            .columns
                            .iter()
                            .position(|column| column.name == name)
                            .ok_or_else(|| {
                                PgError::new(
                                    SqlState::UndefinedColumn,
                                    format!("column {:?} does not exist", name),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            if let Some(source) = &insert.source
                && let SetExpr::Values(values) = source.body.as_ref()
            {
                for row in &values.rows {
                    for (expression, column) in row.iter().zip(&columns) {
                        validate_assignment(
                            expression,
                            schema.columns[*column].data_type,
                            executor::RowScope::Table(&executor::constant_schema()),
                        )?;
                    }
                }
            }
        }
        Statement::Update(update) => {
            let schema = table_schema(&update.table.relation, catalog)?;
            if let Some(selection) = &update.selection {
                validate_boolean(
                    selection,
                    executor::RowScope::Table(schema),
                    "WHERE requires a boolean expression",
                )?;
            }
            for assignment in &update.assignments {
                let AssignmentTarget::ColumnName(name) = &assignment.target else {
                    continue;
                };
                let name = executor::name(name)?;
                let column = schema
                    .columns
                    .iter()
                    .find(|column| column.name == name)
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {name:?} does not exist"),
                        )
                    })?;
                validate_assignment(
                    &assignment.value,
                    column.data_type,
                    executor::RowScope::Table(schema),
                )?;
            }
        }
        Statement::Delete(delete) => {
            let FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(());
            };
            if let Some(first) = from.first()
                && let Some(selection) = &delete.selection
            {
                validate_boolean(
                    selection,
                    executor::RowScope::Table(table_schema(&first.relation, catalog)?),
                    "WHERE requires a boolean expression",
                )?;
            }
        }
        Statement::Query(query) => {
            let SetExpr::Select(select) = query.body.as_ref() else {
                return Ok(());
            };
            let bound = executor::bind_query_scope(catalog, select)?;
            let schema = executor::RowScope::Bound(&bound);
            if let Some(selection) = &select.selection {
                validate_boolean(selection, schema, "WHERE requires a boolean expression")?;
            }
            for item in &select.projection {
                match item {
                    SelectItem::UnnamedExpr(expression)
                    | SelectItem::ExprWithAlias {
                        expr: expression, ..
                    } => {
                        executor::expression_type(expression, schema)?;
                    }
                    _ => {}
                }
            }
            if let Some(order_by) = &query.order_by
                && let OrderByKind::Expressions(orders) = &order_by.kind
            {
                for order in orders {
                    if !projection_alias(&order.expr, &select.projection) {
                        executor::expression_type(&order.expr, schema)?;
                    }
                }
            }
            if let Some(LimitClause::LimitOffset { limit, offset, .. }) = &query.limit_clause {
                if let Some(limit) = limit
                    && !matches!(limit, Expr::Identifier(name) if name.value.eq_ignore_ascii_case("all"))
                {
                    validate_implicit_type(limit, BaseType::Int8, schema)?;
                }
                if let Some(offset) = offset {
                    validate_implicit_type(&offset.value, BaseType::Int8, schema)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn projection_alias(expression: &Expr, projection: &[SelectItem]) -> bool {
    let Expr::Identifier(identifier) = expression else {
        return false;
    };
    projection.iter().any(|item| {
        matches!(item, SelectItem::ExprWithAlias { alias, .. }
            if executor::identifier_name(alias) == executor::identifier_name(identifier))
    })
}

fn validate_boolean(
    expression: &Expr,
    schema: executor::RowScope<'_>,
    message: &str,
) -> Result<()> {
    let data_type = executor::expression_type(expression, schema)?;
    if data_type == BaseType::Bool || executor::null_expression(expression) {
        Ok(())
    } else {
        Err(PgError::new(SqlState::DatatypeMismatch, message))
    }
}

fn validate_assignment(
    expression: &Expr,
    target: PgType,
    schema: executor::RowScope<'_>,
) -> Result<()> {
    if matches!(expression, Expr::Identifier(name) if name.value.eq_ignore_ascii_case("default"))
        || matches!(expression, Expr::Value(value) if matches!(&value.value, AstValue::SingleQuotedString(_)))
        || executor::null_expression(expression)
    {
        return Ok(());
    }
    let source = executor::expression_type(expression, schema)?;
    if coercion::can_cast(source, target.base, CastContext::Assignment) {
        Ok(())
    } else {
        Err(PgError::new(
            SqlState::DatatypeMismatch,
            "column has incompatible type",
        ))
    }
}

fn validate_implicit_type(
    expression: &Expr,
    target: BaseType,
    schema: executor::RowScope<'_>,
) -> Result<()> {
    if executor::null_expression(expression)
        || matches!(expression, Expr::Value(value) if matches!(&value.value, AstValue::SingleQuotedString(_)))
    {
        return Ok(());
    }
    let source = executor::expression_type(expression, schema)?;
    if coercion::can_cast(source, target, CastContext::Implicit) {
        Ok(())
    } else {
        Err(PgError::new(
            SqlState::DatatypeMismatch,
            "parameter has incompatible type",
        ))
    }
}

fn coerce_parameter(value: Value, target: BaseType) -> Result<Value> {
    let Some(source) = value.base_type() else {
        return Ok(Value::Null);
    };
    coercion::coerce(value, source, PgType::new(target), CastContext::Implicit)
}

pub(crate) fn typed_literal(value: Value, data_type: BaseType) -> Expr {
    let literal = match value {
        Value::Null => AstValue::Null,
        Value::Bool(value) => AstValue::Boolean(value),
        Value::Int2(value) => AstValue::Number(value.to_string(), false),
        Value::Int4(value) => AstValue::Number(value.to_string(), false),
        Value::Int8(value) => AstValue::Number(value.to_string(), false),
        Value::Float4(value) => AstValue::SingleQuotedString(Value::Float4(value).to_text()),
        Value::Float8(value) => AstValue::SingleQuotedString(Value::Float8(value).to_text()),
        Value::Numeric(value) => AstValue::Number(value.to_plain_string(), false),
        Value::Text(value) => AstValue::SingleQuotedString(value),
        Value::Bytea(value) => AstValue::SingleQuotedString(Value::Bytea(value).to_text()),
        Value::Uuid(value) => AstValue::SingleQuotedString(value.to_string()),
        Value::Date(value) => AstValue::SingleQuotedString(Value::Date(value).to_text()),
        Value::Time(value) => AstValue::SingleQuotedString(Value::Time(value).to_text()),
        Value::Timestamp(value) => AstValue::SingleQuotedString(Value::Timestamp(value).to_text()),
        Value::TimestampTz(value) => {
            AstValue::SingleQuotedString(Value::TimestampTz(value).to_text())
        }
        Value::Interval(value) => AstValue::SingleQuotedString(Value::Interval(value).to_text()),
    };
    Expr::Cast {
        kind: CastKind::Cast,
        expr: Box::new(Expr::Value(literal.into())),
        data_type: ast_data_type(data_type),
        array: false,
        format: None,
    }
}

fn ast_data_type(data_type: BaseType) -> DataType {
    match data_type {
        BaseType::Bool => DataType::Boolean,
        BaseType::Int2 => DataType::SmallInt(None),
        BaseType::Int4 => DataType::Integer(None),
        BaseType::Int8 => DataType::BigInt(None),
        BaseType::Float4 => DataType::Real,
        BaseType::Float8 => DataType::DoublePrecision,
        BaseType::Numeric => DataType::Numeric(sqlparser::ast::ExactNumberInfo::None),
        BaseType::Text => DataType::Text,
        BaseType::Varchar => DataType::Varchar(None),
        BaseType::Bpchar => DataType::Char(None),
        BaseType::Bytea => DataType::Bytea,
        BaseType::Uuid => DataType::Uuid,
        BaseType::Date => DataType::Date,
        BaseType::Time => DataType::Time(None, sqlparser::ast::TimezoneInfo::WithoutTimeZone),
        BaseType::Timestamp => {
            DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::WithoutTimeZone)
        }
        BaseType::TimestampTz => {
            DataType::Timestamp(None, sqlparser::ast::TimezoneInfo::WithTimeZone)
        }
        BaseType::Interval => DataType::Interval {
            fields: None,
            precision: None,
        },
    }
}
