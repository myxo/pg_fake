use super::*;

pub(super) fn expression_value(
    expr: &Expr,
    target: PgType,
    schema: &TableSchema,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<Value> {
    if let Some(text) = unknown_string(expr) {
        coercion::coerce_unknown(text, target, CastContext::Assignment)
    } else {
        coercion::coerce(
            evaluate(expr, RowScope::Table(schema), row, context)?,
            expression_type(expr, RowScope::Table(schema))?,
            target,
            CastContext::Assignment,
        )
    }
}

pub(super) fn column_default(column: &ColumnDef, context: &ExecutionContext) -> Result<Value> {
    let Some(expr) = &column.default else {
        return Ok(Value::Null);
    };
    expression_value(expr, column.data_type, &constant_schema(), &[], context).map_err(|error| {
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

pub(super) fn validate_not_null(schema: &TableSchema, row: &[Value]) -> Result<()> {
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

pub(super) fn validate_check_constraint_types(schema: &TableSchema) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check(expression) = constraint else {
            continue;
        };
        let base = expression_type(expression, RowScope::Table(schema))?;
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

pub(super) fn validate_check_constraints(
    schema: &TableSchema,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check(expression) = constraint else {
            continue;
        };
        match evaluate_as(
            expression,
            BaseType::Bool,
            CastContext::Implicit,
            RowScope::Table(schema),
            row,
            context,
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

pub(super) fn default_expression(expr: &Expr) -> bool {
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

fn ast_value(expr: &Expr) -> Option<&AstValue> {
    let Expr::Value(value) = expr else {
        return None;
    };
    Some(&value.value)
}

pub(super) fn number_literal(expr: &Expr) -> Option<&str> {
    let AstValue::Number(value, _) = ast_value(expr)? else {
        return None;
    };
    Some(value)
}

fn literal_value(expr: &Expr) -> Result<Value> {
    if let Some(value) = ast_value(expr) {
        return match value {
            AstValue::Null => Ok(Value::Null),
            AstValue::Boolean(value) => Ok(Value::Bool(*value)),
            AstValue::SingleQuotedString(value) => Ok(Value::Text(value.clone())),
            AstValue::Number(value, _) if value.contains(['.', 'e', 'E']) => {
                Value::parse(BaseType::Numeric, value)
            }
            AstValue::Number(value, _) => integer_literal(value),
            _ => Err(PgError::new(
                SqlState::CannotCoerce,
                "literal has incompatible type",
            )),
        };
    }
    match expr {
        Expr::UnaryOp {
            op: UnaryOperator::Plus,
            expr,
        } => literal_value(expr),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } if number_literal(expr).is_some_and(|value| !value.contains(['.', 'e', 'E'])) => {
            let value = number_literal(expr).expect("integer literal pattern was checked");
            integer_literal(&format!("-{value}"))
        }
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } if number_literal(expr).is_some() => unary(UnaryOperator::Minus, literal_value(expr)?),
        Expr::Nested(expr) => literal_value(expr),
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

pub(super) fn unknown_string(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Value(value) => match &value.value {
            AstValue::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        Expr::Nested(expr) => unknown_string(expr),
        _ => None,
    }
}

pub(crate) fn expression_type(expr: &Expr, schema: RowScope<'_>) -> Result<BaseType> {
    if let Some(value) = ast_value(expr) {
        return match value {
            AstValue::Null => Ok(BaseType::Text),
            AstValue::Boolean(_) => Ok(BaseType::Bool),
            AstValue::SingleQuotedString(_) => Ok(BaseType::Text),
            AstValue::Number(value, _) if value.contains(['.', 'e', 'E']) => Ok(BaseType::Numeric),
            AstValue::Number(value, _) => Ok(integer_literal(value)?
                .base_type()
                .expect("numeric literal is not null")),
            _ => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "literal is not implemented",
            )),
        };
    }
    match expr {
        Expr::Identifier(column) => Ok(schema.resolve_column(std::slice::from_ref(column))?.1.base),
        Expr::CompoundIdentifier(columns) => Ok(schema.resolve_column(columns)?.1.base),
        Expr::Nested(expr) => expression_type(expr, schema),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } if number_literal(expr).is_some_and(|value| !value.contains(['.', 'e', 'E'])) => {
            let value = number_literal(expr).expect("integer literal pattern was checked");
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
        Expr::InList { expr, list, .. } => {
            validate_membership_types(expr, list, schema)?;
            Ok(BaseType::Bool)
        }
        Expr::InSubquery { .. } | Expr::Exists { .. } | Expr::AnyOp { .. } | Expr::AllOp { .. } => {
            Ok(BaseType::Bool)
        }
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
            else_result,
            ..
        } => {
            if let Some(operand) = operand {
                for condition in conditions {
                    expression_common_type(operand, &condition.condition, schema).map_err(
                        |_| PgError::new(SqlState::DatatypeMismatch, "CASE types are incompatible"),
                    )?;
                }
            } else {
                for condition in conditions {
                    let base = expression_type(&condition.condition, schema)?;
                    if base != BaseType::Bool && !null_expression(&condition.condition) {
                        return Err(PgError::new(
                            SqlState::DatatypeMismatch,
                            "CASE condition must be boolean",
                        ));
                    }
                }
            }
            common_expression_type(
                conditions
                    .iter()
                    .map(|condition| &condition.result)
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
            ..
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
        Expr::Value(value) if matches!(&value.value, AstValue::Null) => true,
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

fn expression_common_type(left: &Expr, right: &Expr, schema: RowScope<'_>) -> Result<BaseType> {
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

fn common_expression_type(expressions: &[&Expr], schema: RowScope<'_>) -> Result<BaseType> {
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

fn function_type(function: &Function, schema: RowScope<'_>) -> Result<BaseType> {
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
        "gen_random_uuid" | "uuidv4" | "uuidv7" if arguments.is_empty() => Ok(BaseType::Uuid),
        "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp"
            if arguments.is_empty() =>
        {
            Ok(BaseType::TimestampTz)
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

pub(super) fn evaluate(
    expr: &Expr,
    schema: RowScope<'_>,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<Value> {
    match expr {
        Expr::Identifier(column) => schema.column_value(std::slice::from_ref(column), row),
        Expr::CompoundIdentifier(columns) => schema.column_value(columns, row),
        Expr::Value(_) => literal_value(expr),
        Expr::Nested(expr) => evaluate(expr, schema, row, context),
        Expr::UnaryOp { op, expr } => {
            if matches!(op, UnaryOperator::Minus)
                && let Some(value) = number_literal(expr)
                && !value.contains(['.', 'e', 'E'])
            {
                return integer_literal(&format!("-{value}"));
            }
            unary(*op, evaluate(expr, schema, row, context)?)
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
                let left = evaluate(left, schema, row, context)?;
                let right = evaluate(right, schema, row, context)?;
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
            let left = evaluate_as(left, target, CastContext::Implicit, schema, row, context)?;
            let right = evaluate_as(right, target, CastContext::Implicit, schema, row, context)?;
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
        Expr::IsNull(expr) => Ok(Value::Bool(evaluate(expr, schema, row, context)?.is_null())),
        Expr::IsNotNull(expr) => Ok(Value::Bool(
            !evaluate(expr, schema, row, context)?.is_null(),
        )),
        Expr::InList {
            expr,
            list,
            negated,
        } => evaluate_membership(expr, list, *negated, schema, row, context),
        Expr::AnyOp {
            left,
            compare_op,
            right,
            ..
        } => evaluate_quantified(left, compare_op, right, false, schema, row, context),
        Expr::AllOp {
            left,
            compare_op,
            right,
        } => evaluate_quantified(left, compare_op, right, true, schema, row, context),
        Expr::IsTrue(expr) => Ok(Value::Bool(matches!(
            evaluate_as(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context
            )?,
            Value::Bool(true)
        ))),
        Expr::IsFalse(expr) => Ok(Value::Bool(matches!(
            evaluate_as(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context
            )?,
            Value::Bool(false)
        ))),
        Expr::IsUnknown(expr) => Ok(Value::Bool(
            evaluate_as(
                expr,
                BaseType::Bool,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?
            .is_null(),
        )),
        Expr::IsDistinctFrom(left, right) | Expr::IsNotDistinctFrom(left, right) => {
            let target = expression_common_type(left, right, schema)?;
            distinct(
                evaluate_as(left, target, CastContext::Implicit, schema, row, context)?,
                evaluate_as(right, target, CastContext::Implicit, schema, row, context)?,
                matches!(expr, Expr::IsNotDistinctFrom(_, _)),
            )
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            let result_type = common_expression_type(
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
                    let target = expression_common_type(operand, &condition.condition, schema)?;
                    let operand =
                        evaluate_as(operand, target, CastContext::Implicit, schema, row, context)?;
                    let condition = evaluate_as(
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
                            comparison(&BinaryOperator::Eq, &operand, &condition)?,
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
                    return evaluate_as(
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
                Some(result) => evaluate_as(
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
        Expr::Function(function) => evaluate_function(function, schema, row, context),
        Expr::Cast {
            kind,
            expr,
            data_type,
            format,
            ..
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
                    evaluate(expr, schema, row, context)?,
                    expression_type(expr, schema)?,
                    target,
                    CastContext::Explicit,
                )
            }
        }
        Expr::Extract { field, expr, .. } => {
            extract_value(field.clone(), evaluate(expr, schema, row, context)?)
        }
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "expression is not implemented",
        )),
    }
}

fn validate_membership_types(expr: &Expr, list: &[Expr], schema: RowScope<'_>) -> Result<()> {
    let left = split_tuple_fields(expr);
    for candidate in list {
        let right = split_tuple_fields(candidate);
        if left.len() != right.len() {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "subquery has too many columns",
            ));
        }
        for (left, right) in left.iter().zip(right) {
            expression_common_type(left, right, schema)?;
        }
    }
    Ok(())
}

fn split_tuple_fields(expr: &Expr) -> &[Expr] {
    match expr {
        Expr::Tuple(fields) => fields,
        expr => std::slice::from_ref(expr),
    }
}

fn evaluate_membership(
    expr: &Expr,
    list: &[Expr],
    negated: bool,
    schema: RowScope<'_>,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<Value> {
    validate_membership_types(expr, list, schema)?;
    let mut result = Value::Bool(false);
    for candidate in list {
        result = boolean_binary(
            &BinaryOperator::Or,
            result,
            evaluate_row_comparison(expr, candidate, &BinaryOperator::Eq, schema, row, context)?,
        )?;
        if result == Value::Bool(true) {
            break;
        }
    }
    if negated {
        unary(UnaryOperator::Not, result)
    } else {
        Ok(result)
    }
}

fn evaluate_quantified(
    left: &Expr,
    compare_op: &BinaryOperator,
    right: &Expr,
    all: bool,
    schema: RowScope<'_>,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<Value> {
    let candidates = split_tuple_fields(right);
    let mut result = Value::Bool(all);
    for candidate in candidates {
        result = boolean_binary(
            if all {
                &BinaryOperator::And
            } else {
                &BinaryOperator::Or
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
    left: &Expr,
    right: &Expr,
    operator: &BinaryOperator,
    schema: RowScope<'_>,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<Value> {
    let left = split_tuple_fields(left);
    let right = split_tuple_fields(right);
    if left.len() != right.len() {
        return Err(PgError::new(
            SqlState::SyntaxError,
            "subquery has too many columns",
        ));
    }
    let mut result = Value::Bool(true);
    for (left, right) in left.iter().zip(right) {
        let target = expression_common_type(left, right, schema)?;
        let left = evaluate_as(left, target, CastContext::Implicit, schema, row, context)?;
        let right = evaluate_as(right, target, CastContext::Implicit, schema, row, context)?;
        let comparison = if left.is_null() || right.is_null() {
            Value::Null
        } else {
            comparison(operator, &left, &right)?
        };
        result = boolean_binary(&BinaryOperator::And, result, comparison)?;
        if result == Value::Bool(false) {
            break;
        }
    }
    Ok(result)
}

pub(super) fn evaluate_as(
    expression: &Expr,
    target: BaseType,
    context: CastContext,
    schema: RowScope<'_>,
    row: &[Value],
    execution: &ExecutionContext,
) -> Result<Value> {
    if let Some(text) = unknown_string(expression) {
        coercion::coerce_unknown(text, PgType::new(target), context)
    } else {
        let source = expression_type(expression, schema)?;
        coercion::coerce(
            evaluate(expression, schema, row, execution)?,
            source,
            PgType::new(target),
            context,
        )
    }
}

fn evaluate_function(
    function: &Function,
    schema: RowScope<'_>,
    row: &[Value],
    context: &ExecutionContext,
) -> Result<Value> {
    function_type(function, schema)?;
    let function_name = name(&function.name)?;
    let arguments = function_arguments(function)?;
    let result_type = function_type(function, schema)?;
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
                    PgError::new(
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
        "coalesce" => {
            for argument in arguments {
                let value = evaluate_as(
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
            let left = evaluate_as(
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
            let right = evaluate_as(
                arguments[1],
                result_type,
                CastContext::Implicit,
                schema,
                row,
                context,
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
                let value = evaluate_as(
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

pub(super) fn comparison(operator: &BinaryOperator, left: &Value, right: &Value) -> Result<Value> {
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

pub(super) fn value_ordering(left: &Value, right: &Value) -> Result<Ordering> {
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
