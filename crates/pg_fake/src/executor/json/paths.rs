use super::text::{encode_string, parse_elements, parse_object};
use crate::{
    error::{PgError, Result, SqlState},
    value::{BaseType, Value},
};

pub(super) fn resolve_json_index(text: &str, length: usize) -> Option<usize> {
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

pub(super) fn modify_path(
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
