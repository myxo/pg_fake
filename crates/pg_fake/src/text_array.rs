use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, Value},
};

pub(crate) fn parse_array(input: &str, elem_type: BaseType) -> Result<Vec<Value>> {
    let invalid = || {
        PgError::create(
            SqlState::InvalidTextRepresentation,
            "malformed array literal",
        )
    };
    let input = input.trim();
    if input.starts_with('[') {
        return reject_unsupported("explicit array bounds are not implemented");
    }
    let inner = input
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(invalid)?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut chars = inner.chars().peekable();
    let mut values = Vec::new();
    loop {
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        let quoted = chars.peek() == Some(&'"');
        let mut escaped = false;
        let mut value = String::new();
        if quoted {
            chars.next();
            loop {
                match chars.next().ok_or_else(invalid)? {
                    '"' => break,
                    '\\' => {
                        escaped = true;
                        value.push(chars.next().ok_or_else(invalid)?);
                    }
                    c => value.push(c),
                }
            }
            while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                chars.next();
            }
        } else {
            let mut trailing = 0;
            while chars.peek().is_some_and(|c| *c != ',') {
                match chars.next().expect("peeked character") {
                    '{' | '}' => {
                        return reject_unsupported("multidimensional arrays are not implemented");
                    }
                    '"' => return Err(invalid()),
                    '\\' => {
                        escaped = true;
                        value.push(chars.next().ok_or_else(invalid)?);
                        trailing = 0;
                    }
                    c => {
                        value.push(c);
                        if c.is_ascii_whitespace() {
                            trailing += 1;
                        } else {
                            trailing = 0;
                        }
                    }
                }
            }
            value.truncate(value.len() - trailing);
            if value.is_empty() {
                return Err(invalid());
            }
        }
        values.push(
            if !quoted && !escaped && value.eq_ignore_ascii_case("null") {
                Value::Null
            } else {
                Value::parse(elem_type, &value)?
            },
        );
        match chars.next() {
            None => break,
            Some(',') if chars.peek().is_some() => (),
            _ => return Err(invalid()),
        }
    }
    Ok(values)
}

pub(crate) fn format_array(values: &[Value]) -> String {
    format!(
        "{{{}}}",
        values
            .iter()
            .map(|value| match value {
                Value::Null => "NULL".to_owned(),
                value => {
                    let value = value.format_postgres_text();
                    if !value.is_empty()
                        && !value.eq_ignore_ascii_case("null")
                        && !value
                            .chars()
                            .any(|c| c.is_whitespace() || matches!(c, ',' | '{' | '}' | '"' | '\\'))
                    {
                        value
                    } else {
                        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}
