use std::cmp::Ordering;

use crate::{error::Result, executor::expressions::compare_values, value::Value};

#[derive(Eq, Hash, PartialEq)]
pub(super) enum EqualityKey {
    Jsonb(crate::jsonb::Jsonb),
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(uuid::Uuid),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_equality_key(value: &Value) -> Option<EqualityKey> {
    match value {
        Value::Jsonb(value) => Some(EqualityKey::Jsonb(value.clone())),
        Value::Null => None,
        Value::Bool(value) => Some(EqualityKey::Bool(*value)),
        Value::Int2(value) => Some(EqualityKey::Int2(*value)),
        Value::Int4(value) => Some(EqualityKey::Int4(*value)),
        Value::Int8(value) => Some(EqualityKey::Int8(*value)),
        Value::Text(value) => Some(EqualityKey::Text(value.clone())),
        Value::Bytea(value) => Some(EqualityKey::Bytea(value.clone())),
        Value::Uuid(value) => Some(EqualityKey::Uuid(*value)),
        _ => None,
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn are_rows_not_distinct(left: &[Value], right: &[Value]) -> Result<bool> {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        match (left, right) {
            (Value::Null, Value::Null) => {}
            (Value::Null, _) | (_, Value::Null) => return Ok(false),
            _ if compare_values(left, right)? == Ordering::Equal => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}
