use super::{
    comparisons::{
        create_missing_operator_error, extract_row_fields, validate_comparison_type,
        validate_equality_type, validate_membership_types, validate_row_comparison_types,
    },
    functions::{infer_function_return_type, validate_function_argument},
    literals::{
        evaluate_literal, extract_ast_value, extract_number_literal,
        extract_unknown_string_literal, is_null_literal, is_parameter_placeholder,
        parse_integer_literal,
    },
};
use crate::executor::{arithmetic::infer_interval_arithmetic_type, json, scope::RowScope};
use crate::{
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn infer_expression_type(expr: &ast::Expr, schema: RowScope<'_>) -> Result<BaseType> {
    if let Some(value) = extract_ast_value(expr) {
        return match value {
            ast::Value::Null => Ok(BaseType::Text),
            ast::Value::Boolean(_) => Ok(BaseType::Bool),
            ast::Value::SingleQuotedString(_) | ast::Value::DollarQuotedString(_) => {
                Ok(BaseType::Text)
            }
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
        ast::Expr::TypedString(typed) if !typed.uses_odbc_syntax => {
            let value = evaluate_literal(expr)?;
            Ok(value
                .get_base_type()
                .expect("typed string literal is not NULL"))
        }
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
        ast::Expr::BinaryOp { left, op, right }
            if json::infer_json_operator(op, left, right, schema)?.is_some() =>
        {
            Ok(json::infer_json_operator(op, left, right, schema)?
                .expect("resolved JSON operator")
                .2)
        }
        ast::Expr::Array(array) => {
            for element in &array.elem {
                validate_function_argument(element, BaseType::Text, schema, &|| {
                    PgError::create(
                        SqlState::DatatypeMismatch,
                        "text array element has incompatible type",
                    )
                })?;
            }
            Ok(BaseType::TextArray)
        }
        ast::Expr::Interval(interval)
            if interval.leading_field.is_none()
                && interval.leading_precision.is_none()
                && interval.last_field.is_none()
                && interval.fractional_seconds_precision.is_none() =>
        {
            if extract_unknown_string_literal(&interval.value).is_none() {
                return reject_unsupported("interval expression is not implemented");
            }
            Ok(BaseType::Interval)
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
                let data_type = resolve_operator_type(left, right, schema)?;
                validate_comparison_type(op, data_type)?;
                Ok(BaseType::Bool)
            }
            ast::BinaryOperator::PGRegexMatch => {
                for expression in [left.as_ref(), right.as_ref()] {
                    let base = infer_expression_type(expression, schema)?;
                    if !matches!(base, BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
                        && !is_null_literal(expression)
                        && extract_unknown_string_literal(expression).is_none()
                    {
                        return Err(PgError::create(
                            SqlState::UndefinedFunction,
                            "operator does not exist for these argument types",
                        ));
                    }
                }
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
        ast::Expr::IsNull(expression) | ast::Expr::IsNotNull(expression) => {
            infer_expression_type(expression, schema)?;
            Ok(BaseType::Bool)
        }
        ast::Expr::InList { expr, list, .. } => {
            validate_membership_types(expr, list, schema)?;
            Ok(BaseType::Bool)
        }
        ast::Expr::InSubquery { .. } | ast::Expr::Exists { .. } => Ok(BaseType::Bool),
        ast::Expr::AnyOp {
            left,
            compare_op,
            right,
            ..
        }
        | ast::Expr::AllOp {
            left,
            compare_op,
            right,
        } => {
            if let ast::Expr::Tuple(candidates) = right.as_ref() {
                if candidates.is_empty() {
                    for field in extract_row_fields(left) {
                        validate_comparison_type(
                            compare_op,
                            infer_expression_type(field, schema)?,
                        )?;
                    }
                } else {
                    for candidate in candidates {
                        validate_row_comparison_types(left, candidate, compare_op, schema)?;
                    }
                }
            }
            Ok(BaseType::Bool)
        }
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
            validate_equality_type(resolve_operator_type(left, right, schema)?)?;
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
                    let data_type =
                        resolve_expression_pair_type(operand, &condition.condition, schema)
                            .map_err(|_| {
                                PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "CASE types are incompatible",
                                )
                            })?;
                    validate_equality_type(data_type)?;
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
            if matches!(target.base, BaseType::Json | BaseType::Jsonb)
                && let Some(text) = extract_unknown_string_literal(expr)
            {
                coercion::coerce_unknown(text, target, CastContext::Explicit)?;
            }
            if extract_unknown_string_literal(expr).is_none()
                && !is_null_literal(expr)
                && !is_parameter_placeholder(expr)
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn infer_expression_data_type(
    expression: &ast::Expr,
    schema: RowScope<'_>,
) -> Result<PgType> {
    match expression {
        ast::Expr::Identifier(column) => Ok(schema.resolve_column(std::slice::from_ref(column))?.1),
        ast::Expr::CompoundIdentifier(columns) => Ok(schema.resolve_column(columns)?.1),
        ast::Expr::Nested(expression) => infer_expression_data_type(expression, schema),
        ast::Expr::Cast { data_type, .. } => {
            infer_expression_type(expression, schema)?;
            coercion::convert_ast_data_type(data_type)
        }
        _ => Ok(PgType::create(infer_expression_type(expression, schema)?)),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn is_numeric_type(base: BaseType) -> bool {
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn resolve_operator_type(
    left: &ast::Expr,
    right: &ast::Expr,
    schema: RowScope<'_>,
) -> Result<BaseType> {
    if is_parameter_placeholder(left) {
        return infer_expression_type(right, schema);
    }
    if is_parameter_placeholder(right) {
        return infer_expression_type(left, schema);
    }
    let left_type = infer_expression_type(left, schema)?;
    let right_type = infer_expression_type(right, schema)?;
    if left_type != right_type
        && (matches!(left_type, BaseType::Json | BaseType::Jsonb)
            || matches!(right_type, BaseType::Json | BaseType::Jsonb))
        && !is_null_literal(left)
        && !is_null_literal(right)
        && extract_unknown_string_literal(left).is_none()
        && extract_unknown_string_literal(right).is_none()
    {
        return Err(create_missing_operator_error(left_type));
    }
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn resolve_expression_list_type(
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
