use super::{
    comparisons::{evaluate_comparison, validate_equality_type, validate_ordering_type},
    evaluate, evaluate_and_coerce,
    literals::{extract_unknown_string_literal, is_null_literal},
    types::{infer_expression_type, is_numeric_type, resolve_expression_list_type},
};
use crate::executor::{
    StatementContext,
    aggregates::{infer_aggregate_return_type, is_aggregate_function},
    json, normalize_function_name,
    scope::RowScope,
};
use crate::{
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, Value},
};
use bigdecimal::{BigDecimal, num_bigint::BigInt};
use rand_chacha::rand_core::RngCore;
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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
    let arguments = match &function.args {
        ast::FunctionArguments::None => return Ok(Vec::new()),
        ast::FunctionArguments::List(arguments) => arguments,
        ast::FunctionArguments::Subquery(_) => {
            return Err(PgError::create(
                SqlState::UndefinedFunction,
                "function signature does not exist",
            ));
        }
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn infer_window_return_type(
    function: &ast::Function,
    schema: RowScope<'_>,
) -> Result<Option<BaseType>> {
    let Some(ast::WindowType::WindowSpec(window)) = &function.over else {
        if function.over.is_some() {
            return reject_unsupported("named windows are not implemented");
        }
        return Ok(None);
    };
    let name = normalize_function_name(&function.name)?;
    if function.uses_odbc_syntax
        || !matches!(function.parameters, ast::FunctionArguments::None)
        || function.filter.is_some()
        || function.null_treatment.is_some()
        || !function.within_group.is_empty()
        || window.window_name.is_some()
        || window.window_frame.is_some()
    {
        return reject_unsupported("window function feature is not implemented");
    }
    match name.as_str() {
        "row_number" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function row_number does not exist",
                ));
            };
            if !arguments.args.is_empty()
                || !arguments.clauses.is_empty()
                || arguments.duplicate_treatment.is_some()
                || !window.partition_by.is_empty()
                || window.order_by.is_empty()
            {
                return reject_unsupported("row_number window shape is not implemented");
            }
            for order in &window.order_by {
                if order.with_fill.is_some()
                    || matches!(order.options.sort, Some(ast::OrderBySort::Using(_)))
                {
                    return reject_unsupported("window order feature is not implemented");
                }
                validate_ordering_type(infer_expression_type(&order.expr, schema)?)?;
            }
            Ok(Some(BaseType::Int8))
        }
        "count" => {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return Err(PgError::create(
                    SqlState::UndefinedFunction,
                    "function count does not exist",
                ));
            };
            if arguments.args.is_empty() {
                return Err(PgError::create(
                    SqlState::WrongObjectType,
                    "count requires an argument or wildcard",
                ));
            }
            if !matches!(
                arguments.args.as_slice(),
                [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Wildcard)]
            ) || !arguments.clauses.is_empty()
                || arguments.duplicate_treatment.is_some()
                || window.partition_by.len() != 1
                || !window.order_by.is_empty()
            {
                return reject_unsupported("count window shape is not implemented");
            }
            validate_equality_type(infer_expression_type(&window.partition_by[0], schema)?)?;
            Ok(Some(BaseType::Int8))
        }
        _ => Err(PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_function_return_type(
    function: &ast::Function,
    schema: RowScope<'_>,
) -> Result<BaseType> {
    if let Some(result) = infer_window_return_type(function, schema)? {
        return Ok(result);
    }
    if let Some(result) = infer_aggregate_return_type(function, schema)? {
        return Ok(result);
    }
    let function_name = normalize_function_name(&function.name)?;
    let arguments = extract_function_arguments(function)?;
    let signature_error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )
    };
    if let Some(base) = json::infer_json_function(&function_name, &arguments, schema)? {
        return Ok(base);
    }
    if let Some((_, result)) =
        super::runtime::infer_runtime_function(&function_name, &arguments, schema)?
    {
        return Ok(result);
    }
    match function_name.as_str() {
        "coalesce" if !arguments.is_empty() => resolve_expression_list_type(&arguments, schema),
        "greatest" | "least" if !arguments.is_empty() => {
            let data_type = resolve_expression_list_type(&arguments, schema)?;
            validate_ordering_type(data_type)?;
            Ok(data_type)
        }
        "nullif" if arguments.len() == 2 => {
            let data_type = resolve_expression_list_type(&arguments, schema)?;
            validate_equality_type(data_type)?;
            Ok(data_type)
        }
        "length" | "lower" | "upper" | "btrim" if arguments.len() == 1 => {
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
        "pg_is_in_recovery" if arguments.is_empty() => Ok(BaseType::Bool),
        "now"
        | "current_timestamp"
        | "transaction_timestamp"
        | "statement_timestamp"
        | "clock_timestamp"
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
        "to_regclass" if arguments.len() == 1 => {
            validate_function_argument(arguments[0], BaseType::Text, schema, &signature_error)?;
            Ok(BaseType::Regclass)
        }
        "format_type" if arguments.len() == 2 => {
            let oid = infer_expression_type(arguments[0], schema)?;
            let typmod = infer_expression_type(arguments[1], schema)?;
            if !matches!(
                oid,
                BaseType::Oid | BaseType::Int4 | BaseType::Int8 | BaseType::Regclass
            ) || typmod != BaseType::Int4
            {
                return Err(signature_error());
            }
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
        | "btrim"
        | "abs"
        | "nextval"
        | "currval"
        | "lastval"
        | "setval"
        | "pg_get_serial_sequence"
        | "pg_is_in_recovery"
        | "to_regclass"
        | "format_type" => Err(signature_error()),
        _ => Err(PgError::create(
            SqlState::UndefinedFunction,
            format!("function {function_name} does not exist"),
        )),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn validate_function_argument(
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_function(
    function: &ast::Function,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if infer_window_return_type(function, schema)?.is_some() {
        return Err(PgError::create(
            SqlState::GroupingError,
            "window function is not allowed in this context",
        ));
    }
    if is_aggregate_function(function) {
        infer_aggregate_return_type(function, schema)?;
        return Err(PgError::create(
            SqlState::GroupingError,
            "aggregate function is not allowed in this context",
        ));
    }
    infer_function_return_type(function, schema)?;
    let function_name = normalize_function_name(&function.name)?;
    let arguments = extract_function_arguments(function)?;
    let result_type = infer_function_return_type(function, schema)?;
    if json::infer_json_function(&function_name, &arguments, schema)?.is_some() {
        return json::evaluate_json_function(
            &function_name,
            &arguments,
            result_type,
            schema,
            row,
            context,
        );
    }
    if let Some((targets, _)) =
        super::runtime::infer_runtime_function(&function_name, &arguments, schema)?
    {
        return super::runtime::evaluate_runtime_function(
            &function_name,
            &arguments,
            &targets,
            schema,
            row,
            context,
        );
    }
    match function_name.as_str() {
        "pg_is_in_recovery" => Ok(Value::Bool(false)),
        "to_regclass" => {
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
            Ok(context
                .sequences
                .resolve_regclass_lenient(&name)?
                .map(Value::Regclass)
                .unwrap_or(Value::Null))
        }
        "format_type" => {
            let oid = evaluate(arguments[0], schema, row, context)?;
            let typmod = evaluate_and_coerce(
                arguments[1],
                BaseType::Int4,
                CastContext::Implicit,
                schema,
                row,
                context,
            )?;
            let oid = match oid {
                Value::Oid(oid) => oid,
                Value::Int4(oid) => oid as u32,
                Value::Int8(oid) => u32::try_from(oid).map_err(|_| {
                    PgError::create(SqlState::NumericValueOutOfRange, "OID out of range")
                })?,
                Value::Regclass(crate::value::PgRegclass(oid)) => oid,
                Value::Null => return Ok(Value::Null),
                _ => unreachable!("format_type arguments were type-checked"),
            };
            let Value::Int4(typmod) = typmod else {
                return Ok(Value::Null);
            };
            Ok(Value::Text(super::super::format_type(oid, typmod)?))
        }
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
        "now"
        | "current_timestamp"
        | "transaction_timestamp"
        | "statement_timestamp"
        | "clock_timestamp" => {
            let value = match function_name.as_str() {
                "now" | "current_timestamp" | "transaction_timestamp" => {
                    context.transaction_timestamp
                }
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
            Value::Text(value) => {
                let value = if infer_expression_type(arguments[0], schema)? == BaseType::Bpchar {
                    value.trim_end_matches(' ')
                } else {
                    &value
                };
                Ok(Value::Int4(
                    i32::try_from(value.chars().count()).expect("text length must fit in int4"),
                ))
            }
            _ => unreachable!("length argument was type-checked"),
        },
        "lower" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => {
                let value = if infer_expression_type(arguments[0], schema)? == BaseType::Bpchar {
                    value.trim_end_matches(' ')
                } else {
                    &value
                };
                Ok(Value::Text(value.to_lowercase()))
            }
            _ => unreachable!("lower argument was type-checked"),
        },
        "upper" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => {
                let value = if infer_expression_type(arguments[0], schema)? == BaseType::Bpchar {
                    value.trim_end_matches(' ')
                } else {
                    &value
                };
                Ok(Value::Text(value.to_uppercase()))
            }
            _ => unreachable!("upper argument was type-checked"),
        },
        "btrim" => match evaluate(arguments[0], schema, row, context)? {
            Value::Null => Ok(Value::Null),
            Value::Text(value) => Ok(Value::Text(value.trim_matches(' ').into())),
            _ => unreachable!("btrim argument was type-checked"),
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn extract_datetime_field(field: ast::DateTimeField, value: Value) -> Result<Value> {
    use chrono::{Datelike, Timelike};
    let value = match value {
        Value::Null => return Ok(Value::Null),
        Value::Date(crate::value::PgDate::Finite(value)) => match field {
            ast::DateTimeField::Year => value.year() as i64,
            ast::DateTimeField::Month => i64::from(value.month()),
            ast::DateTimeField::Day => i64::from(value.day()),
            ast::DateTimeField::Dow => i64::from(value.weekday().num_days_from_sunday()),
            ast::DateTimeField::Doy => i64::from(value.ordinal()),
            ast::DateTimeField::Epoch => value
                .and_hms_opt(0, 0, 0)
                .expect("midnight is valid")
                .and_utc()
                .timestamp(),
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
            ast::DateTimeField::Epoch => {
                return Ok(convert_epoch_to_numeric(
                    value.and_utc().timestamp(),
                    value.and_utc().timestamp_subsec_micros(),
                ));
            }
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
            ast::DateTimeField::Epoch => {
                return Ok(convert_epoch_to_numeric(
                    value.timestamp(),
                    value.timestamp_subsec_micros(),
                ));
            }
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn convert_epoch_to_numeric(seconds: i64, subsecond_micros: u32) -> Value {
    Value::Numeric(BigDecimal::from(seconds) + BigDecimal::new(BigInt::from(subsecond_micros), 6))
}
