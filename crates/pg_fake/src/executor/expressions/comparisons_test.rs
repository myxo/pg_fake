use super::compare_values;
use crate::value::Value;
use std::cmp::Ordering;

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn compares_all_phase_one_value_types() {
    let pairs = [
        (Value::Bool(false), Value::Bool(true)),
        (Value::Int2(1), Value::Int2(2)),
        (Value::Int4(1), Value::Int4(2)),
        (Value::Int8(1), Value::Int8(2)),
        (Value::Float4(1.0), Value::Float4(2.0)),
        (Value::Float8(1.0), Value::Float8(2.0)),
        (
            Value::Numeric("1".parse().unwrap()),
            Value::Numeric("2".parse().unwrap()),
        ),
        (Value::Text("a".into()), Value::Text("b".into())),
        (Value::Bytea(vec![1]), Value::Bytea(vec![2])),
    ];

    for (lower, higher) in pairs {
        assert_eq!(compare_values(&lower, &higher).unwrap(), Ordering::Less);
        assert_eq!(compare_values(&higher, &lower).unwrap(), Ordering::Greater);
    }

    assert_eq!(
        compare_values(&Value::Float4(f32::NAN), &Value::Float4(1.0)).unwrap(),
        Ordering::Greater
    );
    assert_eq!(
        compare_values(&Value::Float8(f64::NAN), &Value::Float8(1.0)).unwrap(),
        Ordering::Greater
    );
}
