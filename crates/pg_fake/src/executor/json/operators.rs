use super::{
    paths::{modify_path, resolve_json_index},
    text::{
        JsonNode, create_json_result, decode_string, encode_string, get_json_text, parse_elements,
        parse_nodes, parse_object, validate_json_strings,
    },
};
use crate::executor::{
    expressions::{
        extract_unknown_string_literal, infer_expression_type, is_null_literal,
        validate_function_argument,
    },
    scope::RowScope,
};
use crate::{
    error::{PgError, Result, SqlState},
    value::{BaseType, Value},
};
use sqlparser::ast;

pub(crate) fn resolve_json_operator_types(
    op: &ast::BinaryOperator,
    left: Option<BaseType>,
    right: Option<BaseType>,
) -> Option<(BaseType, BaseType, BaseType)> {
    use ast::BinaryOperator::*;
    let input = if left == Some(BaseType::Json) {
        BaseType::Json
    } else {
        BaseType::Jsonb
    };
    Some(match op {
        Arrow | LongArrow => (
            input,
            if matches!(right, Some(BaseType::Int2 | BaseType::Int4)) {
                BaseType::Int4
            } else {
                BaseType::Text
            },
            if *op == LongArrow {
                BaseType::Text
            } else {
                input
            },
        ),
        HashArrow | HashLongArrow => (
            input,
            BaseType::Array(crate::value::ArrayElementType::Text),
            if *op == HashLongArrow {
                BaseType::Text
            } else {
                input
            },
        ),
        AtArrow | ArrowAt => (BaseType::Jsonb, BaseType::Jsonb, BaseType::Bool),
        Question => (BaseType::Jsonb, BaseType::Text, BaseType::Bool),
        QuestionAnd | QuestionPipe => (
            BaseType::Jsonb,
            BaseType::Array(crate::value::ArrayElementType::Text),
            BaseType::Bool,
        ),
        HashMinus => (
            BaseType::Jsonb,
            BaseType::Array(crate::value::ArrayElementType::Text),
            BaseType::Jsonb,
        ),
        StringConcat if left == Some(BaseType::Jsonb) || right == Some(BaseType::Jsonb) => {
            (BaseType::Jsonb, BaseType::Jsonb, BaseType::Jsonb)
        }
        Minus if left == Some(BaseType::Jsonb) => (
            BaseType::Jsonb,
            if matches!(right, Some(BaseType::Int2 | BaseType::Int4)) {
                BaseType::Int4
            } else {
                BaseType::Text
            },
            BaseType::Jsonb,
        ),
        _ => return None,
    })
}

pub(in crate::executor) fn infer_json_operator(
    op: &ast::BinaryOperator,
    left: &ast::Expr,
    right: &ast::Expr,
    scope: RowScope<'_>,
) -> Result<Option<(BaseType, BaseType, BaseType)>> {
    use ast::BinaryOperator::*;
    if !matches!(
        op,
        Arrow
            | LongArrow
            | HashArrow
            | HashLongArrow
            | AtArrow
            | ArrowAt
            | Question
            | QuestionAnd
            | QuestionPipe
            | HashMinus
            | StringConcat
            | Minus
    ) {
        return Ok(None);
    }
    let unknown_left = is_null_literal(left) || extract_unknown_string_literal(left).is_some();
    let unknown_right = is_null_literal(right) || extract_unknown_string_literal(right).is_some();
    if *op == StringConcat {
        let l = infer_expression_type(left, scope)?;
        let r = infer_expression_type(right, scope)?;
        if matches!(l, BaseType::Json | BaseType::Jsonb)
            && matches!(r, BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
            && !unknown_right
        {
            return Ok(Some((l, BaseType::Text, BaseType::Text)));
        }
        if matches!(r, BaseType::Json | BaseType::Jsonb)
            && matches!(l, BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
            && !unknown_left
        {
            return Ok(Some((BaseType::Text, r, BaseType::Text)));
        }
    }
    let types = resolve_json_operator_types(
        op,
        infer_expression_type(left, scope).ok(),
        infer_expression_type(right, scope).ok(),
    );
    if let Some((l, r, _)) = types {
        let error = || {
            PgError::create(
                SqlState::UndefinedFunction,
                format!("operator does not exist: {op}"),
            )
        };
        validate_function_argument(left, l, scope, &error)?;
        validate_function_argument(right, r, scope, &error)?;
        if unknown_left
            && (matches!(op, Arrow | LongArrow | HashArrow | HashLongArrow)
                || unknown_right && matches!(op, AtArrow | ArrowAt))
        {
            return Err(PgError::create(
                SqlState::AmbiguousFunction,
                "operator is not unique",
            ));
        }
    }
    Ok(types)
}

pub(in crate::executor) fn evaluate_json_operator(
    op: &ast::BinaryOperator,
    left: Value,
    right: Value,
    result: BaseType,
) -> Result<Value> {
    use ast::BinaryOperator::*;
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    if *op == StringConcat && result == BaseType::Text {
        return Ok(Value::Text(format!(
            "{}{}",
            left.format_postgres_text(),
            right.format_postgres_text()
        )));
    }
    let text = get_json_text(&left);
    if matches!(left, Value::Json(_)) {
        validate_json_strings(text)?;
    }
    match op {
        Arrow | LongArrow | HashArrow | HashLongArrow => {
            let (path, index_only, key_only) = match right {
                Value::Text(key) => (vec![Some(key)], false, true),
                Value::Int4(index) => (vec![Some(index.to_string())], true, false),
                Value::Array {
                    elem_type: BaseType::Text,
                    values,
                } => (
                    values
                        .into_iter()
                        .map(|value| match value {
                            Value::Null => None,
                            Value::Text(value) => Some(value),
                            _ => unreachable!(),
                        })
                        .collect(),
                    false,
                    false,
                ),
                _ => unreachable!("JSON path was coerced"),
            };
            let mut selected = text;
            for key in path {
                let Some(key) = key else {
                    return Ok(Value::Null);
                };
                selected = match selected.as_bytes()[0] {
                    b'{' if !index_only => match parse_object(selected)?
                        .into_iter()
                        .rev()
                        .find(|(k, _)| *k == key)
                    {
                        Some((_, v)) => v.get(),
                        None => return Ok(Value::Null),
                    },
                    b'[' if !key_only => {
                        let elements = parse_elements(selected)?;
                        match resolve_json_index(&key, elements.len()) {
                            Some(i) => elements[i].get(),
                            None => return Ok(Value::Null),
                        }
                    }
                    _ => return Ok(Value::Null),
                };
            }
            create_json_result(selected, result)
        }
        Question | QuestionAnd | QuestionPipe => {
            let keys = match right {
                Value::Text(key) => vec![Some(key)],
                Value::Array {
                    elem_type: BaseType::Text,
                    values,
                } => values
                    .into_iter()
                    .map(|value| match value {
                        Value::Null => None,
                        Value::Text(value) => Some(value),
                        _ => unreachable!(),
                    })
                    .collect(),
                _ => unreachable!(),
            };
            let candidates = match text.as_bytes()[0] {
                b'{' => parse_object(text)?
                    .into_iter()
                    .map(|(key, _)| Ok(key))
                    .collect::<Result<Vec<_>>>()?,
                b'[' => parse_elements(text)?
                    .into_iter()
                    .filter(|v| v.get().starts_with('"'))
                    .map(|v| decode_string(v.get()))
                    .collect::<Result<Vec<_>>>()?,
                b'"' => vec![decode_string(text)?],
                _ => Vec::new(),
            };
            let mut keys = keys.iter().flatten();
            Ok(Value::Bool(if *op == QuestionAnd {
                keys.all(|key| candidates.contains(key))
            } else {
                keys.any(|key| candidates.contains(key))
            }))
        }
        AtArrow | ArrowAt => {
            let other = get_json_text(&right);
            Ok(Value::Bool(if *op == AtArrow {
                check_containment(text, other)?
            } else {
                check_containment(other, text)?
            }))
        }
        StringConcat => {
            let other = get_json_text(&right);
            let output = if text.starts_with('{') && other.starts_with('{') {
                let entries = parse_object(text)?
                    .into_iter()
                    .chain(parse_object(other)?)
                    .map(|(key, value)| format!("{}:{}", encode_string(&key), value.get()))
                    .collect::<Vec<_>>();
                format!("{{{}}}", entries.join(","))
            } else {
                let mut values = if text.starts_with('[') {
                    parse_elements(text)?
                        .iter()
                        .map(|v| v.get())
                        .collect::<Vec<_>>()
                } else {
                    vec![text]
                };
                values.extend(if other.starts_with('[') {
                    parse_elements(other)?
                        .iter()
                        .map(|v| v.get())
                        .collect::<Vec<_>>()
                } else {
                    vec![other]
                });
                format!("[{}]", values.join(","))
            };
            Value::parse(BaseType::Jsonb, &output)
        }
        Minus => {
            let output = if text.starts_with('{') {
                let Value::Text(key) = right else {
                    return Err(PgError::create(
                        SqlState::InvalidParameterValue,
                        "cannot delete from object using integer index",
                    ));
                };
                format!(
                    "{{{}}}",
                    parse_object(text)?
                        .into_iter()
                        .filter(|(k, _)| *k != key)
                        .map(|(k, v)| format!("{}:{}", encode_string(&k), v.get()))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else if text.starts_with('[') {
                let elements = parse_elements(text)?;
                let index = if let Value::Int4(i) = &right {
                    resolve_json_index(&i.to_string(), elements.len())
                } else {
                    None
                };
                let mut kept = Vec::new();
                for (i, value) in elements.iter().enumerate() {
                    if Some(i) == index {
                        continue;
                    }
                    if let Value::Text(key) = &right
                        && value.get().starts_with('"')
                        && decode_string(value.get())? == *key
                    {
                        continue;
                    }
                    kept.push(value.get());
                }
                format!("[{}]", kept.join(","))
            } else {
                return Err(PgError::create(
                    SqlState::InvalidParameterValue,
                    "cannot delete from scalar",
                ));
            };
            Value::parse(BaseType::Jsonb, &output)
        }
        HashMinus => {
            let Value::Array {
                elem_type: BaseType::Text,
                values: path,
            } = right
            else {
                unreachable!()
            };
            let path = path
                .into_iter()
                .map(|value| match value {
                    Value::Null => None,
                    Value::Text(value) => Some(value),
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>();
            modify_path(text, &path, None, false)
        }
        _ => unreachable!("JSON operator was resolved"),
    }
}

fn check_containment(left: &str, right: &str) -> Result<bool> {
    let left = parse_nodes(left)?;
    let right = parse_nodes(right)?;
    let mut matches = vec![vec![false; right.len()]; left.len()];
    for l in (0..left.len()).rev() {
        for r in (0..right.len()).rev() {
            matches[l][r] = match (&left[l], &right[r]) {
                (JsonNode::Scalar(l), JsonNode::Scalar(r)) => {
                    crate::jsonb::Jsonb::parse(l)? == crate::jsonb::Jsonb::parse(r)?
                }
                (JsonNode::Array(l), JsonNode::Array(r)) => {
                    r.iter().all(|r| l.iter().any(|l| matches[*l][*r]))
                }
                (JsonNode::Object(l), JsonNode::Object(r)) => r
                    .iter()
                    .all(|(key, r)| l.iter().any(|(k, l)| k == key && matches[*l][*r])),
                _ => false,
            };
        }
    }
    Ok(matches[0][0]
        || matches!((&left[0], &right[0]), (JsonNode::Array(children), JsonNode::Scalar(_)) if children.iter().any(|i| matches[*i][0])))
}
