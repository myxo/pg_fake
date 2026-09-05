use super::expressions::validate_function_argument;
use super::*;
use serde::de::Visitor;
use serde_json::value::RawValue;

struct JsonObjectVisitor;
impl<'de> Visitor<'de> for JsonObjectVisitor {
    type Value = Vec<(String, &'de RawValue)>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JSON object")
    }
    fn visit_map<M: serde::de::MapAccess<'de>>(
        self,
        mut map: M,
    ) -> std::result::Result<Self::Value, M::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry::<String, &RawValue>()? {
            if entry.0.contains('\0') {
                return Err(serde::de::Error::custom(
                    "unsupported Unicode escape sequence",
                ));
            }
            entries.push(entry);
        }
        Ok(entries)
    }
}

fn parse_json_error(error: serde_json::Error) -> PgError {
    PgError::create(
        if error.to_string().contains("unsupported Unicode escape") {
            SqlState::UntranslatableCharacter
        } else {
            SqlState::InvalidTextRepresentation
        },
        error.to_string(),
    )
}
fn parse_object(text: &str) -> Result<Vec<(String, &RawValue)>> {
    serde::Deserializer::deserialize_map(
        &mut serde_json::Deserializer::from_str(text),
        JsonObjectVisitor,
    )
    .map_err(parse_json_error)
}
fn parse_elements(text: &str) -> Result<Vec<&RawValue>> {
    serde_json::from_str(text).map_err(parse_json_error)
}
fn decode_string(text: &str) -> Result<String> {
    let value: String = serde_json::from_str(text).map_err(parse_json_error)?;
    if value.contains('\0') {
        return Err(PgError::create(
            SqlState::UntranslatableCharacter,
            "unsupported Unicode escape sequence",
        ));
    }
    Ok(value)
}
fn encode_string(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}
fn get_json_text(value: &Value) -> &str {
    match value {
        Value::Json(text) => text.trim(),
        Value::Jsonb(value) => value.get_postgres_text(),
        _ => unreachable!("JSON argument was coerced"),
    }
}
fn create_json_result(text: &str, base: BaseType) -> Result<Value> {
    if base == BaseType::Text {
        Ok(if text == "null" {
            Value::Null
        } else if text.starts_with('"') {
            Value::Text(decode_string(text)?)
        } else {
            Value::Text(text.to_owned())
        })
    } else {
        Value::parse(base, text)
    }
}
fn resolve_json_index(text: &str, length: usize) -> Option<usize> {
    let index = text
        .trim_start_matches(|c: char| c.is_ascii_whitespace())
        .parse::<i32>()
        .ok()?;
    let index = if index < 0 {
        length as i64 + index as i64
    } else {
        index as i64
    };
    (index >= 0 && index < length as i64).then_some(index as usize)
}

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
            BaseType::TextArray,
            if *op == HashLongArrow {
                BaseType::Text
            } else {
                input
            },
        ),
        AtArrow | ArrowAt => (BaseType::Jsonb, BaseType::Jsonb, BaseType::Bool),
        Question => (BaseType::Jsonb, BaseType::Text, BaseType::Bool),
        QuestionAnd | QuestionPipe => (BaseType::Jsonb, BaseType::TextArray, BaseType::Bool),
        HashMinus => (BaseType::Jsonb, BaseType::TextArray, BaseType::Jsonb),
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

pub(super) fn infer_json_operator(
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

pub(super) fn evaluate_json_operator(
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
                Value::TextArray(path) => (path, false, false),
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
                Value::TextArray(keys) => keys,
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
            let Value::TextArray(path) = right else {
                unreachable!()
            };
            mutate_path(text, &path, None, false)
        }
        _ => unreachable!("JSON operator was resolved"),
    }
}

#[derive(Clone)]
enum JsonNode<'a> {
    Scalar(&'a str),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
}
fn parse_nodes(text: &str) -> Result<Vec<JsonNode<'_>>> {
    let mut nodes = vec![JsonNode::Scalar(text)];
    let mut pending = vec![(0, text)];
    while let Some((index, text)) = pending.pop() {
        nodes[index] = if text.starts_with('[') {
            let mut children = Vec::new();
            for value in parse_elements(text)? {
                let slot = nodes.len();
                nodes.push(JsonNode::Scalar(value.get()));
                children.push(slot);
                pending.push((slot, value.get()));
            }
            JsonNode::Array(children)
        } else if text.starts_with('{') {
            let mut children = Vec::new();
            for (key, value) in parse_object(text)? {
                let slot = nodes.len();
                nodes.push(JsonNode::Scalar(value.get()));
                children.push((key, slot));
                pending.push((slot, value.get()));
            }
            JsonNode::Object(children)
        } else {
            JsonNode::Scalar(text)
        };
    }
    Ok(nodes)
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

fn mutate_path(
    text: &str,
    path: &[Option<String>],
    replacement: Option<&str>,
    create: bool,
) -> Result<Value> {
    if !text.starts_with(['{', '[']) {
        return Err(PgError::create(
            SqlState::InvalidParameterValue,
            "cannot modify scalar JSONB",
        ));
    }
    if path.is_empty() || !create && matches!(text, "{}" | "[]") {
        return Value::parse(BaseType::Jsonb, text);
    }
    let mut current = text;
    let mut ancestors = Vec::<(String, String)>::new();
    for (depth, key) in path.iter().enumerate() {
        let key = key.as_ref().ok_or_else(|| {
            PgError::create(
                SqlState::NullValueNotAllowed,
                "path element must not be null",
            )
        })?;
        let last = depth + 1 == path.len();
        if current.starts_with('{') {
            let entries = parse_object(current)?;
            let found = entries.iter().position(|(k, _)| k == key);
            if last {
                let mut parts = entries
                    .iter()
                    .filter(|(k, _)| k != key || replacement.is_some())
                    .map(|(k, v)| {
                        format!(
                            "{}:{}",
                            encode_string(k),
                            if k == key {
                                replacement.unwrap_or(v.get())
                            } else {
                                v.get()
                            }
                        )
                    })
                    .collect::<Vec<_>>();
                if found.is_none()
                    && create
                    && let Some(value) = replacement
                {
                    parts.push(format!("{}:{value}", encode_string(key)));
                }
                current = "";
                ancestors.push((format!("{{{}}}", parts.join(",")), String::new()));
                break;
            }
            let Some(index) = found else {
                return Value::parse(BaseType::Jsonb, text);
            };
            let before = entries[..index]
                .iter()
                .map(|(k, v)| format!("{}:{},", encode_string(k), v.get()))
                .collect::<String>();
            let after = entries[index + 1..]
                .iter()
                .map(|(k, v)| format!(",{}:{}", encode_string(k), v.get()))
                .collect::<String>();
            ancestors.push((
                format!("{{{before}{}:", encode_string(key)),
                format!("{after}}}"),
            ));
            current = entries[index].1.get();
        } else if current.starts_with('[') {
            let elements = parse_elements(current)?;
            let index: i32 = key
                .trim_start_matches(|c: char| c.is_ascii_whitespace())
                .parse()
                .map_err(|_| {
                    PgError::create(
                        SqlState::InvalidTextRepresentation,
                        "path element is not an integer",
                    )
                })?;
            let slot = resolve_json_index(key, elements.len());
            if last {
                let mut parts = elements.iter().map(|v| v.get()).collect::<Vec<_>>();
                if let Some(slot) = slot {
                    if let Some(value) = replacement {
                        parts[slot] = value;
                    } else {
                        parts.remove(slot);
                    }
                } else if create && let Some(value) = replacement {
                    if index < 0 {
                        parts.insert(0, value);
                    } else {
                        parts.push(value);
                    }
                }
                current = "";
                ancestors.push((format!("[{}]", parts.join(",")), String::new()));
                break;
            }
            let Some(slot) = slot else {
                return Value::parse(BaseType::Jsonb, text);
            };
            ancestors.push((
                format!(
                    "[{}",
                    elements[..slot]
                        .iter()
                        .map(|v| format!("{},", v.get()))
                        .collect::<String>()
                ),
                format!(
                    "{}]",
                    elements[slot + 1..]
                        .iter()
                        .map(|v| format!(",{}", v.get()))
                        .collect::<String>()
                ),
            ));
            current = elements[slot].get();
        } else {
            return Value::parse(BaseType::Jsonb, text);
        }
    }
    let mut output = current.to_owned();
    for (before, after) in ancestors.into_iter().rev() {
        output = format!("{before}{output}{after}");
    }
    Value::parse(BaseType::Jsonb, &output)
}

pub(crate) fn is_json_expansion(name: &str) -> bool {
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

pub(super) fn infer_json_function(
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

fn convert_json_value(value: &Value) -> Result<String> {
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
        Value::TextArray(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|v| v
                    .as_ref()
                    .map(|v| encode_string(v))
                    .unwrap_or_else(|| "null".into()))
                .collect::<Vec<_>>()
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

pub(super) fn evaluate_json_function(
    name: &str,
    arguments: &[&ast::Expr],
    base: BaseType,
    scope: RowScope<'_>,
    row: &[Value],
    context: &StatementExecutionContext,
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
        let Value::TextArray(path) = &values[1] else {
            unreachable!()
        };
        return mutate_path(
            get_json_text(&values[0]),
            path,
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
                Value::Json(_) | Value::Jsonb(_) | Value::TextArray(_)
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
                convert_json_value(&pair[1])?
            ));
        }
        format!("{{{}}}", entries.join(", "))
    } else if name.ends_with("build_array") {
        format!(
            "[{}]",
            values
                .iter()
                .map(convert_json_value)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        )
    } else {
        convert_json_value(&values[0])?
    };
    Value::parse(base, &text)
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
    let name = normalize_unqualified_object_name(name)?;
    if !is_json_expansion(&name) {
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

pub(super) fn describe_json_expansion(name: &str, ordinality: bool) -> Vec<(String, BaseType)> {
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

pub(super) fn evaluate_json_expansion(
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

fn validate_json_strings(text: &str) -> Result<()> {
    for node in parse_nodes(text)? {
        if let JsonNode::Scalar(text) = node
            && text.starts_with('"')
        {
            decode_string(text)?;
        }
    }
    Ok(())
}

pub(super) fn contains_json_expansion(factor: &ast::TableFactor) -> bool {
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
