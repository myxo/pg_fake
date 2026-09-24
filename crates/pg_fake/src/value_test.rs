use super::*;

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn stores_pg_type_typmod() {
    let t = PgType::create(BaseType::Varchar);
    assert_eq!(t.typmod, PgType::NO_TYPEMOD);
    let t = PgType::create_with_typmod(BaseType::Varchar, 14);
    assert_ne!(t.typmod, PgType::NO_TYPEMOD);
    assert_eq!(t.map_to_oid(), 1043);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn roundtrips_integers() {
    for (base, text) in [
        (BaseType::Int2, "32767"),
        (BaseType::Int2, "-32768"),
        (BaseType::Int4, "2147483647"),
        (BaseType::Int4, "-2147483648"),
        (BaseType::Int8, "9000000000000000000"),
    ] {
        let v = Value::parse(base, text).unwrap();
        assert_eq!(v.format_postgres_text(), text);
        assert_eq!(v.get_base_type(), Some(base));
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_22003_for_integer_overflow() {
    let err = Value::parse(BaseType::Int2, "40000").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    let err = Value::parse(BaseType::Int4, "3000000000").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_22p02_for_invalid_integer_syntax() {
    let err = Value::parse(BaseType::Int4, "abc").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn accepts_postgres_boolean_forms() {
    for t in ["t", "tr", "tru", "TRUE", "y", "ye", "yes", "on", "1"] {
        assert_eq!(Value::parse(BaseType::Bool, t).unwrap(), Value::Bool(true));
    }
    for f in [
        "f", "fa", "fal", "fals", "FALSE", "n", "no", "of", "off", "0",
    ] {
        assert_eq!(Value::parse(BaseType::Bool, f).unwrap(), Value::Bool(false));
    }
    let err = Value::parse(BaseType::Bool, "maybe").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    let err = Value::parse(BaseType::Bool, "o").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn roundtrips_floats_and_special_values() {
    let v = Value::parse(BaseType::Float8, "1.5").unwrap();
    assert_eq!(v.format_postgres_text(), "1.5");
    let v = Value::parse(BaseType::Float8, "Infinity").unwrap();
    assert_eq!(v.format_postgres_text(), "Infinity");
    let v = Value::parse(BaseType::Float8, "-Infinity").unwrap();
    assert_eq!(v.format_postgres_text(), "-Infinity");
    let v = Value::parse(BaseType::Float8, "NaN").unwrap();
    assert_eq!(v.format_postgres_text(), "NaN");
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_22003_for_float_overflow() {
    let err = Value::parse(BaseType::Float4, "1e999").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    for (base, input) in [(BaseType::Float4, "10e-70"), (BaseType::Float8, "10e-400")] {
        let err = Value::parse(base, input).unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    }
    assert_eq!(
        Value::parse(BaseType::Float4, "0e-70").unwrap(),
        Value::Float4(0.0)
    );
}

#[test]
fn distinguishes_invalid_date_fields_from_invalid_text() {
    assert_eq!(
        Value::parse(BaseType::Date, "2040-04-10 BC")
            .unwrap()
            .format_postgres_text(),
        "2040-04-10 BC"
    );
    assert_eq!(
        Value::parse(BaseType::Date, "1997-02-29")
            .unwrap_err()
            .sqlstate,
        SqlState::DatetimeFieldOverflow
    );
    assert_eq!(
        Value::parse(BaseType::Date, "not-a-date")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parses_numeric_beyond_i64() {
    // 50-digit number: well outside i64/f64 exact range.
    let big = "12345678901234567890123456789012345678901234567890";
    let v = Value::parse(BaseType::Numeric, big).unwrap();
    assert_eq!(v.format_postgres_text(), big);
    // precision preserved through arithmetic identity
    match v {
        Value::Numeric(d) => assert_eq!(d.to_plain_string(), big),
        _ => panic!("expected numeric"),
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn preserves_numeric_scale() {
    let v = Value::parse(BaseType::Numeric, "1.10").unwrap();
    assert_eq!(v.format_postgres_text(), "1.10");
    let v = Value::parse(BaseType::Numeric, "0.001").unwrap();
    assert_eq!(v.format_postgres_text(), "0.001");
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_22p02_for_invalid_numeric() {
    let err = Value::parse(BaseType::Numeric, "1.2.3").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn roundtrips_text() {
    for base in [BaseType::Text, BaseType::Varchar, BaseType::Bpchar] {
        let v = Value::parse(base, "hello world").unwrap();
        assert_eq!(v, Value::Text("hello world".into()));
        assert_eq!(v.format_postgres_text(), "hello world");
    }
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn roundtrips_hex_bytea() {
    let v = Value::parse(BaseType::Bytea, "\\x414243").unwrap();
    assert_eq!(v, Value::Bytea(vec![0x41, 0x42, 0x43]));
    assert_eq!(v.format_postgres_text(), "\\x414243");
    // empty
    let v = Value::parse(BaseType::Bytea, "\\x").unwrap();
    assert_eq!(v, Value::Bytea(vec![]));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn roundtrips_escape_bytea() {
    let v = Value::parse(BaseType::Bytea, "ABC").unwrap();
    assert_eq!(v, Value::Bytea(b"ABC".to_vec()));
    // \\ -> single backslash
    let v = Value::parse(BaseType::Bytea, "\\\\").unwrap();
    assert_eq!(v, Value::Bytea(vec![b'\\']));
    // \101 (octal) -> 'A'
    let v = Value::parse(BaseType::Bytea, "\\101").unwrap();
    assert_eq!(v, Value::Bytea(vec![b'A']));
}

#[test]
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn reports_22p02_for_invalid_bytea() {
    let err = Value::parse(BaseType::Bytea, "\\xZZ").unwrap_err();
    assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
}
