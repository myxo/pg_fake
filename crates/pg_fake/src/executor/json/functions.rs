use super::{
    expansion::is_json_expansion,
    paths::modify_path,
    text::{encode_string, get_json_text, parse_elements},
};
use crate::executor::{
    StatementContext,
    expressions::{
        evaluate, evaluate_and_coerce, extract_unknown_string_literal, infer_expression_type,
        is_null_literal, validate_function_argument,
    },
    scope::RowScope,
};
use crate::{
    coercion::CastContext,
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, Value},
};
use sqlparser::ast;

pub(crate) fn resolve_json_function_arguments(name: &str) -> Option<Vec<BaseType>> {
    let base = if name.starts_with("jsonb_") {
        BaseType::Jsonb
    } else {
        BaseType::Json
    };
    if is_json_expansion(name)
        || matches!(
            name,
            "json_typeof" | "jsonb_typeof" | "json_array_length" | "jsonb_array_length"
        )
    {
        Some(vec![base])
    } else if name == "jsonb_set" {
        Some(vec![
            BaseType::Jsonb,
            BaseType::TextArray,
            BaseType::Jsonb,
            BaseType::Bool,
        ])
    } else {
        None
    }
}

pub(in crate::executor) fn infer_json_function(
    name: &str,
    arguments: &[&ast::Expr],
    scope: RowScope<'_>,
) -> Result<Option<BaseType>> {
    if is_json_expansion(name) {
        return reject_unsupported("set-returning JSON functions are supported only in FROM");
    }
    let base = if name.starts_with("jsonb_") || name == "to_jsonb" {
        BaseType::Jsonb
    } else {
        BaseType::Json
    };
    let error = || {
        PgError::create(
            SqlState::UndefinedFunction,
            format!("function {name} does not exist"),
        )
    };
    if let Some(targets) = resolve_json_function_arguments(name) {
        if arguments.len() != targets.len() && !(name == "jsonb_set" && arguments.len() == 3) {
            return Err(error());
        }
        for (argument, target) in arguments.iter().zip(targets) {
            validate_function_argument(argument, target, scope, &error)?;
        }
        return Ok(Some(if name.ends_with("typeof") {
            BaseType::Text
        } else if name.ends_with("length") {
            BaseType::Int4
        } else {
            base
        }));
    }
    if matches!(
        name,
        "json_build_object"
            | "jsonb_build_object"
            | "json_build_array"
            | "jsonb_build_array"
            | "to_json"
            | "to_jsonb"
    ) {
        if name.starts_with("to_") && arguments.len() != 1 {
            return Err(error());
        }
        for argument in arguments {
            if name.starts_with("to_")
                && (is_null_literal(argument) || extract_unknown_string_literal(argument).is_some())
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "could not determine polymorphic type because input has type unknown",
                ));
            }
            infer_expression_type(argument, scope)?;
        }
        return Ok(Some(base));
    }
    Ok(None)
}

fn encode_json_value(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => "null".into(),
        Value::Json(text) => text.clone(),
        Value::Jsonb(_) => get_json_text(value).into(),
        Value::Bool(value) => value.to_string(),
        Value::Int2(_) | Value::Int4(_) | Value::Int8(_) | Value::Numeric(_) => {
            value.format_postgres_text()
        }
        Value::Float4(v) if v.is_finite() => v.to_string(),
        Value::Float8(v) if v.is_finite() => v.to_string(),
        Value::Array { values, .. } => format!(
            "[{}]",
            values
                .iter()
                .map(encode_json_value)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Value::TimestampTz(crate::value::PgTimestampTz::Finite(_)) => encode_string(&format!(
            "{}:00",
            value.format_postgres_text().replacen(' ', "T", 1)
        )),
        Value::Timestamp(_) | Value::TimestampTz(_) => {
            encode_string(&value.format_postgres_text().replacen(' ', "T", 1))
        }
        _ => encode_string(&value.format_postgres_text()),
    })
}

pub(in crate::executor) fn evaluate_json_function(
    name: &str,
    arguments: &[&ast::Expr],
    base: BaseType,
    scope: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    let targets = resolve_json_function_arguments(name);
    let values = arguments
        .iter()
        .enumerate()
        .map(|(index, arg)| {
            if let Some(targets) = &targets {
                evaluate_and_coerce(
                    arg,
                    targets[index],
                    CastContext::Implicit,
                    scope,
                    row,
                    context,
                )
            } else {
                evaluate(arg, scope, row, context)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    if targets.is_some() && values.iter().any(Value::is_null)
        || name.starts_with("to_") && values[0].is_null()
    {
        return Ok(Value::Null);
    }
    if name.ends_with("typeof") {
        return Ok(Value::Text(
            match get_json_text(&values[0]).as_bytes()[0] {
                b'{' => "object",
                b'[' => "array",
                b'"' => "string",
                b't' | b'f' => "boolean",
                b'n' => "null",
                _ => "number",
            }
            .into(),
        ));
    }
    if name.ends_with("array_length") {
        let text = get_json_text(&values[0]);
        if !text.starts_with('[') {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "cannot get array length of a non-array",
            ));
        }
        return Ok(Value::Int4(
            i32::try_from(parse_elements(text)?.len()).expect("JSON array length fits i32"),
        ));
    }
    if name == "jsonb_set" {
        let Value::Array {
            elem_type: BaseType::Text,
            values: path,
        } = &values[1]
        else {
            unreachable!()
        };
        let path = path
            .iter()
            .map(|value| match value {
                Value::Null => None,
                Value::Text(value) => Some(value.clone()),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        return modify_path(
            get_json_text(&values[0]),
            &path,
            Some(get_json_text(&values[2])),
            values.get(3) != Some(&Value::Bool(false)),
        );
    }
    let text = if name.ends_with("build_object") {
        if values.len() % 2 != 0 {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "argument list must have even number of elements",
            ));
        }
        let mut entries = Vec::new();
        for pair in values.chunks_exact(2) {
            if pair[0].is_null() {
                return Err(PgError::create(
                    if base == BaseType::Json {
                        SqlState::NullValueNotAllowed
                    } else {
                        SqlState::InvalidParameterValue
                    },
                    "key must not be null",
                ));
            }
            if matches!(
                pair[0],
                Value::Json(_) | Value::Jsonb(_) | Value::Array { .. }
            ) {
                return Err(PgError::create(
                    SqlState::InvalidParameterValue,
                    "key value must be scalar",
                ));
            }
            let key = if let Value::Bool(v) = pair[0] {
                v.to_string()
            } else {
                pair[0].format_postgres_text()
            };
            entries.push(format!(
                "{} : {}",
                encode_string(&key),
                encode_json_value(&pair[1])?
            ));
        }
        format!("{{{}}}", entries.join(", "))
    } else if name.ends_with("build_array") {
        format!(
            "[{}]",
            values
                .iter()
                .map(encode_json_value)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        )
    } else {
        encode_json_value(&values[0])?
    };
    Value::parse(base, &text)
}
