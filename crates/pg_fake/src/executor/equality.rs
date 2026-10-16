use std::{cmp::Ordering, collections::HashSet};

use crate::{error::Result, executor::expressions::compare_values, value::Value};

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum EqualityKey {
    Jsonb(crate::jsonb::Jsonb),
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Oid(u32),
    Int8(i64),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(uuid::Uuid),
    PgLsn(crate::value::PgLsn),
    Regclass(crate::value::PgRegclass),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_equality_key(value: &Value) -> Option<EqualityKey> {
    match value {
        Value::Jsonb(value) => Some(EqualityKey::Jsonb(value.clone())),
        Value::Null => None,
        Value::Bool(value) => Some(EqualityKey::Bool(*value)),
        Value::Int2(value) => Some(EqualityKey::Int2(*value)),
        Value::Int4(value) => Some(EqualityKey::Int4(*value)),
        Value::Oid(value) => Some(EqualityKey::Oid(*value)),
        Value::Int8(value) => Some(EqualityKey::Int8(*value)),
        Value::Text(value) => Some(EqualityKey::Text(value.clone())),
        Value::Bytea(value) => Some(EqualityKey::Bytea(value.clone())),
        Value::Uuid(value) => Some(EqualityKey::Uuid(*value)),
        Value::PgLsn(value) => Some(EqualityKey::PgLsn(*value)),
        Value::Regclass(value) => Some(EqualityKey::Regclass(*value)),
        _ => None,
    }
}

pub(super) fn create_row_equality_key(values: &[Value]) -> Option<Vec<Option<EqualityKey>>> {
    values
        .iter()
        .map(|value| {
            if value.is_null() {
                Some(None)
            } else {
                create_equality_key(value).map(Some)
            }
        })
        .collect()
}

pub(super) fn retain_first_unique_rows<T>(
    rows: Vec<T>,
    values: impl Fn(&T) -> &[Value],
) -> Result<Vec<T>> {
    let mut selected = Vec::new();
    let mut seen = None::<HashSet<Vec<Option<EqualityKey>>>>;
    let mut hashable = true;
    for row in rows {
        let value = values(&row);
        if let Some(seen) = &mut seen
            && let Some(key) = create_row_equality_key(value)
        {
            if seen.insert(key) {
                selected.push(row);
            }
            continue;
        }
        if seen.is_some() {
            seen = None;
            hashable = false;
        }
        let mut duplicate = false;
        for prior in &selected {
            if are_rows_not_distinct(values(prior), value)? {
                duplicate = true;
                break;
            }
        }
        if duplicate {
            continue;
        }
        selected.push(row);
        if hashable && selected.len() == 16 {
            let keys = selected
                .iter()
                .map(|prior| create_row_equality_key(values(prior)))
                .collect::<Option<HashSet<_>>>();
            if let Some(keys) = keys {
                seen = Some(keys);
            } else {
                hashable = false;
            }
        }
    }
    Ok(selected)
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

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        hash::{BuildHasherDefault, Hasher},
    };

    use super::{EqualityKey, create_row_equality_key};
    use crate::value::Value;

    #[derive(Default)]
    struct ConstantHasher;

    impl Hasher for ConstantHasher {
        fn finish(&self) -> u64 {
            0
        }

        fn write(&mut self, _: &[u8]) {}
    }

    #[test]
    fn preserves_distinct_keys_when_hashes_collide() {
        let mut seen =
            HashSet::<Vec<Option<EqualityKey>>, BuildHasherDefault<ConstantHasher>>::default();
        let first = create_row_equality_key(&[Value::Int4(1)]).unwrap();
        let second = create_row_equality_key(&[Value::Int4(2)]).unwrap();
        assert!(seen.insert(first.clone()));
        assert!(seen.insert(second));
        assert!(!seen.insert(first));
        assert_eq!(seen.len(), 2);

        let mut counts = HashMap::<
            Vec<Option<EqualityKey>>,
            usize,
            BuildHasherDefault<ConstantHasher>,
        >::default();
        let first = create_row_equality_key(&[Value::Int4(1)]).unwrap();
        let second = create_row_equality_key(&[Value::Int4(2)]).unwrap();
        *counts.entry(first.clone()).or_default() += 2;
        *counts.entry(second.clone()).or_default() += 1;
        assert_eq!(counts.get(&first), Some(&2));
        assert_eq!(counts.get(&second), Some(&1));
    }
}
