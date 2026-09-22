use super::{
    comparisons::{
        create_missing_operator_error, extract_row_fields, validate_comparison_type,
        validate_equality_type, validate_membership_types, validate_row_comparison_types,
    },
    functions::{infer_function_return_type, validate_function_argument},
    literals::{
        extract_ast_value, extract_number_literal, extract_unknown_string_literal, is_null_literal,
        is_parameter_placeholder, parse_integer_literal,
    },
};
use crate::executor::{
    arithmetic::{infer_interval_arithmetic_type, infer_pg_lsn_arithmetic_type},
    json,
    scope::RowScope,
};
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
            Ok(coercion::convert_ast_data_type(&typed.data_type)?.base)
        }
        ast::Expr::Identifier(column) => {
            Ok(schema.resolve_column(std::slice::from_ref(column))?.1.base)
        }
        ast::Expr::CompoundIdentifier(columns) => Ok(schema.resolve_column(columns)?.1.base),
        ast::Expr::CompoundFieldAccess { root, access_chain } => {
            let [ast::AccessExpr::Subscript(ast::Subscript::Index { index })] =
                access_chain.as_slice()
            else {
                return reject_unsupported("array access shape is not implemented");
            };
            let Some(element_type) = infer_expression_type(root, schema)?.get_array_element_type()
            else {
                return reject_unsupported("array subscript element type is not implemented");
            };
            let index_type = infer_expression_type(index, schema)?;
            if !is_null_literal(index)
                && extract_unknown_string_literal(index).is_none()
                && !is_parameter_placeholder(index)
                && !coercion::can_cast(index_type, BaseType::Int4, CastContext::Assignment)
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "array subscript must have type integer",
                ));
            }
            Ok(element_type)
        }
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
            if !(matches!(
                op,
                ast::BinaryOperator::StringConcat
                    | ast::BinaryOperator::AtArrow
                    | ast::BinaryOperator::ArrowAt
                    | ast::BinaryOperator::PGOverlap
            ) && (infer_expression_type(left, schema)?
                .get_array_element_type()
                .is_some()
                || infer_expression_type(right, schema)?
                    .get_array_element_type()
                    .is_some()))
                && json::infer_json_operator(op, left, right, schema)?.is_some() =>
        {
            Ok(json::infer_json_operator(op, left, right, schema)?
                .expect("resolved JSON operator")
                .2)
        }
        ast::Expr::Floor { expr, field } => {
            if !matches!(
                field,
                ast::CeilFloorKind::DateTimeField(ast::DateTimeField::NoDateTime)
            ) {
                return reject_unsupported("FLOOR modifier is not implemented");
            }
            Ok(
                super::runtime::infer_runtime_function("floor", &[expr], schema)?
                    .expect("floor is a runtime function")
                    .1,
            )
        }
        ast::Expr::AtTimeZone {
            timestamp,
            time_zone,
        } => {
            if infer_expression_type(time_zone, schema)? == BaseType::Interval {
                return reject_unsupported("interval time zones are not implemented");
            }
            validate_function_argument(time_zone, BaseType::Text, schema, &|| {
                PgError::create(
                    SqlState::UndefinedFunction,
                    "time zone signature does not exist",
                )
            })?;
            if is_null_literal(timestamp) || extract_unknown_string_literal(timestamp).is_some() {
                return Ok(BaseType::Timestamp);
            }
            match infer_expression_type(timestamp, schema)? {
                BaseType::Timestamp => Ok(BaseType::TimestampTz),
                BaseType::TimestampTz | BaseType::Date => Ok(BaseType::Timestamp),
                BaseType::Time => {
                    reject_unsupported("time zone conversion for time is not implemented")
                }
                _ => Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "time zone signature does not exist",
                )),
            }
        }
        ast::Expr::Like {
            expr, pattern, any, ..
        }
        | ast::Expr::ILike {
            expr, pattern, any, ..
        } => {
            if *any {
                return reject_unsupported("LIKE ANY is not implemented");
            }
            for argument in [expr, pattern] {
                validate_function_argument(argument, BaseType::Text, schema, &|| {
                    PgError::create(
                        SqlState::UndefinedFunction,
                        "pattern operator does not exist for argument types",
                    )
                })?;
            }
            Ok(BaseType::Bool)
        }
        ast::Expr::Array(array) => infer_array_type(array, schema),
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
            ast::BinaryOperator::StringConcat => {
                let left_type = infer_expression_type(left, schema)?;
                let right_type = infer_expression_type(right, schema)?;
                let left_unknown =
                    is_null_literal(left) || extract_unknown_string_literal(left).is_some();
                let right_unknown =
                    is_null_literal(right) || extract_unknown_string_literal(right).is_some();
                match (
                    left_type.get_array_element_type(),
                    right_type.get_array_element_type(),
                ) {
                    (Some(left), Some(right)) => coercion::resolve_common_type(left, right)
                        .and_then(BaseType::get_array_type)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedFunction,
                                "array concatenation types are incompatible",
                            )
                        }),
                    (Some(element), None) if right_unknown => Ok(element
                        .get_array_type()
                        .expect("array element has an array type")),
                    (None, Some(element)) if left_unknown => Ok(element
                        .get_array_type()
                        .expect("array element has an array type")),
                    (Some(element), None) | (None, Some(element)) => {
                        let scalar = if left_type.get_array_element_type().is_none() {
                            left_type
                        } else {
                            right_type
                        };
                        coercion::resolve_common_type(element, scalar)
                            .and_then(BaseType::get_array_type)
                            .ok_or_else(|| {
                                PgError::create(
                                    SqlState::UndefinedFunction,
                                    "array concatenation types are incompatible",
                                )
                            })
                    }
                    (None, None)
                        if matches!(
                            left_type,
                            BaseType::Text | BaseType::Varchar | BaseType::Bpchar
                        ) && matches!(
                            right_type,
                            BaseType::Text | BaseType::Varchar | BaseType::Bpchar
                        ) =>
                    {
                        Ok(BaseType::Text)
                    }
                    (None, None) => Err(PgError::create(
                        SqlState::UndefinedFunction,
                        "operator does not exist for argument types",
                    )),
                }
            }
            ast::BinaryOperator::AtArrow
            | ast::BinaryOperator::ArrowAt
            | ast::BinaryOperator::PGOverlap => {
                let data_type = resolve_operator_type(left, right, schema)?;
                let Some(element_type) = data_type.get_array_element_type() else {
                    return Err(PgError::create(
                        SqlState::UndefinedFunction,
                        "operator does not exist for argument types",
                    ));
                };
                validate_equality_type(element_type)?;
                Ok(BaseType::Bool)
            }
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
                if left_type == BaseType::PgLsn || right_type == BaseType::PgLsn {
                    return infer_pg_lsn_arithmetic_type(op, left_type, right_type);
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
            ast::BinaryOperator::PGRegexMatch
            | ast::BinaryOperator::PGRegexIMatch
            | ast::BinaryOperator::PGRegexNotMatch
            | ast::BinaryOperator::PGRegexNotIMatch => {
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
        ast::Expr::Between {
            expr,
            negated,
            low,
            high,
        } => infer_expression_type(
            &super::expand_between_expression(expr, low, high, *negated),
            schema,
        ),
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
            if let Ok(Some(element_type)) =
                infer_expression_type(right, schema).map(BaseType::get_array_element_type)
            {
                let left_type = infer_expression_type(left, schema)?;
                let Some(comparison_type) = coercion::resolve_common_type(left_type, element_type)
                else {
                    return Err(PgError::create(
                        SqlState::UndefinedFunction,
                        "operator does not exist for quantified comparison",
                    ));
                };
                validate_comparison_type(compare_op, comparison_type)?;
                return Ok(BaseType::Bool);
            }
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
            if let ast::Expr::Array(array) = expr.as_ref()
                && let Some(elem_type) = target.base.get_array_element_type()
            {
                for element in &array.elem {
                    if is_null_literal(element) || extract_unknown_string_literal(element).is_some()
                    {
                        continue;
                    }
                    let source = infer_expression_type(element, schema)?;
                    if !coercion::can_cast(source, elem_type, CastContext::Explicit) {
                        return Err(PgError::create(
                            SqlState::CannotCoerce,
                            "array element types cannot be cast",
                        ));
                    }
                }
                return Ok(target.base);
            }
            if target.base != BaseType::Regclass
                && target.base.get_array_element_type() != Some(BaseType::Regclass)
                && let Some(text) = extract_unknown_string_literal(expr)
            {
                coercion::coerce_unknown(text, target, CastContext::Explicit, "UTC")?;
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
pub(super) fn infer_array_type(array: &ast::Array, schema: RowScope<'_>) -> Result<BaseType> {
    if array.elem.is_empty() {
        return Err(PgError::create(
            SqlState::IndeterminateDatatype,
            "cannot determine type of empty array",
        ));
    }
    let elem_type = resolve_expression_list_type(&array.elem.iter().collect::<Vec<_>>(), schema)?;
    elem_type.get_array_type().ok_or_else(|| {
        PgError::create(
            SqlState::FeatureNotSupported,
            "array element type is not implemented",
        )
    })
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
