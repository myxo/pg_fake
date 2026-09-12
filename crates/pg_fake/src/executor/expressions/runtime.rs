use super::{
    evaluate_and_coerce, extract_unknown_string_literal, infer_expression_type, is_null_literal,
};
use crate::{
    coercion::CastContext,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{StatementContext, scope::RowScope},
    value::{BaseType, Value},
};
use sqlparser::ast;

pub(crate) fn resolve_runtime_function(
    name: &str,
    arguments: &[Option<BaseType>],
) -> Option<Result<(Vec<BaseType>, BaseType)>> {
    use BaseType::*;
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )
    };
    let ambiguous_error = || {
        PgError::create(
            SqlState::AmbiguousFunction,
            format!("function {name} is not unique"),
        )
    };
    Some(match (name, arguments) {
        ("to_timestamp", [_]) => Ok((vec![Float8], TimestampTz)),
        ("to_timestamp", [_, _]) => {
            reject_unsupported("formatted timestamp input is not implemented")
        }
        ("floor", [argument]) => {
            let target = if *argument == Some(Numeric) {
                Numeric
            } else {
                Float8
            };
            Ok((vec![target], target))
        }
        ("to_char", [argument, _]) => match argument {
            Some(Timestamp) => Ok((vec![Timestamp, Text], Text)),
            Some(TimestampTz | Date) => Ok((vec![TimestampTz, Text], Text)),
            Some(Numeric | Float4 | Float8 | Int2 | Int4 | Int8 | Interval) => {
                reject_unsupported("to_char overload is not implemented")
            }
            None => Err(ambiguous_error()),
            _ => Err(signature_error()),
        },
        ("date_trunc", [_, argument]) => match argument {
            Some(Timestamp) => Ok((vec![Text, Timestamp], Timestamp)),
            Some(TimestampTz | Date) => Ok((vec![Text, TimestampTz], TimestampTz)),
            Some(Interval) => reject_unsupported("interval truncation is not implemented"),
            None => Err(ambiguous_error()),
            _ => Err(signature_error()),
        },
        ("date_trunc", [_, _, _]) => Ok((vec![Text, TimestampTz, Text], TimestampTz)),
        ("regexp_like", [_, _]) => Ok((vec![Text, Text], Bool)),
        ("regexp_like", [_, _, _]) => Ok((vec![Text, Text, Text], Bool)),
        ("to_timestamp" | "floor" | "to_char" | "date_trunc" | "regexp_like", _) => {
            Err(signature_error())
        }
        _ => return None,
    })
}

pub(super) fn infer_runtime_function(
    name: &str,
    arguments: &[&ast::Expr],
    schema: RowScope<'_>,
) -> Result<Option<(Vec<BaseType>, BaseType)>> {
    if !matches!(
        name,
        "to_timestamp" | "floor" | "to_char" | "date_trunc" | "regexp_like"
    ) {
        return Ok(None);
    }
    let types = arguments
        .iter()
        .map(|argument| {
            if is_null_literal(argument) || extract_unknown_string_literal(argument).is_some() {
                Ok(None)
            } else {
                infer_expression_type(argument, schema).map(Some)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let (targets, result) =
        resolve_runtime_function(name, &types).expect("runtime function was recognized")?;
    for (argument, target) in arguments.iter().zip(&targets) {
        super::validate_function_argument(argument, *target, schema, &|| {
            PgError::create(
                SqlState::UndefinedFunction,
                format!("function {name} does not exist"),
            )
        })?;
    }
    Ok(Some((targets, result)))
}

pub(super) fn evaluate_runtime_function(
    name: &str,
    arguments: &[&ast::Expr],
    targets: &[BaseType],
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    let values = arguments
        .iter()
        .zip(targets)
        .map(|(argument, target)| {
            evaluate_and_coerce(
                argument,
                *target,
                CastContext::Implicit,
                schema,
                row,
                context,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    if values.iter().any(Value::is_null) {
        return Ok(Value::Null);
    }
    match (name, values.as_slice()) {
        ("floor", [Value::Float8(value)]) => Ok(Value::Float8(value.floor())),
        ("floor", [Value::Numeric(value)]) => Ok(Value::Numeric(
            value.with_scale_round(0, bigdecimal::RoundingMode::Floor),
        )),
        ("to_timestamp", [Value::Float8(seconds)]) => super::temporal::convert_epoch(*seconds),
        ("to_char", [value, Value::Text(format)]) => {
            super::temporal::format_timestamp(value, format, &context.timezone)
        }
        ("date_trunc", [Value::Text(unit), value]) => {
            super::temporal::truncate_timestamp(unit, value.clone(), &context.timezone)
        }
        ("date_trunc", [Value::Text(unit), value, Value::Text(zone)]) => {
            super::temporal::truncate_timestamp(unit, value.clone(), zone)
        }
        ("regexp_like", [Value::Text(value), Value::Text(pattern)]) => {
            super::patterns::evaluate_regex(value, pattern, "")
        }
        ("regexp_like", [Value::Text(value), Value::Text(pattern), Value::Text(flags)]) => {
            super::patterns::evaluate_regex(value, pattern, flags)
        }
        _ => unreachable!("runtime function arguments were coerced"),
    }
}
