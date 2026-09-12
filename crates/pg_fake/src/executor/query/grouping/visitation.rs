use std::cmp::Ordering;

use super::CollectedGroup;
use crate::{executor::expressions::compare_values, value::Value};

fn compare_group_visitation(left: &[Value], right: &[Value]) -> Ordering {
    assert_eq!(left.len(), right.len());
    left.iter()
        .zip(right)
        .find_map(|(left, right)| {
            let ordering = match (left, right) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => Ordering::Greater,
                (_, Value::Null) => Ordering::Less,
                _ => compare_values(left, right).expect("GROUP BY expression type was checked"),
            };
            (ordering != Ordering::Equal).then_some(ordering)
        })
        .unwrap_or(Ordering::Equal)
}

fn hash_postgres_uint32(key: u32) -> u32 {
    let mut a = 0x9e37_79b9_u32.wrapping_add(4).wrapping_add(3_923_095);
    let mut b = a;
    let mut c = a;
    a = a.wrapping_add(key);
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(14));
    a ^= c;
    a = a.wrapping_sub(c.rotate_left(11));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(25));
    c ^= b;
    c = c.wrapping_sub(b.rotate_left(16));
    a ^= c;
    a = a.wrapping_sub(c.rotate_left(4));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(14));
    c ^= b;
    c.wrapping_sub(b.rotate_left(24))
}

fn hash_postgres_murmur32(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 13;
    value = value.wrapping_mul(0xc2b2_ae35);
    value ^ (value >> 16)
}

fn create_postgres_group_hash(key: &[Value]) -> Option<u32> {
    let [value] = key else {
        return None;
    };
    let hash = match value {
        Value::Null => 0,
        Value::Bool(value) => hash_postgres_uint32(u32::from(*value)),
        Value::Int2(value) => {
            hash_postgres_uint32(u32::from_ne_bytes(i32::from(*value).to_ne_bytes()))
        }
        Value::Int4(value) => hash_postgres_uint32(u32::from_ne_bytes(value.to_ne_bytes())),
        Value::Int8(value) => {
            let high = (*value >> 32) as u32;
            let low = *value as u32;
            hash_postgres_uint32(low ^ if *value >= 0 { high } else { !high })
        }
        _ => return None,
    };
    Some(hash_postgres_murmur32(hash))
}

pub(super) fn sort_groups_by_postgres_visitation(
    mut groups: Vec<CollectedGroup>,
) -> Vec<CollectedGroup> {
    if groups.len() > 230 {
        groups.sort_by(|left, right| compare_group_visitation(&right.key, &left.key));
        return groups;
    }
    let Some(hashes) = groups
        .iter()
        .map(|group| create_postgres_group_hash(&group.key))
        .collect::<Option<Vec<_>>>()
    else {
        groups.sort_by(|left, right| compare_group_visitation(&right.key, &left.key));
        return groups;
    };
    let mut buckets = vec![None; 256];
    let mask = buckets.len() - 1;
    for (index, hash) in hashes.iter().copied().enumerate() {
        let mut candidate = index;
        let mut position = hash as usize & mask;
        let mut distance = 0;
        loop {
            let Some(current) = buckets[position] else {
                buckets[position] = Some(candidate);
                break;
            };
            let current_optimal = hashes[current] as usize & mask;
            let current_distance = position.wrapping_sub(current_optimal) & mask;
            if distance > current_distance {
                buckets[position] = Some(candidate);
                candidate = current;
                distance = current_distance;
            }
            position = position.wrapping_add(1) & mask;
            distance += 1;
            assert!(distance < buckets.len());
        }
    }
    let start = buckets
        .iter()
        .position(Option::is_none)
        .expect("group hash table retains an empty bucket");
    let mut order = Vec::with_capacity(groups.len());
    let mut position = start;
    for _ in 0..buckets.len() {
        if let Some(index) = buckets[position] {
            order.push(index);
        }
        position = position.wrapping_sub(1) & mask;
    }
    let mut groups = groups.drain(..).map(Some).collect::<Vec<_>>();
    order
        .into_iter()
        .map(|index| groups[index].take().expect("group is visited once"))
        .collect()
}
