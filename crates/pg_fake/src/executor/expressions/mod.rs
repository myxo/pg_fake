use super::{
    StatementContext,
    arithmetic::{
        evaluate_boolean_operator, evaluate_distinctness, evaluate_numeric_operator,
        evaluate_pg_lsn_arithmetic, evaluate_temporal_arithmetic, evaluate_unary_operator,
    },
    json,
    scope::RowScope,
};
use crate::{
    catalog::{TableId, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType, Value},
};
use sqlparser::ast;

mod comparisons;
mod functions;
mod hashing;
mod literals;
mod patterns;
mod resume;
mod runtime;
mod temporal;
mod types;

pub(super) use comparisons::{
    compare_values, evaluate_comparison, validate_equality_type, validate_ordering_type,
};
pub(super) use functions::{infer_window_return_type, validate_function_argument};
pub(super) use literals::{evaluate_literal, extract_number_literal};
pub(crate) use literals::{
    extract_unknown_string_literal, is_null_literal, is_parameter_placeholder,
};
pub(crate) use resume::{EvaluationCursor, PendingEvaluation, PendingOperation};
pub(super) use resume::{
    EvaluationOperation, evaluate, evaluate_in_cursor, resume_evaluation, resume_operation,
};
pub(crate) use runtime::resolve_runtime_function;
pub(super) use types::resolve_operator_type;
pub(crate) use types::{infer_expression_data_type, infer_expression_type};

use comparisons::{evaluate_membership, evaluate_quantified};
use functions::{evaluate_function, extract_datetime_field};
use literals::parse_integer_literal;
use types::{infer_array_type, resolve_expression_list_type};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_assignment_expression(
    expr: &ast::Expr,
    target: PgType,
    schema: &TableSchema,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if let Some(text) = extract_unknown_string_literal(expr) {
        coercion::coerce_unknown(text, target, CastContext::Assignment, &context.timezone)
    } else {
        coercion::coerce(
            evaluate(expr, RowScope::Table(schema), row, context)?,
            infer_expression_type(expr, RowScope::Table(schema))?,
            target,
            CastContext::Assignment,
            &context.timezone,
        )
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn create_constant_expression_schema() -> TableSchema {
    TableSchema {
        id: TableId(0),
        schema_id: crate::catalog::SchemaId(0),
        name: String::new(),
        columns: Vec::new(),
        constraints: Vec::new(),
        indexes: Vec::new(),
        triggers: Vec::new(),
        persistence: crate::catalog::TablePersistence::Permanent,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_inner(
    expr: &ast::Expr,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    context.check_timeout()?;
    match expr {
        ast::Expr::Identifier(column) => {
            schema.resolve_column_value(std::slice::from_ref(column), row)
        }
        ast::Expr::CompoundIdentifier(columns) => schema.resolve_column_value(columns, row),
        ast::Expr::CompoundFieldAccess { root, access_chain } => {
            infer_expression_type(expr, schema)?;
            let [ast::AccessExpr::Subscript(ast::Subscript::Index { index })] =
                access_chain.as_slice()
            else {
                unreachable!("array access shape was type-checked");
            };
            let array = evaluate(root, schema, row, context)?;
            let index = evaluate_and_coerce(
                index,
                BaseType::Int4,
                CastContext::Assignment,
                schema,
                row,
                context,
            )?;
            let (
                Value::Array {
                    elem_type: BaseType::Int8,
                    values,
                },
                Value::Int4(index),
            ) = (array, index)
            else {
                return Ok(Value::Null);
            };
            Ok(index
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| values.get(index).cloned())
                .unwrap_or(Value::Null))
        }
        ast::Expr::TypedString(typed) if !typed.uses_odbc_syntax => {
            let base = infer_expression_type(expr, schema)?;
            let ast::Value::SingleQuotedString(text) = &typed.value.value else {
                unreachable!("typed literal was validated");
            };
            if base == BaseType::Regclass {
                return context
                    .sequences
                    .resolve_regclass(text)?
                    .map(Value::Regclass)
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedTable,
                            format!("relation {text:?} does not exist"),
                        )
                    });
            }
            coercion::coerce_unknown(
                text,
                PgType::create(base),
                CastContext::Explicit,
                &context.timezone,
            )
        }
        ast::Expr::Value(_) | ast::Expr::TypedString(_) => evaluate_literal(expr),
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
        ast::Expr::Array(array) => {
            let array_type = infer_array_type(array, schema)?;
            let elem_type = array_type
                .get_array_element_type()
                .expect("array type has an element type");
            evaluate_array(
                array,
                elem_type,
                CastContext::Implicit,
                schema,
                row,
                context,
            )
        }
        ast::Expr::Interval(interval)
            if interval.leading_field.is_none()
                && interval.leading_precision.is_none()
                && interval.last_field.is_none()
                && interval.fractional_seconds_precision.is_none() =>
        {
            let text = extract_unknown_string_literal(&interval.value)
                .expect("interval expression type was checked");
            Value::parse(BaseType::Interval, text)
        }
        ast::Expr::Floor { expr: value, .. } => {
            let target = infer_expression_type(expr, schema)?;
            runtime::evaluate_runtime_function("floor", &[value], &[target], schema, row, context)
        }
        ast::Expr::AtTimeZone {
            timestamp,
            time_zone,
        } => {
            let result = infer_expression_type(expr, schema)?;
            let target = if result == BaseType::Timestamp {
                BaseType::TimestampTz
            } else {
                BaseType::Timestamp
            };
            let timestamp = evaluate_and_coerce(
                timestamp,
                target,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let zone = evaluate_and_coerce(
                time_zone,
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let Value::Text(zone) = zone else {
                return Ok(Value::Null);
            };
            coercion::time_zones::convert_time_zone(timestamp, &zone)
        }
        ast::Expr::Like {
            expr: value,
            pattern,
            escape_char,
            negated,
            ..
        }
        | ast::Expr::ILike {
            expr: value,
            pattern,
            escape_char,
            negated,
            ..
        } => {
            infer_expression_type(expr, schema)?;
            let value = evaluate(value, schema, row, context)?;
            let pattern = evaluate_and_coerce(
                pattern,
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let escape = match escape_char.as_ref().map(|escape| &escape.value) {
                None => "\\",
                Some(ast::Value::SingleQuotedString(escape)) => escape,
                Some(ast::Value::Null) => return Ok(Value::Null),
                _ => return reject_unsupported("LIKE escape is not implemented"),
            };
            let (Value::Text(value), Value::Text(pattern)) = (value, pattern) else {
                return Ok(Value::Null);
            };
            let Value::Bool(matched) = patterns::evaluate_like(
                &value,
                &pattern,
                escape,
                matches!(expr, ast::Expr::ILike { .. }),
                context,
            )?
            else {
                unreachable!()
            };
            Ok(Value::Bool(matched != *negated))
        }
        ast::Expr::BinaryOp { left, op, right }
            if matches!(
                op,
                ast::BinaryOperator::PGRegexMatch
                    | ast::BinaryOperator::PGRegexIMatch
                    | ast::BinaryOperator::PGRegexNotMatch
                    | ast::BinaryOperator::PGRegexNotIMatch
            ) =>
        {
            infer_expression_type(expr, schema)?;
            let left = evaluate(left, schema, row, context)?;
            let right = evaluate_and_coerce(
                right,
                BaseType::Text,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let (Value::Text(value), Value::Text(pattern)) = (left, right) else {
                return Ok(Value::Null);
            };
            let flags = if matches!(
                op,
                ast::BinaryOperator::PGRegexIMatch | ast::BinaryOperator::PGRegexNotIMatch
            ) {
                "i"
            } else {
                ""
            };
            let Value::Bool(matched) = patterns::evaluate_regex(&value, &pattern, flags)? else {
                unreachable!()
            };
            Ok(Value::Bool(
                matched
                    != matches!(
                        op,
                        ast::BinaryOperator::PGRegexNotMatch
                            | ast::BinaryOperator::PGRegexNotIMatch
                    ),
            ))
        }
        ast::Expr::BinaryOp { left, op, right } => {
            if let Some((l, r, result)) = json::infer_json_operator(op, left, right, schema)? {
                return json::evaluate_json_operator(
                    op,
                    evaluate_and_coerce(left, l, CastContext::Implicit, schema, row, context)?,
                    evaluate_and_coerce(right, r, CastContext::Implicit, schema, row, context)?,
                    result,
                );
            }
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
            if matches!(op, ast::BinaryOperator::Plus | ast::BinaryOperator::Minus)
                && (left_type == BaseType::PgLsn || right_type == BaseType::PgLsn)
            {
                let left = evaluate(left, schema, row, context)?;
                let right = evaluate(right, schema, row, context)?;
                if left.is_null() || right.is_null() {
                    return Ok(Value::Null);
                }
                return evaluate_pg_lsn_arithmetic(op, left, right);
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
            if matches!(
                (op, &left),
                (ast::BinaryOperator::And, Value::Bool(false))
                    | (ast::BinaryOperator::Or, Value::Bool(true))
            ) {
                return Ok(left);
            }
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
            if target.base == BaseType::Regclass {
                if let Some(text) = extract_unknown_string_literal(expr) {
                    return context
                        .sequences
                        .resolve_regclass(text)?
                        .map(Value::Regclass)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedTable,
                                format!("relation {text:?} does not exist"),
                            )
                        });
                }
                let value = evaluate(expr, schema, row, context)?;
                if let Value::Text(text) = value {
                    return context
                        .sequences
                        .resolve_regclass(&text)?
                        .map(Value::Regclass)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedTable,
                                format!("relation {text:?} does not exist"),
                            )
                        });
                }
            }
            if target.base == BaseType::Text
                && infer_expression_type(expr, schema)? == BaseType::Regclass
            {
                return match evaluate(expr, schema, row, context)? {
                    Value::Regclass(crate::value::PgRegclass(oid)) => {
                        context.sequences.format_regclass(oid).map(Value::Text)
                    }
                    Value::Null => Ok(Value::Null),
                    _ => unreachable!("regclass expression has regclass value"),
                };
            }
            if let ast::Expr::Array(array) = expr.as_ref()
                && let Some(elem_type) = target.base.get_array_element_type()
            {
                return evaluate_array(
                    array,
                    elem_type,
                    CastContext::Explicit,
                    schema,
                    row,
                    context,
                );
            }
            if let Some(text) = extract_unknown_string_literal(expr) {
                coercion::coerce_unknown(text, target, CastContext::Explicit, &context.timezone)
            } else {
                coercion::coerce(
                    evaluate(expr, schema, row, context)?,
                    infer_expression_type(expr, schema)?,
                    target,
                    CastContext::Explicit,
                    &context.timezone,
                )
            }
        }
        ast::Expr::Extract { field, expr, .. } => {
            extract_datetime_field(field.clone(), evaluate(expr, schema, row, context)?)
        }
        _ => reject_unsupported("expression is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn evaluate_array(
    array: &ast::Array,
    elem_type: BaseType,
    cast_context: CastContext,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    let values = array
        .elem
        .iter()
        .map(|element| evaluate_and_coerce(element, elem_type, cast_context, schema, row, context))
        .collect::<Result<Vec<_>>>()?;
    Ok(Value::Array { elem_type, values })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_and_coerce(
    expression: &ast::Expr,
    target: BaseType,
    context: CastContext,
    schema: RowScope<'_>,
    row: &[Value],
    execution: &StatementContext,
) -> Result<Value> {
    if let ast::Expr::Array(array) = expression
        && let Some(elem_type) = target.get_array_element_type()
    {
        return evaluate_array(array, elem_type, context, schema, row, execution);
    }
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, PgType::create(target), context, &execution.timezone)
    } else {
        let source = infer_expression_type(expression, schema)?;
        coercion::coerce(
            evaluate(expression, schema, row, execution)?,
            source,
            PgType::create(target),
            context,
            &execution.timezone,
        )
    }
}
