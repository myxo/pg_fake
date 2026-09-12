use crate::{
    error::{PgError, Result, SqlState},
    value::{BaseType, Value},
};
use serde::de::Visitor;
use serde_json::value::RawValue;

fn convert_json_error(error: serde_json::Error) -> PgError {
    PgError::create(
        if error.to_string().contains("unsupported Unicode escape") {
            SqlState::UntranslatableCharacter
        } else {
            SqlState::InvalidTextRepresentation
        },
        error.to_string(),
    )
}

pub(super) fn parse_object(text: &str) -> Result<Vec<(String, &RawValue)>> {
    serde::Deserializer::deserialize_map(
        &mut serde_json::Deserializer::from_str(text),
        JsonObjectVisitor,
    )
    .map_err(convert_json_error)
}

pub(super) fn parse_elements(text: &str) -> Result<Vec<&RawValue>> {
    serde_json::from_str(text).map_err(convert_json_error)
}

pub(super) fn decode_string(text: &str) -> Result<String> {
    let value: String = serde_json::from_str(text).map_err(convert_json_error)?;
    if value.contains('\0') {
        return Err(PgError::create(
            SqlState::UntranslatableCharacter,
            "unsupported Unicode escape sequence",
        ));
    }
    Ok(value)
}

pub(super) fn encode_string(text: &str) -> String {
    serde_json::to_string(text).expect("strings serialize")
}

pub(super) fn get_json_text(value: &Value) -> &str {
    match value {
        Value::Json(text) => text.trim(),
        Value::Jsonb(value) => value.get_postgres_text(),
        _ => unreachable!("JSON argument was coerced"),
    }
}

pub(super) fn create_json_result(text: &str, base: BaseType) -> Result<Value> {
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

#[derive(Clone)]
pub(super) enum JsonNode<'a> {
    Scalar(&'a str),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
}

pub(super) fn parse_nodes(text: &str) -> Result<Vec<JsonNode<'_>>> {
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

pub(super) fn validate_json_strings(text: &str) -> Result<()> {
    for node in parse_nodes(text)? {
        if let JsonNode::Scalar(text) = node
            && text.starts_with('"')
        {
            decode_string(text)?;
        }
    }
    Ok(())
}

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
