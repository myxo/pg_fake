use super::*;
use sqlparser::ast;

pub(super) fn evaluate_assignment_expression(
    expr: &ast::Expr,
    target: PgType,
    schema: &TableSchema,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expr) {
        coercion::coerce_unknown(text, target, CastContext::Assignment)
    } else {
        coercion::coerce(
            evaluate(expr, RowScope::Table(schema), row, context)?,
            infer_expression_type(expr, RowScope::Table(schema))?,
            target,
            CastContext::Assignment,
        )
    }
}

pub(super) fn evaluate_column_default(
    column: &ColumnDef,
    context: &StatementExecutionContext,
) -> Result<Value> {
    if let Some(sequence) = &column.default_sequence {
        let value = context.sequences.get_next_value(sequence)?;
        return coercion::coerce(
            Value::Int8(value),
            BaseType::Int8,
            column.data_type,
            CastContext::Assignment,
        );
    }
    let Some(expr) = &column.default else {
        return Ok(Value::Null);
    };
    evaluate_assignment_expression(
        expr,
        column.data_type,
        &create_constant_expression_schema(),
        &[],
        context,
    )
    .map_err(|error| {
        if error.sqlstate == SqlState::UndefinedColumn {
            PgError::create(
                SqlState::FeatureNotSupported,
                "cannot use column reference in DEFAULT expression",
            )
        } else {
            error
        }
    })
}

pub(super) fn validate_column_default(column: &ColumnDef) -> Result<()> {
    let Some(expression) = &column.default else {
        return Ok(());
    };
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, column.data_type, CastContext::Assignment)?;
        return Ok(());
    }
    let source = infer_expression_type(
        expression,
        RowScope::Table(&create_constant_expression_schema()),
    )
    .map_err(|error| {
        if error.sqlstate == SqlState::UndefinedColumn {
            PgError::create(
                SqlState::FeatureNotSupported,
                "cannot use column reference in DEFAULT expression",
            )
        } else {
            error
        }
    })?;
    if coercion::can_cast(source, column.data_type.base, CastContext::Assignment) {
        Ok(())
    } else {
        Err(PgError::create(
            SqlState::DatatypeMismatch,
            "default expression has incompatible type",
        ))
    }
}

pub(super) fn validate_not_null(schema: &TableSchema, row: &[Value]) -> Result<()> {
    if let Some(column) = schema
        .columns
        .iter()
        .zip(row)
        .find_map(|(column, value)| (!column.nullable && value.is_null()).then_some(column))
    {
        return Err(PgError::create(
            SqlState::NotNullViolation,
            format!(
                "null value in column {:?} of relation {:?} violates not-null constraint",
                column.name, schema.name
            ),
        ));
    }
    Ok(())
}

pub(super) fn validate_check_constraint_types(schema: &TableSchema) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check(expression) = constraint else {
            continue;
        };
        let base = infer_expression_type(expression, RowScope::Table(schema))?;
        if base != BaseType::Bool
            && !is_null_literal(expression)
            && extract_unknown_string_literal(expression).is_none()
        {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "CHECK constraint must be a boolean expression",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_check_constraints(
    schema: &TableSchema,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check(expression) = constraint else {
            continue;
        };
        match evaluate_and_coerce(
            expression,
            BaseType::Bool,
            CastContext::Implicit,
            RowScope::Table(schema),
            row,
            context,
        )? {
            Value::Bool(true) | Value::Null => {}
            Value::Bool(false) => {
                return Err(PgError::create(
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

pub(super) fn is_default_expression(expr: &ast::Expr) -> bool {
    matches!(expr, ast::Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("default"))
}

pub(crate) fn create_constant_expression_schema() -> TableSchema {
    TableSchema {
        id: TableId(0),
        name: String::new(),
        columns: Vec::new(),
        constraints: Vec::new(),
    }
}

fn extract_ast_value(expr: &ast::Expr) -> Option<&ast::Value> {
    let ast::Expr::Value(value) = expr else {
        return None;
    };
    Some(&value.value)
}

pub(super) fn extract_number_literal(expr: &ast::Expr) -> Option<&str> {
    let ast::Value::Number(value, _) = extract_ast_value(expr)? else {
        return None;
    };
    Some(value)
}

fn evaluate_literal(expr: &ast::Expr) -> Result<Value> {
    if let Some(value) = extract_ast_value(expr) {
        return match value {
            ast::Value::Null => Ok(Value::Null),
            ast::Value::Boolean(value) => Ok(Value::Bool(*value)),
            ast::Value::SingleQuotedString(value) => Ok(Value::Text(value.clone())),
            ast::Value::Number(value, _) if value.contains(['.', 'e', 'E']) => {
                Value::parse(BaseType::Numeric, value)
            }
            ast::Value::Number(value, _) => parse_integer_literal(value),
            _ => Err(PgError::create(
                SqlState::CannotCoerce,
                "literal has incompatible type",
            )),
        };
    }
    match expr {
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Plus,
            expr,
        } => evaluate_literal(expr),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } if extract_number_literal(expr).is_some_and(|value| !value.contains(['.', 'e', 'E'])) => {
            let value = extract_number_literal(expr).expect("integer literal pattern was checked");
            parse_integer_literal(&format!("-{value}"))
        }
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } if extract_number_literal(expr).is_some() => {
            evaluate_unary_operator(ast::UnaryOperator::Minus, evaluate_literal(expr)?)
        }
        ast::Expr::Nested(expr) => evaluate_literal(expr),
        _ => reject_unsupported("expression is not implemented"),
    }
}

fn parse_integer_literal(value: &str) -> Result<Value> {
    if let Ok(value) = value.parse::<i32>() {
        return Ok(Value::Int4(value));
    }
    if let Ok(value) = value.parse::<i64>() {
        return Ok(Value::Int8(value));
    }
    Value::parse(BaseType::Numeric, value)
}

pub(crate) fn extract_unknown_string_literal(expr: &ast::Expr) -> Option<&str> {
    match expr {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        ast::Expr::Nested(expr) => extract_unknown_string_literal(expr),
        _ => None,
    }
}

pub(crate) fn infer_expression_type(expr: &ast::Expr, schema: RowScope<'_>) -> Result<BaseType> {
    if let Some(value) = extract_ast_value(expr) {
        return match value {
            ast::Value::Null => Ok(BaseType::Text),
            ast::Value::Boolean(_) => Ok(BaseType::Bool),
            ast::Value::SingleQuotedString(_) => Ok(BaseType::Text),
            ast::Value::Number(value, _) if value.contains(['.', 'e', 'E']) => {
                Ok(BaseType::Numeric)
            }
            ast::Value::Number(value, _) => Ok(parse_integer_literal(value)?
                .get_base_type()
                .expect("numeric literal is not null")),
            _ => reject_unsupported("literal is not implemented"),
        };
    }
    match expr {
        ast::Expr::Identifier(column) => {
            Ok(schema.resolve_column(std::slice::from_ref(column))?.1.base)
        }
        ast::Expr::CompoundIdentifier(columns) => Ok(schema.resolve_column(columns)?.1.base),
        ast::Expr::Nested(expr) => infer_expression_type(expr, schema),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } if extract_number_literal(expr).is_some_and(|value| !value.contains(['.', 'e', 'E'])) => {
            let value = extract_number_literal(expr).expect("integer literal pattern was checked");
            Ok(parse_integer_literal(&format!("-{value}"))?
                .get_base_type()
                .expect("integer literal is not null"))
        }
        ast::Expr::UnaryOp { op, expr } => {
            let base = infer_expression_type(expr, schema)?;
            if matches!(op, ast::UnaryOperator::Plus | ast::UnaryOperator::Minus)
                && is_numeric_type(base)
            {
                Ok(base)
            } else if matches!(op, ast::UnaryOperator::Not)
                && (base == BaseType::Bool || is_null_literal(expr))
            {
                Ok(BaseType::Bool)
            } else {
                Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "operator has incompatible type",
                ))
            }
        }
        ast::Expr::BinaryOp { left, op, right } => match op {
            ast::BinaryOperator::Plus
            | ast::BinaryOperator::Minus
            | ast::BinaryOperator::Multiply
            | ast::BinaryOperator::Divide
            | ast::BinaryOperator::Modulo => {
                let left_type = infer_expression_type(left, schema)?;
                let right_type = infer_expression_type(right, schema)?;
                if matches!(left_type, BaseType::Interval)
                    || matches!(right_type, BaseType::Interval)
                {
                    return infer_interval_arithmetic_type(op, left_type, right_type);
                }
                let base = resolve_operator_type(left, right, schema)?;
                if is_numeric_type(base) {
                    Ok(base)
                } else {
                    Err(PgError::create(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ))
                }
            }
            ast::BinaryOperator::Eq
            | ast::BinaryOperator::NotEq
            | ast::BinaryOperator::Gt
            | ast::BinaryOperator::Lt
            | ast::BinaryOperator::GtEq
            | ast::BinaryOperator::LtEq => {
                resolve_operator_type(left, right, schema)?;
                Ok(BaseType::Bool)
            }
            ast::BinaryOperator::And | ast::BinaryOperator::Or => {
                let left_base = infer_expression_type(left, schema)?;
                let right_base = infer_expression_type(right, schema)?;
                if (left_base == BaseType::Bool
                    || is_null_literal(left)
                    || extract_unknown_string_literal(left).is_some())
                    && (right_base == BaseType::Bool
                        || is_null_literal(right)
                        || extract_unknown_string_literal(right).is_some())
                {
                    Ok(BaseType::Bool)
                } else {
                    Err(PgError::create(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ))
                }
            }
            _ => Err(PgError::create(
                SqlState::DatatypeMismatch,
                "operator has incompatible types",
            )),
        },
        ast::Expr::IsNull(_) | ast::Expr::IsNotNull(_) => Ok(BaseType::Bool),
        ast::Expr::InList { expr, list, .. } => {
            validate_membership_types(expr, list, schema)?;
            Ok(BaseType::Bool)
        }
        ast::Expr::InSubquery { .. }
        | ast::Expr::Exists { .. }
        | ast::Expr::AnyOp { .. }
        | ast::Expr::AllOp { .. } => Ok(BaseType::Bool),
        ast::Expr::IsTrue(expr) | ast::Expr::IsFalse(expr) | ast::Expr::IsUnknown(expr) => {
            let base = infer_expression_type(expr, schema)?;
            if base == BaseType::Bool
                || is_null_literal(expr)
                || extract_unknown_string_literal(expr).is_some()
            {
                Ok(BaseType::Bool)
            } else {
                Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "operator has incompatible type",
                ))
            }
        }
        ast::Expr::IsDistinctFrom(left, right) | ast::Expr::IsNotDistinctFrom(left, right) => {
            resolve_operator_type(left, right, schema)?;
            Ok(BaseType::Bool)
        }
        ast::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                for condition in conditions {
                    resolve_expression_pair_type(operand, &condition.condition, schema).map_err(
                        |_| {
                            PgError::create(
                                SqlState::DatatypeMismatch,
                                "CASE types are incompatible",
                            )
                        },
                    )?;
                }
            } else {
                for condition in conditions {
                    let base = infer_expression_type(&condition.condition, schema)?;
                    if base != BaseType::Bool && !is_null_literal(&condition.condition) {
                        return Err(PgError::create(
                            SqlState::DatatypeMismatch,
                            "CASE condition must be boolean",
                        ));
                    }
                }
            }
            resolve_expression_list_type(
                conditions
                    .iter()
                    .map(|condition| &condition.result)
                    .chain(else_result.as_deref())
                    .collect::<Vec<_>>()
                    .as_slice(),
                schema,
            )
        }
        ast::Expr::Function(function) => infer_function_return_type(function, schema),
        ast::Expr::Cast {
            kind,
            expr,
            data_type,
            format,
            ..
        } => {
            if !matches!(kind, ast::CastKind::Cast | ast::CastKind::DoubleColon) || format.is_some()
            {
                return reject_unsupported("cast variant is not implemented");
            }
            let target = coercion::convert_ast_data_type(data_type)?;
            if extract_unknown_string_literal(expr).is_none()
                && !is_null_literal(expr)
                && !coercion::can_cast(
                    infer_expression_type(expr, schema)?,
                    target.base,
                    CastContext::Explicit,
                )
            {
                return Err(PgError::create(
                    SqlState::CannotCoerce,
                    "types cannot be cast",
                ));
            }
            Ok(target.base)
        }
        ast::Expr::Extract { expr, .. } => {
            let base = infer_expression_type(expr, schema)?;
            if matches!(
                base,
                BaseType::Date | BaseType::Time | BaseType::Timestamp | BaseType::TimestampTz
            ) || is_null_literal(expr)
            {
                Ok(BaseType::Numeric)
            } else {
                Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "extract source must be a temporal value",
                ))
            }
        }
        _ => reject_unsupported("expression is not implemented"),
    }
}

pub(crate) fn is_null_literal(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Value(value) if matches!(&value.value, ast::Value::Null) => true,
        ast::Expr::Nested(expr) => is_null_literal(expr),
        _ => false,
    }
}

fn is_numeric_type(base: BaseType) -> bool {
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

fn resolve_expression_pair_type(
    left: &ast::Expr,
    right: &ast::Expr,
    schema: RowScope<'_>,
) -> Result<BaseType> {
    if is_null_literal(left) && is_null_literal(right)
        || extract_unknown_string_literal(left).is_some()
            && extract_unknown_string_literal(right).is_some()
    {
        return Ok(BaseType::Text);
    }
    if is_null_literal(left) || extract_unknown_string_literal(left).is_some() {
        return infer_expression_type(right, schema);
    }
    if is_null_literal(right) || extract_unknown_string_literal(right).is_some() {
        return infer_expression_type(left, schema);
    }
    coercion::resolve_common_type(
        infer_expression_type(left, schema)?,
        infer_expression_type(right, schema)?,
    )
    .ok_or_else(|| {
        PgError::create(
            SqlState::DatatypeMismatch,
            "expressions have incompatible types",
        )
    })
}

fn resolve_operator_type(
    left: &ast::Expr,
    right: &ast::Expr,
    schema: RowScope<'_>,
) -> Result<BaseType> {
    let left_type = infer_expression_type(left, schema)?;
    let right_type = infer_expression_type(right, schema)?;
    if left_type != right_type
        && (left_type == BaseType::Float4 || right_type == BaseType::Float4)
        && is_numeric_type(left_type)
        && is_numeric_type(right_type)
    {
        return Ok(BaseType::Float8);
    }
    let string = |data_type| {
        matches!(
            data_type,
            BaseType::Text | BaseType::Varchar | BaseType::Bpchar
        )
    };
    if string(left_type)
        && string(right_type)
        && (left_type == BaseType::Bpchar || right_type == BaseType::Bpchar)
    {
        Ok(BaseType::Text)
    } else {
        resolve_expression_pair_type(left, right, schema)
    }
}

fn resolve_expression_list_type(
    expressions: &[&ast::Expr],
    schema: RowScope<'_>,
) -> Result<BaseType> {
    let mut result = None;
    for expression in expressions {
        if is_null_literal(expression) || extract_unknown_string_literal(expression).is_some() {
            continue;
        }
        let base = infer_expression_type(expression, schema)?;
        result = Some(match result {
            None => base,
            Some(current) if current == base => current,
            Some(current) if coercion::resolve_common_type(current, base).is_some() => {
                coercion::resolve_common_type(current, base).expect("common type was checked")
            }
            Some(_) => {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "expressions have incompatible types",
                ));
            }
        });
    }
    Ok(result.unwrap_or(BaseType::Text))
}

fn extract_function_arguments(function: &ast::Function) -> Result<Vec<&ast::Expr>> {
    if function.uses_odbc_syntax
        || !matches!(function.parameters, ast::FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || function.over.is_some()
        || !function.within_group.is_empty()
    {
        return reject_unsupported("function feature is not implemented");
    }
    let ast::FunctionArguments::List(arguments) = &function.args else {
        return Err(PgError::create(
            SqlState::UndefinedFunction,
            "function signature does not exist",
        ));
    };
    if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
        return reject_unsupported("function argument feature is not implemented");
    }
    arguments
        .args
        .iter()
        .map(|argument| match argument {
            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => Ok(expression),
            _ => reject_unsupported("function argument is not implemented"),
        })
        .collect()
}

fn infer_function_return_type(function: &ast::Function, schema: RowScope<'_>) -> Result<BaseType> {
    if let Some(result) = infer_aggregate_return_type(function, schema)? {
        return Ok(result);
    }
    let function_name = normalize_unqualified_object_name(&function.name)?;
    let arguments = extract_function_arguments(function)?;
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )
    };
    match function_name.as_str() {
        "coalesce" | "greatest" | "least" if !arguments.is_empty() => {
            resolve_expression_list_type(&arguments, schema)
        }
        "nullif" if arguments.len() == 2 => resolve_expression_list_type(&arguments, schema),
        "length" | "lower" | "upper" if arguments.len() == 1 => {
            let base = infer_expression_type(arguments[0], schema)?;
            if !is_null_literal(arguments[0])
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
            if extract_unknown_string_literal(arguments[0]).is_some() {
                return Ok(BaseType::Float8);
            }
            let base = infer_expression_type(arguments[0], schema)?;
            if !is_null_literal(arguments[0]) && !is_numeric_type(base) {
                return Err(signature_error());
            }
            Ok(base)
        }
        "gen_random_uuid" | "uuidv4" | "uuidv7" if arguments.is_empty() => Ok(BaseType::Uuid),
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp"
            if arguments.is_empty() =>
        {
            Ok(BaseType::TimestampTz)
        }
        "nextval" | "currval" if arguments.len() == 1 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            Ok(BaseType::Int8)
        }
        "pg_get_serial_sequence" if arguments.len() == 2 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            validate_function_argument(arguments[1], BaseType::Text, schema, &signature_error)?;
            Ok(BaseType::Text)
        }
        "lastval" if arguments.is_empty() => Ok(BaseType::Int8),
        "setval" if matches!(arguments.len(), 2 | 3) => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            validate_function_argument(arguments[1], BaseType::Int8, schema, &signature_error)?;
            if let Some(is_called) = arguments.get(2) {
                validate_function_argument(is_called, BaseType::Bool, schema, &signature_error)?;
            }
            Ok(BaseType::Int8)
        }
        "coalesce"
        | "nullif"
        | "greatest"
        | "least"
        | "length"
        | "lower"
        | "upper"
        | "abs"
        | "nextval"
        | "currval"
        | "lastval"
        | "setval"
        | "pg_get_serial_sequence" => Err(signature_error()),
        _ => Err(PgError::create(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )),
    }
}

fn validate_function_argument(
    argument: &ast::Expr,
    target: BaseType,
    schema: RowScope<'_>,
    create_error: &impl Fn() -> PgError,
) -> Result<()> {
    if is_null_literal(argument) || extract_unknown_string_literal(argument).is_some() {
        return Ok(());
    }
    let source = infer_expression_type(argument, schema)?;
    if coercion::can_cast(source, target, CastContext::Implicit) {
        Ok(())
    } else {
        Err(create_error())
    }
}

pub(super) fn evaluate(
    expr: &ast::Expr,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    match expr {
        ast::Expr::Identifier(column) => {
            schema.resolve_column_value(std::slice::from_ref(column), row)
        }
        ast::Expr::CompoundIdentifier(columns) => schema.resolve_column_value(columns, row),
        ast::Expr::Value(_) => evaluate_literal(expr),
        ast::Expr::Nested(expr) => evaluate(expr, schema, row, context),
        ast::Expr::UnaryOp { op, expr } => {
            if matches!(op, ast::UnaryOperator::Minus)
                && let Some(value) = extract_number_literal(expr)
                && !value.contains(['.', 'e', 'E'])
            {
                return parse_integer_literal(&format!("-{value}"));
            }
            evaluate_unary_operator(*op, evaluate(expr, schema, row, context)?)
        }
        ast::Expr::BinaryOp { left, op, right } => {
            let left_type = infer_expression_type(left, schema)?;
            let right_type = infer_expression_type(right, schema)?;
            if matches!(
                op,
                ast::BinaryOperator::Plus
                    | ast::BinaryOperator::Minus
                    | ast::BinaryOperator::Multiply
                    | ast::BinaryOperator::Divide
            ) && (left_type == BaseType::Interval || right_type == BaseType::Interval)
            {
                let left = evaluate(left, schema, row, context)?;
                let right = evaluate(right, schema, row, context)?;
                if left.is_null() || right.is_null() {
                    return Ok(Value::Null);
                }
                return evaluate_temporal_arithmetic(op, left, right);
            }
            let target = match op {
                ast::BinaryOperator::And | ast::BinaryOperator::Or => BaseType::Bool,
                ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo => resolve_operator_type(left, right, schema)?,
                _ => resolve_operator_type(left, right, schema)?,
            };
            let left =
                evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?;
            let right =
                evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?;
            match op {
                ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide
                | ast::BinaryOperator::Modulo => {
                    if left.is_null() || right.is_null() {
                        Ok(Value::Null)
                    } else {
                        evaluate_numeric_operator(op, left, right)
                    }
                }
                ast::BinaryOperator::Eq
                | ast::BinaryOperator::NotEq
                | ast::BinaryOperator::Gt
                | ast::BinaryOperator::Lt
                | ast::BinaryOperator::GtEq
                | ast::BinaryOperator::LtEq => {
                    if left.is_null() || right.is_null() {
                        Ok(Value::Null)
                    } else {
                        evaluate_comparison(op, &left, &right)
                    }
                }
                ast::BinaryOperator::And | ast::BinaryOperator::Or => {
                    evaluate_boolean_operator(op, left, right)
                }
                _ => reject_unsupported("operator is not implemented"),
            }
        }
        ast::Expr::IsNull(expr) => Ok(Value::Bool(evaluate(expr, schema, row, context)?.is_null())),
        ast::Expr::IsNotNull(expr) => Ok(Value::Bool(
            !evaluate(expr, schema, row, context)?.is_null(),
        )),
        ast::Expr::InList {
            expr,
            list,
            negated,
        } => evaluate_membership(expr, list, *negated, schema, row, context),
        ast::Expr::AnyOp {
            left,
            compare_op,
            right,
            ..
        } => evaluate_quantified(left, compare_op, right, false, schema, row, context),
        ast::Expr::AllOp {
            left,
            compare_op,
            right,
        } => evaluate_quantified(left, compare_op, right, true, schema, row, context),
        ast::Expr::IsTrue(expr) => Ok(Value::Bool(matches!(
            evaluate_and_coerce(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context
            )?,
            Value::Bool(true)
        ))),
        ast::Expr::IsFalse(expr) => Ok(Value::Bool(matches!(
            evaluate_and_coerce(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context
            )?,
            Value::Bool(false)
        ))),
        ast::Expr::IsUnknown(expr) => Ok(Value::Bool(
            evaluate_and_coerce(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
            .is_null(),
        )),
        ast::Expr::IsDistinctFrom(left, right) | ast::Expr::IsNotDistinctFrom(left, right) => {
            let target = resolve_operator_type(left, right, schema)?;
            evaluate_distinctness(
                evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?,
                evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?,
                matches!(expr, ast::Expr::IsNotDistinctFrom(_, _)),
            )
        }
        ast::Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let result_type = resolve_expression_list_type(
                conditions
                    .iter()
                    .map(|condition| &condition.result)
                    .chain(else_result.as_deref())
                    .collect::<Vec<_>>()
                    .as_slice(),
                schema,
            )?;
            let operand = operand.as_deref();
            for condition in conditions {
                let matches = if let Some(operand) = &operand {
                    let target = resolve_operator_type(operand, &condition.condition, schema)?;
                    let operand = evaluate_and_coerce(
                        operand,
                        target,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    )?;
                    let condition = evaluate_and_coerce(
                        &condition.condition,
                        target,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    )?;
                    if operand.is_null() || condition.is_null() {
                        false
                    } else {
                        matches!(
                            evaluate_comparison(&ast::BinaryOperator::Eq, &operand, &condition)?,
                            Value::Bool(true)
                        )
                    }
                } else {
                    matches!(
                        evaluate(&condition.condition, schema, row, context)?,
                        Value::Bool(true)
                    )
                };
                if matches {
                    return evaluate_and_coerce(
                        &condition.result,
                        result_type,
                        CastContext::Implicit,
                        schema,
                        row,
                        context,
                    );
                }
            }
            match else_result {
                Some(result) => evaluate_and_coerce(
                    result,
                    result_type,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                ),
                None => Ok(Value::Null),
            }
        }
        ast::Expr::Function(function) => evaluate_function(function, schema, row, context),
        ast::Expr::Cast {
            kind,
            expr,
            data_type,
            format,
            ..
        } => {
            if !matches!(kind, ast::CastKind::Cast | ast::CastKind::DoubleColon) || format.is_some()
            {
                return reject_unsupported("cast variant is not implemented");
            }
            let target = coercion::convert_ast_data_type(data_type)?;
            if let Some(text) = extract_unknown_string_literal(expr) {
                coercion::coerce_unknown(text, target, CastContext::Explicit)
            } else {
                coercion::coerce(
                    evaluate(expr, schema, row, context)?,
                    infer_expression_type(expr, schema)?,
                    target,
                    CastContext::Explicit,
                )
            }
        }
        ast::Expr::Extract { field, expr, .. } => {
            extract_datetime_field(field.clone(), evaluate(expr, schema, row, context)?)
        }
        _ => reject_unsupported("expression is not implemented"),
    }
}

fn validate_membership_types(
    expr: &ast::Expr,
    list: &[ast::Expr],
    schema: RowScope<'_>,
) -> Result<()> {
    let left = extract_row_fields(expr);
    for candidate in list {
        let right = extract_row_fields(candidate);
        if left.len() != right.len() {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "subquery has too many columns",
            ));
        }
        for (left, right) in left.iter().zip(right) {
            resolve_expression_pair_type(left, right, schema)?;
        }
    }
    Ok(())
}

fn extract_row_fields(expr: &ast::Expr) -> &[ast::Expr] {
    match expr {
        ast::Expr::Tuple(fields) => fields,
        expr => std::slice::from_ref(expr),
    }
}

fn evaluate_membership(
    expr: &ast::Expr,
    list: &[ast::Expr],
    negated: bool,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    validate_membership_types(expr, list, schema)?;
    let mut result = Value::Bool(false);
    for candidate in list {
        result = evaluate_boolean_operator(
            &ast::BinaryOperator::Or,
            result,
            evaluate_row_comparison(
                expr,
                candidate,
                &ast::BinaryOperator::Eq,
                schema,
                row,
                context,
            )?,
        )?;
        if result == Value::Bool(true) {
            break;
        }
    }
    if negated {
        evaluate_unary_operator(ast::UnaryOperator::Not, result)
    } else {
        Ok(result)
    }
}

fn evaluate_quantified(
    left: &ast::Expr,
    compare_op: &ast::BinaryOperator,
    right: &ast::Expr,
    all: bool,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    let candidates = extract_row_fields(right);
    let mut result = Value::Bool(all);
    for candidate in candidates {
        result = evaluate_boolean_operator(
            if all {
                &ast::BinaryOperator::And
            } else {
                &ast::BinaryOperator::Or
            },
            result,
            evaluate_row_comparison(left, candidate, compare_op, schema, row, context)?,
        )?;
        if result == Value::Bool(!all) {
            break;
        }
    }
    Ok(result)
}

fn evaluate_row_comparison(
    left: &ast::Expr,
    right: &ast::Expr,
    operator: &ast::BinaryOperator,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    let left = extract_row_fields(left);
    let right = extract_row_fields(right);
    if left.len() != right.len() {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "subquery has too many columns",
        ));
    }
    let mut result = Value::Bool(true);
    for (left, right) in left.iter().zip(right) {
        let target = resolve_operator_type(left, right, schema)?;
        let left = evaluate_and_coerce(left, target, CastContext::Implicit, schema, row, context)?;
        let right =
            evaluate_and_coerce(right, target, CastContext::Implicit, schema, row, context)?;
        let evaluate_comparison = if left.is_null() || right.is_null() {
            Value::Null
        } else {
            evaluate_comparison(operator, &left, &right)?
        };
        result = evaluate_boolean_operator(&ast::BinaryOperator::And, result, evaluate_comparison)?;
        if result == Value::Bool(false) {
            break;
        }
    }
    Ok(result)
}

pub(super) fn evaluate_and_coerce(
    expression: &ast::Expr,
    target: BaseType,
    context: CastContext,
    schema: RowScope<'_>,
    row: &[Value],
    execution: &StatementExecutionContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, PgType::create(target), context)
    } else {
        let source = infer_expression_type(expression, schema)?;
        coercion::coerce(
            evaluate(expression, schema, row, execution)?,
            source,
            PgType::create(target),
            context,
        )
    }
}

fn evaluate_function(
    function: &ast::Function,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementExecutionContext,
) -> Result<Value> {
    if is_aggregate_function(function) {
        infer_aggregate_return_type(function, schema)?;
        return Err(PgError::create(
            SqlState::GroupingError,
            "aggregate function is not allowed in this context",
        ));
    }
    infer_function_return_type(function, schema)?;
    let function_name = normalize_unqualified_object_name(&function.name)?;
    let arguments = extract_function_arguments(function)?;
    let result_type = infer_function_return_type(function, schema)?;
    match function_name.as_str() {
        "gen_random_uuid" | "uuidv4" => {
            let mut bytes = [0; 16];
            context
                .rng
                .lock()
                .expect("rng mutex is poisoned")
                .fill_bytes(&mut bytes);
            Ok(Value::Uuid(
                uuid::Builder::from_random_bytes(bytes).into_uuid(),
            ))
        }
        "uuidv7" => {
            let milliseconds =
                u64::try_from(context.clock_timestamp.timestamp_millis()).map_err(|_| {
                    PgError::create(
                        SqlState::NumericValueOutOfRange,
                        "uuidv7 timestamp is out of range",
                    )
                })?;
            let mut bytes = [0; 10];
            context
                .rng
                .lock()
                .expect("rng mutex is poisoned")
                .fill_bytes(&mut bytes);
            Ok(Value::Uuid(
                uuid::Builder::from_unix_timestamp_millis(milliseconds, &bytes).into_uuid(),
            ))
        }
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
            let value = match function_name.as_str() {
                "now" | "transaction_timestamp" => context.transaction_timestamp,
                "statement_timestamp" => context.statement_timestamp,
                "clock_timestamp" => context.clock_timestamp,
                _ => unreachable!(),
            };
            Ok(Value::TimestampTz(crate::value::PgTimestampTz::Finite(
                value,
            )))
        }
        "nextval" | "currval" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let Value::Text(name) = name else {
                return Ok(Value::Null);
            };
            let value = if function_name == "nextval" {
                context.sequences.get_next_value(&name)?
            } else {
                context.sequences.get_current_value(&name)?
            };
            Ok(Value::Int8(value))
        }
        "lastval" => Ok(Value::Int8(context.sequences.get_last_value()?)),
        "pg_get_serial_sequence" => {
            let table = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let column = evaluate_and_coerce(
                arguments[1],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let (Value::Text(table), Value::Text(column)) = (table, column) else {
                return Ok(Value::Null);
            };
            Ok(context
                .sequences
                .get_owned_sequence(&table, &column)?
                .map(Value::Text)
                .unwrap_or(Value::Null))
        }
        "setval" => {
            let name = evaluate_and_coerce(
                arguments[0],
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let value = evaluate_and_coerce(
                arguments[1],
                BaseType::Int8,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let is_called = if let Some(argument) = arguments.get(2) {
                evaluate_and_coerce(
                    argument,
                    BaseType::Bool,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?
            } else {
                Value::Bool(true)
            };
            let (Value::Text(name), Value::Int8(value), Value::Bool(is_called)) =
                (name, value, is_called)
            else {
                return Ok(Value::Null);
            };
            Ok(Value::Int8(
                context.sequences.set_value(&name, value, is_called)?,
            ))
        }
        "coalesce" => {
            for argument in arguments {
                let value = evaluate_and_coerce(
                    argument,
                    result_type,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?;
                if !value.is_null() {
                    return Ok(value);
                }
            }
            Ok(Value::Null)
        }
        "nullif" => {
            let left = evaluate_and_coerce(
                arguments[0],
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            if left.is_null() {
                return Ok(Value::Null);
            }
            let right = evaluate_and_coerce(
                arguments[1],
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            if !right.is_null()
                && matches!(
                    evaluate_comparison(&ast::BinaryOperator::Eq, &left, &right)?,
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
                let value = evaluate_and_coerce(
                    argument,
                    result_type,
                    CastContext::Implicit,
                    schema,
                    row,
                    context,
                )?;
                if value.is_null() {
                    continue;
                }
                selected = Some(match selected {
                    None => value,
                    Some(current) => {
                        let operator = if function_name == "greatest" {
                            ast::BinaryOperator::Gt
                        } else {
                            ast::BinaryOperator::Lt
                        };
                        if matches!(
                            evaluate_comparison(&operator, &value, &current)?,
                            Value::Bool(true)
                        ) {
                            value
                        } else {
                            current
                        }
                    }
                });
            }
            Ok(selected.unwrap_or(Value::Null))
        }
        "length" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Int4(
                i32::try_from(value.chars().count()).expect("text length must fit in int4"),
            )),
            _ => unreachable!("length argument was type-checked"),
        },
        "lower" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Text(value.to_lowercase())),
            _ => unreachable!("lower argument was type-checked"),
        },
        "upper" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Text(value.to_uppercase())),
            _ => unreachable!("upper argument was type-checked"),
        },
        "abs" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Int2(value) => value.checked_abs().map(Value::Int2).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "smallint out of range")
            }),
            Value::Int4(value) => value.checked_abs().map(Value::Int4).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "integer out of range")
            }),
            Value::Int8(value) => value.checked_abs().map(Value::Int8).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "bigint out of range")
            }),
            Value::Float4(value) => Ok(Value::Float4(value.abs())),
            Value::Float8(value) => Ok(Value::Float8(value.abs())),
            Value::Numeric(value) => Ok(Value::Numeric(value.abs())),
            _ => unreachable!("abs argument was type-checked"),
        },
        _ => unreachable!("function name was type-checked"),
    }
}

pub(super) fn evaluate_comparison(
    operator: &ast::BinaryOperator,
    left: &Value,
    right: &Value,
) -> Result<Value> {
    let ordering = compare_values(left, right)?;
    Ok(Value::Bool(match operator {
        ast::BinaryOperator::Eq => ordering == Ordering::Equal,
        ast::BinaryOperator::NotEq => ordering != Ordering::Equal,
        ast::BinaryOperator::Gt => ordering == Ordering::Greater,
        ast::BinaryOperator::Lt => ordering == Ordering::Less,
        ast::BinaryOperator::GtEq => ordering != Ordering::Less,
        ast::BinaryOperator::LtEq => ordering != Ordering::Greater,
        _ => unreachable!("evaluate_comparison operator was checked by caller"),
    }))
}

fn extract_datetime_field(field: ast::DateTimeField, value: Value) -> Result<Value> {
    use chrono::{Datelike, Timelike};
    let value = match value {
        Value::Null => return Ok(Value::Null),
        Value::Date(crate::value::PgDate::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Dow => i64::from(value.weekday().num_days_from_sunday()),
            ast::DateTimeField::Doy => i64::from(value.ordinal()),
            ast::DateTimeField::Epoch => i64::from(value.num_days_from_ce()) * 86_400,
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::Time(crate::value::PgTime(value)) => match field {
            ast::DateTimeField::Hour => value / 3_600_000_000,
            ast::DateTimeField::Minute => value / 60_000_000 % 60,
            ast::DateTimeField::Second => value / 1_000_000 % 60,
            ast::DateTimeField::Microsecond | ast::DateTimeField::Microseconds => value % 1_000_000,
            ast::DateTimeField::Epoch => value / 1_000_000,
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::Date(crate::value::PgDate::Infinity | crate::value::PgDate::NegInfinity) => {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                "cannot extract from infinite date",
            ));
        }
        Value::Timestamp(crate::value::PgTimestamp::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Hour => i64::from(value.hour()),
            ast::DateTimeField::Minute => i64::from(value.minute()),
            ast::DateTimeField::Second => i64::from(value.second()),
            ast::DateTimeField::Microsecond | ast::DateTimeField::Microseconds => {
                i64::from(value.nanosecond() / 1_000)
            }
            ast::DateTimeField::Epoch => value.and_utc().timestamp(),
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Hour => i64::from(value.hour()),
            ast::DateTimeField::Minute => i64::from(value.minute()),
            ast::DateTimeField::Second => i64::from(value.second()),
            ast::DateTimeField::Microsecond | ast::DateTimeField::Microseconds => {
                i64::from(value.nanosecond() / 1_000)
            }
            ast::DateTimeField::Epoch => value.timestamp(),
            _ => {
                return reject_unsupported("date part is not implemented");
            }
        },
        Value::Timestamp(
            crate::value::PgTimestamp::Infinity | crate::value::PgTimestamp::NegInfinity,
        )
        | Value::TimestampTz(
            crate::value::PgTimestampTz::Infinity | crate::value::PgTimestampTz::NegInfinity,
        ) => {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                "cannot extract from infinite timestamp",
            ));
        }
        _ => {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "extract source must be date or time",
            ));
        }
    };
    Ok(Value::Numeric(value.into()))
}

pub(super) fn compare_values(left: &Value, right: &Value) -> Result<Ordering> {
    Ok(match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
        (Value::Int2(left), Value::Int2(right)) => left.cmp(right),
        (Value::Int4(left), Value::Int4(right)) => left.cmp(right),
        (Value::Int8(left), Value::Int8(right)) => left.cmp(right),
        (Value::Float4(left), Value::Float4(right)) => compare_float4(*left, *right),
        (Value::Float8(left), Value::Float8(right)) => compare_float8(*left, *right),
        (Value::Numeric(left), Value::Numeric(right)) => left.cmp(right),
        (Value::Text(left), Value::Text(right)) => left.cmp(right),
        (Value::Bytea(left), Value::Bytea(right)) => left.cmp(right),
        (Value::Uuid(left), Value::Uuid(right)) => left.cmp(right),
        (Value::Date(left), Value::Date(right)) => left.cmp(right),
        (Value::Time(left), Value::Time(right)) => left.cmp(right),
        (Value::Timestamp(left), Value::Timestamp(right)) => left.cmp(right),
        (Value::TimestampTz(left), Value::TimestampTz(right)) => left.cmp(right),
        (Value::Interval(left), Value::Interval(right)) => {
            let left = i128::from(left.months)
                * i128::from(DAYS_PER_MONTH)
                * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(left.days) * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(left.micros);
            let right = i128::from(right.months)
                * i128::from(DAYS_PER_MONTH)
                * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(right.days) * i128::from(MICROSECONDS_PER_DAY)
                + i128::from(right.micros);
            left.cmp(&right)
        }
        _ => {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "operator has incompatible types",
            ));
        }
    })
}

fn compare_float4(left: f32, right: f32) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .expect("finite floats are comparable"),
    }
}

fn compare_float8(left: f64, right: f64) -> Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => left
            .partial_cmp(&right)
            .expect("finite floats are comparable"),
    }
}
