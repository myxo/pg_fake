use super::*;

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn follows_postgres_numeric_cast_directions() {
    assert!(can_cast(
        BaseType::Int2,
        BaseType::Float8,
        CastContext::Implicit
    ));
    assert!(!can_cast(
        BaseType::Float8,
        BaseType::Int2,
        CastContext::Implicit
    ));
    assert!(can_cast(
        BaseType::Float8,
        BaseType::Int2,
        CastContext::Assignment
    ));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn rounds_numeric_assignments_and_checks_ranges() {
    assert_eq!(
        coerce(
            Value::Numeric("2.5".parse().unwrap()),
            BaseType::Numeric,
            PgType::create(BaseType::Int4),
            CastContext::Assignment,
        )
        .unwrap(),
        Value::Int4(3)
    );
    assert_eq!(
        coerce(
            Value::Int4(32768),
            BaseType::Int4,
            PgType::create(BaseType::Int2),
            CastContext::Assignment,
        )
        .unwrap_err()
        .sqlstate,
        SqlState::NumericValueOutOfRange
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn provides_assignment_conversion_for_every_numeric_pair() {
    let values = [
        (BaseType::Int2, Value::Int2(2)),
        (BaseType::Int4, Value::Int4(2)),
        (BaseType::Int8, Value::Int8(2)),
        (BaseType::Numeric, Value::Numeric("2".parse().unwrap())),
        (BaseType::Float4, Value::Float4(2.0)),
        (BaseType::Float8, Value::Float8(2.0)),
    ];
    let targets = [
        BaseType::Int2,
        BaseType::Int4,
        BaseType::Int8,
        BaseType::Numeric,
        BaseType::Float4,
        BaseType::Float8,
    ];

    for (source, value) in values {
        for target in targets {
            let converted = coerce(
                value.clone(),
                source,
                PgType::create(target),
                CastContext::Assignment,
            )
            .unwrap();
            assert_eq!(
                converted.get_base_type(),
                Some(target),
                "{source:?} -> {target:?}"
            );
        }
    }
}
