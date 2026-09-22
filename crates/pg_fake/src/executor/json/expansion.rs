use super::text::{
    create_json_result, decode_string, get_json_text, parse_elements, parse_object,
    validate_json_strings,
};
use crate::executor::normalize_function_name;
use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, Value},
};
use sqlparser::ast;

pub(super) fn is_json_expansion(name: &str) -> bool {
    matches!(
        name,
        "json_object_keys"
            | "jsonb_object_keys"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
    )
}

pub(crate) struct JsonTableFunction<'a> {
    pub(crate) name: String,
    pub(crate) argument: &'a ast::Expr,
    pub(crate) alias: Option<&'a ast::TableAlias>,
    pub(crate) ordinality: bool,
}

pub(crate) fn extract_json_table_function(
    factor: &ast::TableFactor,
) -> Result<Option<JsonTableFunction<'_>>> {
    let (name, args, alias, ordinality) = match factor {
        ast::TableFactor::Table {
            name,
            args: Some(args),
            alias,
            with_ordinality,
            ..
        } => {
            if args.settings.is_some() {
                return reject_unsupported("table function settings are not implemented");
            }
            (name, &args.args, alias.as_ref(), *with_ordinality)
        }
        ast::TableFactor::Function {
            name,
            args,
            alias,
            with_ordinality,
            ..
        } => (name, args, alias.as_ref(), *with_ordinality),
        _ => return Ok(None),
    };
    let name = normalize_function_name(name)?;
    if !is_json_expansion(&name) {
        if name == "unnest" {
            return Ok(None);
        }
        return reject_unsupported("table function is not implemented");
    }
    let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] = args.as_slice() else {
        return Err(PgError::create(
            SqlState::UndefinedFunction,
            "JSON table function signature does not exist",
        ));
    };
    Ok(Some(JsonTableFunction {
        name,
        argument,
        alias,
        ordinality,
    }))
}

pub(in crate::executor) fn describe_json_expansion(
    name: &str,
    ordinality: bool,
) -> Vec<(String, BaseType)> {
    let base = if name.ends_with("_text") || name.ends_with("object_keys") {
        BaseType::Text
    } else if name.starts_with("jsonb") {
        BaseType::Jsonb
    } else {
        BaseType::Json
    };
    let mut columns = if name.contains("_each") {
        vec![("key".into(), BaseType::Text), ("value".into(), base)]
    } else {
        vec![(
            if name.ends_with("object_keys") {
                name.into()
            } else {
                "value".into()
            },
            base,
        )]
    };
    if ordinality {
        columns.push(("ordinality".into(), BaseType::Int8));
    }
    columns
}

pub(in crate::executor) fn evaluate_json_expansion(
    name: &str,
    value: Value,
    ordinality: bool,
) -> Result<Vec<Vec<Value>>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let text = get_json_text(&value);
    let base = if name.ends_with("_text") {
        BaseType::Text
    } else if name.starts_with("jsonb") {
        BaseType::Jsonb
    } else {
        BaseType::Json
    };
    if matches!(value, Value::Json(_)) && text.starts_with('"') {
        decode_string(text)?;
    }
    if matches!(value, Value::Json(_))
        && (name.contains("array_elements") && text.starts_with('[')
            || !name.contains("array_elements") && text.starts_with('{'))
    {
        validate_json_strings(text)?;
    }
    let mut rows = if name.contains("array_elements") {
        if !text.starts_with('[') {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "cannot extract elements from a non-array",
            ));
        }
        parse_elements(text)?
            .into_iter()
            .map(|v| create_json_result(v.get(), base).map(|v| vec![v]))
            .collect::<Result<Vec<_>>>()?
    } else {
        if !text.starts_with('{') {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "cannot call JSON object function on a non-object",
            ));
        }
        parse_object(text)?
            .into_iter()
            .map(|(key, value)| {
                if name.ends_with("object_keys") {
                    Ok(vec![Value::Text(key)])
                } else {
                    Ok(vec![
                        Value::Text(key),
                        create_json_result(value.get(), base)?,
                    ])
                }
            })
            .collect::<Result<Vec<_>>>()?
    };
    if ordinality {
        for (index, row) in rows.iter_mut().enumerate() {
            row.push(Value::Int8(index as i64 + 1));
        }
    }
    Ok(rows)
}

pub(in crate::executor) fn contains_json_expansion(factor: &ast::TableFactor) -> bool {
    match factor {
        ast::TableFactor::Table { args: Some(_), .. } | ast::TableFactor::Function { .. } => true,
        ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            contains_json_expansion(&table_with_joins.relation)
                || table_with_joins
                    .joins
                    .iter()
                    .any(|join| contains_json_expansion(&join.relation))
        }
        _ => false,
    }
}
