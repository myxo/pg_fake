use std::{cmp::Ordering, str::FromStr};

use bigdecimal::{BigDecimal, Zero};
use serde::de::Visitor;
use serde_json::value::RawValue;

use crate::error::{PgError, Result, SqlState};

#[derive(Debug, Clone)]
pub struct Jsonb {
    tokens: Vec<JsonbToken>,
    text: String,
}

impl PartialEq for Jsonb {
    fn eq(&self, other: &Self) -> bool {
        self.tokens == other.tokens
    }
}

impl Eq for Jsonb {}

impl PartialOrd for Jsonb {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Jsonb {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare(other)
    }
}

impl std::hash::Hash for Jsonb {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.tokens.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum JsonbToken {
    Null,
    String(String),
    Numeric(BigDecimal),
    Bool(bool),
    Array(usize),
    Object(usize),
    EndArray,
    EndObject,
}

enum JsonbNode {
    Scalar(JsonbToken),
    Array(Vec<usize>),
    Object(Vec<(String, usize)>),
}

struct JsonbObjectVisitor;

impl<'de> Visitor<'de> for JsonbObjectVisitor {
    type Value = Vec<(String, &'de RawValue)>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(
        self,
        mut map: M,
    ) -> std::result::Result<Self::Value, M::Error> {
        let mut pairs = Vec::new();
        while let Some(pair) = map.next_entry()? {
            pairs.push(pair);
        }
        Ok(pairs)
    }
}

impl Jsonb {
    pub fn parse(input: &str) -> Result<Self> {
        let root: &RawValue = serde_json::from_str(input).map_err(create_jsonb_input_error)?;
        let mut nodes = vec![JsonbNode::Scalar(JsonbToken::Null)];
        let mut pending = vec![(0, root)];
        while let Some((index, raw)) = pending.pop() {
            let text = raw.get();
            nodes[index] = match text.as_bytes()[0] {
                b'[' => {
                    let children: Vec<&RawValue> =
                        serde_json::from_str(text).map_err(create_jsonb_input_error)?;
                    let mut slots = Vec::with_capacity(children.len());
                    let start = pending.len();
                    for child in children {
                        let slot = nodes.len();
                        nodes.push(JsonbNode::Scalar(JsonbToken::Null));
                        slots.push(slot);
                        pending.push((slot, child));
                    }
                    pending[start..].reverse();
                    JsonbNode::Array(slots)
                }
                b'{' => {
                    let mut deserializer = serde_json::Deserializer::from_str(text);
                    let children =
                        serde::Deserializer::deserialize_map(&mut deserializer, JsonbObjectVisitor)
                            .map_err(create_jsonb_input_error)?;
                    let mut pairs = Vec::with_capacity(children.len());
                    let start = pending.len();
                    for (key, child) in children {
                        validate_jsonb_string(&key)?;
                        let slot = nodes.len();
                        nodes.push(JsonbNode::Scalar(JsonbToken::Null));
                        pairs.push((key, slot));
                        pending.push((slot, child));
                    }
                    pending[start..].reverse();
                    pairs.sort_by(|(left, left_slot), (right, right_slot)| {
                        left.len()
                            .cmp(&right.len())
                            .then_with(|| left.cmp(right))
                            .then_with(|| right_slot.cmp(left_slot))
                    });
                    pairs.dedup_by(|(left, _), (right, _)| left == right);
                    JsonbNode::Object(pairs)
                }
                b'"' => {
                    let value: String =
                        serde_json::from_str(text).map_err(create_jsonb_input_error)?;
                    validate_jsonb_string(&value)?;
                    JsonbNode::Scalar(JsonbToken::String(value))
                }
                b'n' => JsonbNode::Scalar(JsonbToken::Null),
                b't' => JsonbNode::Scalar(JsonbToken::Bool(true)),
                b'f' => JsonbNode::Scalar(JsonbToken::Bool(false)),
                _ => {
                    if let Some((_, exponent)) = text.split_once(['e', 'E'])
                        && exponent
                            .parse::<i64>()
                            .map_or(true, |exponent| exponent.unsigned_abs() > 1_073_741_823)
                    {
                        return Err(PgError::create(
                            SqlState::NumericValueOutOfRange,
                            "value overflows numeric",
                        ));
                    }
                    let value = BigDecimal::from_str(text).map_err(|_| {
                        PgError::create(SqlState::NumericValueOutOfRange, "value overflows numeric")
                    })?;
                    let (_, scale) = value.as_bigint_and_exponent();
                    let digits = i64::try_from(value.digits()).expect("numeric digits fit i64");
                    if scale > 16_383 || !value.is_zero() && digits.saturating_sub(scale) > 131_072
                    {
                        return Err(PgError::create(
                            SqlState::NumericValueOutOfRange,
                            "value overflows numeric",
                        ));
                    }
                    JsonbNode::Scalar(JsonbToken::Numeric(if value.is_zero() {
                        value.with_scale(scale.max(0))
                    } else {
                        value
                    }))
                }
            };
        }

        enum PendingToken {
            Node(usize),
            Token(JsonbToken),
        }
        let mut pending = vec![PendingToken::Node(0)];
        let mut tokens = Vec::new();
        while let Some(next) = pending.pop() {
            let index = match next {
                PendingToken::Token(token) => {
                    tokens.push(token);
                    continue;
                }
                PendingToken::Node(index) => index,
            };
            match std::mem::replace(&mut nodes[index], JsonbNode::Scalar(JsonbToken::Null)) {
                JsonbNode::Scalar(token) => tokens.push(token),
                JsonbNode::Array(children) => {
                    tokens.push(JsonbToken::Array(children.len()));
                    pending.push(PendingToken::Token(JsonbToken::EndArray));
                    pending.extend(children.into_iter().rev().map(PendingToken::Node));
                }
                JsonbNode::Object(children) => {
                    tokens.push(JsonbToken::Object(children.len()));
                    pending.push(PendingToken::Token(JsonbToken::EndObject));
                    for (key, child) in children.into_iter().rev() {
                        pending.push(PendingToken::Node(child));
                        pending.push(PendingToken::Token(JsonbToken::String(key)));
                    }
                }
            }
        }
        let mut value = Self {
            tokens,
            text: String::new(),
        };
        value.text = value.format_postgres_text();
        Ok(value)
    }

    pub fn get_postgres_text(&self) -> &str {
        &self.text
    }

    pub fn compare(&self, other: &Self) -> Ordering {
        let left = &self.tokens[0];
        let right = &other.tokens[0];
        // PostgreSQL's scalar pseudo-array makes an empty root array sort below scalars.
        if matches!(left, JsonbToken::Array(0))
            && !matches!(right, JsonbToken::Array(_) | JsonbToken::Object(_))
        {
            return Ordering::Less;
        }
        if matches!(right, JsonbToken::Array(0))
            && !matches!(left, JsonbToken::Array(_) | JsonbToken::Object(_))
        {
            return Ordering::Greater;
        }
        self.tokens.cmp(&other.tokens)
    }

    pub fn format_postgres_text(&self) -> String {
        let mut output = String::new();
        let mut containers = Vec::<(bool, usize)>::new();
        for token in &self.tokens {
            if matches!(token, JsonbToken::EndArray | JsonbToken::EndObject) {
                containers.pop().expect("JSONB end token has a container");
                output.push(if matches!(token, JsonbToken::EndArray) {
                    ']'
                } else {
                    '}'
                });
                continue;
            }
            if let Some((object, count)) = containers.last_mut() {
                if *count != 0 {
                    output.push_str(if *object && *count % 2 == 1 {
                        ": "
                    } else {
                        ", "
                    });
                }
                *count += 1;
            }
            match token {
                JsonbToken::Null => output.push_str("null"),
                JsonbToken::String(value) => {
                    output.push_str(&serde_json::to_string(value).expect("strings serialize"));
                }
                JsonbToken::Numeric(value) => output.push_str(&value.to_plain_string()),
                JsonbToken::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
                JsonbToken::Array(_) => {
                    output.push('[');
                    containers.push((false, 0));
                }
                JsonbToken::Object(_) => {
                    output.push('{');
                    containers.push((true, 0));
                }
                JsonbToken::EndArray | JsonbToken::EndObject => unreachable!(),
            }
        }
        output
    }
}

fn validate_jsonb_string(value: &str) -> Result<()> {
    if value.contains('\0') {
        Err(PgError::create(
            SqlState::UntranslatableCharacter,
            "unsupported Unicode escape sequence",
        ))
    } else {
        Ok(())
    }
}

fn create_jsonb_input_error(error: serde_json::Error) -> PgError {
    PgError::create(SqlState::InvalidTextRepresentation, error.to_string())
}
