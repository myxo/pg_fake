use bigdecimal::BigDecimal;
use std::str::FromStr;

use crate::error::{PgError, Result, SqlState};

/// A PostgreSQL type OID (unsigned 32-bit, matching `pg_type.oid`).
pub type Oid = u32;

/// Phase-1 PostgreSQL base types (§3.1).
///
/// Each variant maps to a distinct `pg_type` OID. The character types
/// (`Text`, `Varchar`, `Bpchar`) all share the `Value::Text` backing; the
/// declared type (and its typmod) lives in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseType {
    Bool,
    Int2,
    Int4,
    Int8,
    Float4,
    Float8,
    Numeric,
    Text,
    Varchar,
    Bpchar,
    Bytea,
}

impl BaseType {
    /// The `pg_type` OID for this base type.
    pub fn oid(self) -> Oid {
        match self {
            BaseType::Bool => 16,
            BaseType::Bytea => 17,
            BaseType::Int8 => 20,
            BaseType::Int2 => 21,
            BaseType::Int4 => 23,
            BaseType::Text => 25,
            BaseType::Bpchar => 1042,
            BaseType::Varchar => 1043,
            BaseType::Float4 => 700,
            BaseType::Float8 => 701,
            BaseType::Numeric => 1700,
        }
    }

    /// The canonical (internal) PostgreSQL type name, e.g. `int4`, `bpchar`.
    pub fn name(self) -> &'static str {
        match self {
            BaseType::Bool => "bool",
            BaseType::Int2 => "int2",
            BaseType::Int4 => "int4",
            BaseType::Int8 => "int8",
            BaseType::Float4 => "float4",
            BaseType::Float8 => "float8",
            BaseType::Numeric => "numeric",
            BaseType::Text => "text",
            BaseType::Varchar => "varchar",
            BaseType::Bpchar => "bpchar",
            BaseType::Bytea => "bytea",
        }
    }

    /// Look up a base type by OID.
    pub fn from_oid(oid: Oid) -> Option<BaseType> {
        match oid {
            16 => Some(BaseType::Bool),
            17 => Some(BaseType::Bytea),
            20 => Some(BaseType::Int8),
            21 => Some(BaseType::Int2),
            23 => Some(BaseType::Int4),
            25 => Some(BaseType::Text),
            1042 => Some(BaseType::Bpchar),
            1043 => Some(BaseType::Varchar),
            700 => Some(BaseType::Float4),
            701 => Some(BaseType::Float8),
            1700 => Some(BaseType::Numeric),
            _ => None,
        }
    }

    /// Look up a base type by SQL type name, accepting common aliases.
    /// Matching is case-insensitive.
    pub fn from_name(name: &str) -> Option<BaseType> {
        match name.trim().to_ascii_lowercase().as_str() {
            "bool" | "boolean" => Some(BaseType::Bool),
            "int2" | "smallint" => Some(BaseType::Int2),
            "int4" | "integer" | "int" => Some(BaseType::Int4),
            "int8" | "bigint" => Some(BaseType::Int8),
            "float4" | "real" => Some(BaseType::Float4),
            "float8" | "double precision" | "double" => Some(BaseType::Float8),
            "numeric" | "decimal" => Some(BaseType::Numeric),
            "text" => Some(BaseType::Text),
            "varchar" | "character varying" => Some(BaseType::Varchar),
            "bpchar" | "character" => Some(BaseType::Bpchar),
            "bytea" => Some(BaseType::Bytea),
            _ => None,
        }
    }
}

/// A PostgreSQL type descriptor: base type plus a typmod slot (§3.1).
///
/// `typmod == -1` means "no typmod" (the PostgreSQL convention). Typmod
/// encoding is type-specific and interpreted by the catalog/coercion layers;
/// here it is just stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgType {
    pub base: BaseType,
    pub typmod: i32,
}

impl PgType {
    pub const NO_TYPEMOD: i32 = -1;

    pub fn new(base: BaseType) -> Self {
        PgType {
            base,
            typmod: Self::NO_TYPEMOD,
        }
    }

    pub fn with_typmod(base: BaseType, typmod: i32) -> Self {
        PgType { base, typmod }
    }

    pub fn oid(self) -> Oid {
        self.base.oid()
    }

    pub fn has_typmod(self) -> bool {
        self.typmod != Self::NO_TYPEMOD
    }
}

/// A single cell value for any Phase-1 PostgreSQL type (§3.1).
///
/// `Null` is a single variant, not per-type; three-valued logic is applied
/// consistently across operators. `Value` carries its base type but **not**
/// its typmod (the declared typmod lives in the catalog).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Float4(f32),
    Float8(f64),
    Numeric(BigDecimal),
    Text(String),
    Bytea(Vec<u8>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The base type of this value, or `None` for `Null`.
    ///
    /// For `Text` values the result is `BaseType::Text`; the catalog
    /// disambiguates `varchar`/`bpchar`.
    pub fn base_type(&self) -> Option<BaseType> {
        match self {
            Value::Null => None,
            Value::Bool(_) => Some(BaseType::Bool),
            Value::Int2(_) => Some(BaseType::Int2),
            Value::Int4(_) => Some(BaseType::Int4),
            Value::Int8(_) => Some(BaseType::Int8),
            Value::Float4(_) => Some(BaseType::Float4),
            Value::Float8(_) => Some(BaseType::Float8),
            Value::Numeric(_) => Some(BaseType::Numeric),
            Value::Text(_) => Some(BaseType::Text),
            Value::Bytea(_) => Some(BaseType::Bytea),
        }
    }

    /// Render to PostgreSQL text output form (the type's `typoutput` function).
    ///
    /// For `Null` returns an empty string; callers that need to distinguish
    /// NULL should check `is_null()` first.
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => {
                if *b {
                    "t".into()
                } else {
                    "f".into()
                }
            }
            Value::Int2(n) => n.to_string(),
            Value::Int4(n) => n.to_string(),
            Value::Int8(n) => n.to_string(),
            Value::Float4(f) => format_float(*f as f64),
            Value::Float8(f) => format_float(*f),
            Value::Numeric(d) => d.to_plain_string(),
            Value::Text(s) => s.clone(),
            Value::Bytea(bytes) => {
                let mut out = String::with_capacity(2 + bytes.len() * 2);
                out.push_str("\\x");
                for b in bytes {
                    out.push_str(&format!("{:02x}", b));
                }
                out
            }
        }
    }

    /// Parse a text input literal into a `Value` of the given base type
    /// (the type's `typinput` function). Invalid syntax yields `22P02`;
    /// out-of-range values yield `22003`.
    pub fn parse(base: BaseType, input: &str) -> Result<Value> {
        match base {
            BaseType::Bool => parse_bool(input).map(Value::Bool),
            BaseType::Int2 => parse_int::<i16>(input).map(Value::Int2),
            BaseType::Int4 => parse_int::<i32>(input).map(Value::Int4),
            BaseType::Int8 => parse_int::<i64>(input).map(Value::Int8),
            BaseType::Float4 => parse_float::<f32>(input).map(Value::Float4),
            BaseType::Float8 => parse_float::<f64>(input).map(Value::Float8),
            BaseType::Numeric => BigDecimal::from_str(input)
                .map(Value::Numeric)
                .map_err(|_| invalid_text(input, "numeric")),
            BaseType::Text | BaseType::Varchar | BaseType::Bpchar => {
                Ok(Value::Text(input.to_string()))
            }
            BaseType::Bytea => parse_bytea(input).map(Value::Bytea),
        }
    }
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    format!("{}", f)
}

fn parse_bool(input: &str) -> Result<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "y" | "yes" | "on" | "1" => Ok(true),
        "f" | "false" | "n" | "no" | "off" | "0" => Ok(false),
        _ => Err(invalid_text(input, "boolean")),
    }
}

fn parse_int<T: std::str::FromStr<Err = std::num::ParseIntError>>(input: &str) -> Result<T> {
    input.trim().parse::<T>().map_err(|e| match e.kind() {
        std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => PgError::new(
            SqlState::NumericValueOutOfRange,
            format!("value out of range for type: {input}"),
        ),
        _ => invalid_text(input, "integer"),
    })
}

fn parse_float<T: FloatExt>(input: &str) -> Result<T> {
    let s = input.trim();
    let lower = s.to_ascii_lowercase();
    // Rust's parser accepts "inf"/"infinity"/"nan"; PostgreSQL accepts the
    // same spellings (case-insensitive).
    let is_inf_literal = lower == "infinity"
        || lower == "-infinity"
        || lower == "+infinity"
        || lower == "inf"
        || lower == "-inf"
        || lower == "+inf";
    let v = T::from_str(s).map_err(|_| invalid_text(input, "floating point"))?;
    if v.is_infinite() && !is_inf_literal {
        return Err(PgError::new(
            SqlState::NumericValueOutOfRange,
            format!("value out of range for type: {input}"),
        ));
    }
    Ok(v)
}

trait FloatExt: Copy + std::str::FromStr<Err = std::num::ParseFloatError> {
    fn is_infinite(self) -> bool;
}
impl FloatExt for f32 {
    fn is_infinite(self) -> bool {
        f32::is_infinite(self)
    }
}
impl FloatExt for f64 {
    fn is_infinite(self) -> bool {
        f64::is_infinite(self)
    }
}

fn parse_bytea(input: &str) -> Result<Vec<u8>> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix("\\x") {
        hex_decode(hex).map_err(|_| invalid_text(input, "bytea"))
    } else {
        parse_bytea_escape(s).map_err(|_| invalid_text(input, "bytea"))
    }
}

fn hex_decode(hex: &str) -> std::result::Result<Vec<u8>, ()> {
    if !hex.len().is_multiple_of(2) {
        return Err(());
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars();
    while let (Some(h), Some(l)) = (chars.next(), chars.next()) {
        out.push(u8::from_str_radix(&format!("{h}{l}"), 16).map_err(|_| ())?);
    }
    Ok(out)
}

/// Legacy "escape" bytea format: `\\` -> `\`, `\<ooo>` (octal) -> byte, else literal.
fn parse_bytea_escape(s: &str) -> std::result::Result<Vec<u8>, ()> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 1 >= bytes.len() {
                return Err(());
            }
            if bytes[i + 1] == b'\\' {
                out.push(b'\\');
                i += 2;
            } else if bytes[i + 1].is_ascii_digit() {
                // up to 3 octal digits
                let end = (i + 4).min(bytes.len());
                let oct = std::str::from_utf8(&bytes[i + 1..end]).map_err(|_| ())?;
                let take = oct
                    .char_indices()
                    .take_while(|(idx, c)| *idx < 3 && c.is_ascii_digit())
                    .count();
                let val = u8::from_str_radix(&oct[..take], 8).map_err(|_| ())?;
                out.push(val);
                i += 1 + take;
            } else {
                return Err(());
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn invalid_text(input: &str, type_name: &str) -> PgError {
    PgError::new(
        SqlState::InvalidTextRepresentation,
        format!("invalid input syntax for type {type_name}: {input:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_oids_match_pg_catalog() {
        assert_eq!(BaseType::Bool.oid(), 16);
        assert_eq!(BaseType::Bytea.oid(), 17);
        assert_eq!(BaseType::Int8.oid(), 20);
        assert_eq!(BaseType::Int2.oid(), 21);
        assert_eq!(BaseType::Int4.oid(), 23);
        assert_eq!(BaseType::Text.oid(), 25);
        assert_eq!(BaseType::Bpchar.oid(), 1042);
        assert_eq!(BaseType::Varchar.oid(), 1043);
        assert_eq!(BaseType::Float4.oid(), 700);
        assert_eq!(BaseType::Float8.oid(), 701);
        assert_eq!(BaseType::Numeric.oid(), 1700);
    }

    #[test]
    fn name_lookup_canonical_and_aliases() {
        assert_eq!(BaseType::from_name("int4"), Some(BaseType::Int4));
        assert_eq!(BaseType::from_name("INTEGER"), Some(BaseType::Int4));
        assert_eq!(BaseType::from_name("smallint"), Some(BaseType::Int2));
        assert_eq!(BaseType::from_name("bigint"), Some(BaseType::Int8));
        assert_eq!(BaseType::from_name("real"), Some(BaseType::Float4));
        assert_eq!(
            BaseType::from_name("double precision"),
            Some(BaseType::Float8)
        );
        assert_eq!(BaseType::from_name("decimal"), Some(BaseType::Numeric));
        assert_eq!(
            BaseType::from_name("character varying"),
            Some(BaseType::Varchar)
        );
        assert_eq!(BaseType::from_name("character"), Some(BaseType::Bpchar));
        assert_eq!(BaseType::from_name("unknown_type"), None);
    }

    #[test]
    fn pg_type_typmod_slot() {
        let t = PgType::new(BaseType::Varchar);
        assert_eq!(t.typmod, PgType::NO_TYPEMOD);
        assert!(!t.has_typmod());
        let t = PgType::with_typmod(BaseType::Varchar, 14);
        assert!(t.has_typmod());
        assert_eq!(t.oid(), 1043);
    }

    #[test]
    fn integer_roundtrip() {
        for (base, text) in [
            (BaseType::Int2, "32767"),
            (BaseType::Int2, "-32768"),
            (BaseType::Int4, "2147483647"),
            (BaseType::Int4, "-2147483648"),
            (BaseType::Int8, "9000000000000000000"),
        ] {
            let v = Value::parse(base, text).unwrap();
            assert_eq!(v.to_text(), text);
            assert_eq!(v.base_type(), Some(base));
        }
    }

    #[test]
    fn integer_overflow_is_22003() {
        let err = Value::parse(BaseType::Int2, "40000").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
        let err = Value::parse(BaseType::Int4, "3000000000").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    }

    #[test]
    fn integer_invalid_syntax_is_22p02() {
        let err = Value::parse(BaseType::Int4, "abc").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn bool_parsing_accepts_postgres_forms() {
        for t in ["t", "TRUE", "y", "yes", "on", "1"] {
            assert_eq!(Value::parse(BaseType::Bool, t).unwrap(), Value::Bool(true));
        }
        for f in ["f", "FALSE", "n", "no", "off", "0"] {
            assert_eq!(Value::parse(BaseType::Bool, f).unwrap(), Value::Bool(false));
        }
        let err = Value::parse(BaseType::Bool, "maybe").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn bool_rendering() {
        assert_eq!(Value::Bool(true).to_text(), "t");
        assert_eq!(Value::Bool(false).to_text(), "f");
    }

    #[test]
    fn float_roundtrip_and_specials() {
        let v = Value::parse(BaseType::Float8, "1.5").unwrap();
        assert_eq!(v.to_text(), "1.5");
        let v = Value::parse(BaseType::Float8, "Infinity").unwrap();
        assert_eq!(v.to_text(), "Infinity");
        let v = Value::parse(BaseType::Float8, "-Infinity").unwrap();
        assert_eq!(v.to_text(), "-Infinity");
        let v = Value::parse(BaseType::Float8, "NaN").unwrap();
        assert_eq!(v.to_text(), "NaN");
    }

    #[test]
    fn float_overflow_is_22003() {
        let err = Value::parse(BaseType::Float4, "1e999").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    }

    #[test]
    fn numeric_beyond_i64() {
        // 50-digit number: well outside i64/f64 exact range.
        let big = "12345678901234567890123456789012345678901234567890";
        let v = Value::parse(BaseType::Numeric, big).unwrap();
        assert_eq!(v.to_text(), big);
        // precision preserved through arithmetic identity
        match v {
            Value::Numeric(d) => assert_eq!(d.to_plain_string(), big),
            _ => panic!("expected numeric"),
        }
    }

    #[test]
    fn numeric_preserves_scale() {
        let v = Value::parse(BaseType::Numeric, "1.10").unwrap();
        assert_eq!(v.to_text(), "1.10");
        let v = Value::parse(BaseType::Numeric, "0.001").unwrap();
        assert_eq!(v.to_text(), "0.001");
    }

    #[test]
    fn numeric_invalid_is_22p02() {
        let err = Value::parse(BaseType::Numeric, "1.2.3").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn text_roundtrip() {
        for base in [BaseType::Text, BaseType::Varchar, BaseType::Bpchar] {
            let v = Value::parse(base, "hello world").unwrap();
            assert_eq!(v, Value::Text("hello world".into()));
            assert_eq!(v.to_text(), "hello world");
        }
    }

    #[test]
    fn bytea_hex_roundtrip() {
        let v = Value::parse(BaseType::Bytea, "\\x414243").unwrap();
        assert_eq!(v, Value::Bytea(vec![0x41, 0x42, 0x43]));
        assert_eq!(v.to_text(), "\\x414243");
        // empty
        let v = Value::parse(BaseType::Bytea, "\\x").unwrap();
        assert_eq!(v, Value::Bytea(vec![]));
    }

    #[test]
    fn bytea_escape_roundtrip() {
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
    fn bytea_invalid_is_22p02() {
        let err = Value::parse(BaseType::Bytea, "\\xZZ").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }
}
