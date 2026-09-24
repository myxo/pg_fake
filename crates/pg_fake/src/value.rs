use bigdecimal::BigDecimal;
use chrono::{DateTime, Datelike, NaiveDateTime, Timelike, Utc};
use std::str::FromStr;

use crate::error::{PgError, Result, SqlState};

/// A PostgreSQL type OID (unsigned 32-bit, matching `pg_type.oid`).
pub type Oid = u32;

pub(crate) const DAYS_PER_MONTH: i32 = 30;
pub(crate) const MICROSECONDS_PER_DAY: i64 = 86_400_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PgDate {
    NegInfinity,
    Finite(chrono::NaiveDate),
    Infinity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PgTime(pub i64);

/// PostgreSQL `timestamp without time zone`, including its two sentinel values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PgTimestamp {
    NegInfinity,
    Finite(NaiveDateTime),
    Infinity,
}

/// PostgreSQL `timestamp with time zone`: a UTC instant plus PostgreSQL's
/// infinity sentinels. Rendering in a session zone is deliberately kept above
/// the value layer so persisted values never acquire a display-zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PgTimestampTz {
    NegInfinity,
    Finite(DateTime<Utc>),
    Infinity,
}

/// PostgreSQL intervals retain calendar months, days, and clock microseconds
/// independently; collapsing them to a duration loses month-end semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PgInterval {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PgLsn(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PgRegclass(pub Oid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArrayElementType {
    Bool,
    Int2,
    Int4,
    Int8,
    Oid,
    Float4,
    Float8,
    Numeric,
    Text,
    Varchar,
    Bpchar,
    Bytea,
    Uuid,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Json,
    Jsonb,
    PgLsn,
    Regclass,
}

impl ArrayElementType {
    pub fn get_base_type(self) -> BaseType {
        match self {
            Self::Bool => BaseType::Bool,
            Self::Int2 => BaseType::Int2,
            Self::Int4 => BaseType::Int4,
            Self::Int8 => BaseType::Int8,
            Self::Oid => BaseType::Oid,
            Self::Float4 => BaseType::Float4,
            Self::Float8 => BaseType::Float8,
            Self::Numeric => BaseType::Numeric,
            Self::Text => BaseType::Text,
            Self::Varchar => BaseType::Varchar,
            Self::Bpchar => BaseType::Bpchar,
            Self::Bytea => BaseType::Bytea,
            Self::Uuid => BaseType::Uuid,
            Self::Date => BaseType::Date,
            Self::Time => BaseType::Time,
            Self::Timestamp => BaseType::Timestamp,
            Self::TimestampTz => BaseType::TimestampTz,
            Self::Interval => BaseType::Interval,
            Self::Json => BaseType::Json,
            Self::Jsonb => BaseType::Jsonb,
            Self::PgLsn => BaseType::PgLsn,
            Self::Regclass => BaseType::Regclass,
        }
    }
}

/// Phase-1 PostgreSQL base types (§3.1).
///
/// Each variant maps to a distinct `pg_type` OID. The character types
/// (`Text`, `Varchar`, `Bpchar`) all share the `Value::Text` backing; the
/// declared type (and its typmod) lives in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseType {
    Void,
    Bool,
    Int2,
    Int4,
    Int8,
    Oid,
    Float4,
    Float8,
    Numeric,
    Text,
    Varchar,
    Bpchar,
    Bytea,
    Uuid,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    Json,
    Jsonb,
    PgLsn,
    Regclass,
    Array(ArrayElementType),
}

impl BaseType {
    /// The `pg_type` OID for this base type.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn map_to_oid(self) -> Oid {
        match self {
            BaseType::Void => 2278,
            BaseType::Bool => 16,
            BaseType::Bytea => 17,
            BaseType::Int8 => 20,
            BaseType::Int2 => 21,
            BaseType::Int4 => 23,
            BaseType::Oid => 26,
            BaseType::Text => 25,
            BaseType::Bpchar => 1042,
            BaseType::Varchar => 1043,
            BaseType::Float4 => 700,
            BaseType::Float8 => 701,
            BaseType::Numeric => 1700,
            BaseType::Uuid => 2950,
            BaseType::Date => 1082,
            BaseType::Time => 1083,
            BaseType::Timestamp => 1114,
            BaseType::TimestampTz => 1184,
            BaseType::Interval => 1186,
            BaseType::Json => 114,
            BaseType::Jsonb => 3802,
            BaseType::PgLsn => 3220,
            BaseType::Regclass => 2205,
            BaseType::Array(element) => match element {
                ArrayElementType::Bool => 1000,
                ArrayElementType::Bytea => 1001,
                ArrayElementType::Int2 => 1005,
                ArrayElementType::Int4 => 1007,
                ArrayElementType::Text => 1009,
                ArrayElementType::Bpchar => 1014,
                ArrayElementType::Varchar => 1015,
                ArrayElementType::Int8 => 1016,
                ArrayElementType::Float4 => 1021,
                ArrayElementType::Float8 => 1022,
                ArrayElementType::Oid => 1028,
                ArrayElementType::Timestamp => 1115,
                ArrayElementType::Date => 1182,
                ArrayElementType::Time => 1183,
                ArrayElementType::TimestampTz => 1185,
                ArrayElementType::Interval => 1187,
                ArrayElementType::Numeric => 1231,
                ArrayElementType::Json => 199,
                ArrayElementType::Regclass => 2210,
                ArrayElementType::Uuid => 2951,
                ArrayElementType::PgLsn => 3221,
                ArrayElementType::Jsonb => 3807,
            },
        }
    }

    /// The canonical (internal) PostgreSQL type name, e.g. `int4`, `bpchar`.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_postgres_name(self) -> &'static str {
        match self {
            BaseType::Void => "void",
            BaseType::Bool => "bool",
            BaseType::Int2 => "int2",
            BaseType::Int4 => "int4",
            BaseType::Oid => "oid",
            BaseType::Int8 => "int8",
            BaseType::Float4 => "float4",
            BaseType::Float8 => "float8",
            BaseType::Numeric => "numeric",
            BaseType::Text => "text",
            BaseType::Varchar => "varchar",
            BaseType::Bpchar => "bpchar",
            BaseType::Bytea => "bytea",
            BaseType::Uuid => "uuid",
            BaseType::Date => "date",
            BaseType::Time => "time",
            BaseType::Timestamp => "timestamp",
            BaseType::TimestampTz => "timestamptz",
            BaseType::Interval => "interval",
            BaseType::Json => "json",
            BaseType::Jsonb => "jsonb",
            BaseType::PgLsn => "pg_lsn",
            BaseType::Regclass => "regclass",
            BaseType::Array(element) => match element {
                ArrayElementType::Bool => "_bool",
                ArrayElementType::Int2 => "_int2",
                ArrayElementType::Int4 => "_int4",
                ArrayElementType::Int8 => "_int8",
                ArrayElementType::Oid => "_oid",
                ArrayElementType::Float4 => "_float4",
                ArrayElementType::Float8 => "_float8",
                ArrayElementType::Numeric => "_numeric",
                ArrayElementType::Text => "_text",
                ArrayElementType::Varchar => "_varchar",
                ArrayElementType::Bpchar => "_bpchar",
                ArrayElementType::Bytea => "_bytea",
                ArrayElementType::Uuid => "_uuid",
                ArrayElementType::Date => "_date",
                ArrayElementType::Time => "_time",
                ArrayElementType::Timestamp => "_timestamp",
                ArrayElementType::TimestampTz => "_timestamptz",
                ArrayElementType::Interval => "_interval",
                ArrayElementType::Json => "_json",
                ArrayElementType::Jsonb => "_jsonb",
                ArrayElementType::PgLsn => "_pg_lsn",
                ArrayElementType::Regclass => "_regclass",
            },
        }
    }

    /// Look up a base type by OID.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn resolve_oid(oid: Oid) -> Option<BaseType> {
        match oid {
            2278 => Some(BaseType::Void),
            16 => Some(BaseType::Bool),
            17 => Some(BaseType::Bytea),
            20 => Some(BaseType::Int8),
            21 => Some(BaseType::Int2),
            23 => Some(BaseType::Int4),
            26 => Some(BaseType::Oid),
            25 => Some(BaseType::Text),
            1042 => Some(BaseType::Bpchar),
            1043 => Some(BaseType::Varchar),
            700 => Some(BaseType::Float4),
            701 => Some(BaseType::Float8),
            1700 => Some(BaseType::Numeric),
            2950 => Some(BaseType::Uuid),
            1082 => Some(BaseType::Date),
            1083 => Some(BaseType::Time),
            1114 => Some(BaseType::Timestamp),
            1184 => Some(BaseType::TimestampTz),
            1186 => Some(BaseType::Interval),
            114 => Some(BaseType::Json),
            3802 => Some(BaseType::Jsonb),
            3220 => Some(BaseType::PgLsn),
            2205 => Some(BaseType::Regclass),
            1000 => Some(BaseType::Bool.get_array_type().unwrap()),
            1001 => Some(BaseType::Bytea.get_array_type().unwrap()),
            1005 => Some(BaseType::Int2.get_array_type().unwrap()),
            1007 => Some(BaseType::Int4.get_array_type().unwrap()),
            1009 => Some(BaseType::Text.get_array_type().unwrap()),
            1014 => Some(BaseType::Bpchar.get_array_type().unwrap()),
            1015 => Some(BaseType::Varchar.get_array_type().unwrap()),
            1016 => Some(BaseType::Int8.get_array_type().unwrap()),
            1021 => Some(BaseType::Float4.get_array_type().unwrap()),
            1022 => Some(BaseType::Float8.get_array_type().unwrap()),
            1028 => Some(BaseType::Oid.get_array_type().unwrap()),
            1115 => Some(BaseType::Timestamp.get_array_type().unwrap()),
            1182 => Some(BaseType::Date.get_array_type().unwrap()),
            1183 => Some(BaseType::Time.get_array_type().unwrap()),
            1185 => Some(BaseType::TimestampTz.get_array_type().unwrap()),
            1187 => Some(BaseType::Interval.get_array_type().unwrap()),
            1231 => Some(BaseType::Numeric.get_array_type().unwrap()),
            199 => Some(BaseType::Json.get_array_type().unwrap()),
            2210 => Some(BaseType::Regclass.get_array_type().unwrap()),
            2951 => Some(BaseType::Uuid.get_array_type().unwrap()),
            3221 => Some(BaseType::PgLsn.get_array_type().unwrap()),
            3807 => Some(BaseType::Jsonb.get_array_type().unwrap()),
            _ => None,
        }
    }

    /// Look up a base type by SQL type name, accepting common aliases.
    /// Matching is case-insensitive.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn parse_sql_name(name: &str) -> Option<BaseType> {
        match name.trim().to_ascii_lowercase().as_str() {
            "void" => Some(BaseType::Void),
            "bool" | "boolean" => Some(BaseType::Bool),
            "int2" | "smallint" => Some(BaseType::Int2),
            "int4" | "integer" | "int" => Some(BaseType::Int4),
            "oid" => Some(BaseType::Oid),
            "int8" | "bigint" => Some(BaseType::Int8),
            "float4" | "real" => Some(BaseType::Float4),
            "float8" | "double precision" | "double" => Some(BaseType::Float8),
            "numeric" | "decimal" => Some(BaseType::Numeric),
            "text" => Some(BaseType::Text),
            "varchar" | "character varying" => Some(BaseType::Varchar),
            "bpchar" | "character" => Some(BaseType::Bpchar),
            "bytea" => Some(BaseType::Bytea),
            "uuid" => Some(BaseType::Uuid),
            "date" => Some(BaseType::Date),
            "time" | "time without time zone" => Some(BaseType::Time),
            "timestamp" | "timestamp without time zone" => Some(BaseType::Timestamp),
            "timestamptz" | "timestamp with time zone" => Some(BaseType::TimestampTz),
            "interval" => Some(BaseType::Interval),
            "json" => Some(BaseType::Json),
            "jsonb" => Some(BaseType::Jsonb),
            "pg_lsn" => Some(BaseType::PgLsn),
            "regclass" => Some(BaseType::Regclass),
            name if let Some(element) = name.strip_suffix("[]") => {
                BaseType::parse_sql_name(element).and_then(BaseType::get_array_type)
            }
            name if name.starts_with('_') => {
                BaseType::parse_sql_name(&name[1..]).and_then(BaseType::get_array_type)
            }
            _ => None,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_array_element_type(self) -> Option<BaseType> {
        match self {
            BaseType::Array(element) => Some(element.get_base_type()),
            _ => None,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_array_type(self) -> Option<BaseType> {
        match self {
            BaseType::Bool => Some(BaseType::Array(ArrayElementType::Bool)),
            BaseType::Int2 => Some(BaseType::Array(ArrayElementType::Int2)),
            BaseType::Int4 => Some(BaseType::Array(ArrayElementType::Int4)),
            BaseType::Int8 => Some(BaseType::Array(ArrayElementType::Int8)),
            BaseType::Oid => Some(BaseType::Array(ArrayElementType::Oid)),
            BaseType::Float4 => Some(BaseType::Array(ArrayElementType::Float4)),
            BaseType::Float8 => Some(BaseType::Array(ArrayElementType::Float8)),
            BaseType::Numeric => Some(BaseType::Array(ArrayElementType::Numeric)),
            BaseType::Text => Some(BaseType::Array(ArrayElementType::Text)),
            BaseType::Varchar => Some(BaseType::Array(ArrayElementType::Varchar)),
            BaseType::Bpchar => Some(BaseType::Array(ArrayElementType::Bpchar)),
            BaseType::Bytea => Some(BaseType::Array(ArrayElementType::Bytea)),
            BaseType::Uuid => Some(BaseType::Array(ArrayElementType::Uuid)),
            BaseType::Date => Some(BaseType::Array(ArrayElementType::Date)),
            BaseType::Time => Some(BaseType::Array(ArrayElementType::Time)),
            BaseType::Timestamp => Some(BaseType::Array(ArrayElementType::Timestamp)),
            BaseType::TimestampTz => Some(BaseType::Array(ArrayElementType::TimestampTz)),
            BaseType::Interval => Some(BaseType::Array(ArrayElementType::Interval)),
            BaseType::Json => Some(BaseType::Array(ArrayElementType::Json)),
            BaseType::Jsonb => Some(BaseType::Array(ArrayElementType::Jsonb)),
            BaseType::PgLsn => Some(BaseType::Array(ArrayElementType::PgLsn)),
            BaseType::Regclass => Some(BaseType::Array(ArrayElementType::Regclass)),
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
pub(crate) struct PgType {
    pub(crate) base: BaseType,
    pub(crate) typmod: i32,
}

impl PgType {
    pub(crate) const NO_TYPEMOD: i32 = -1;

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create(base: BaseType) -> Self {
        PgType {
            base,
            typmod: Self::NO_TYPEMOD,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_with_typmod(base: BaseType, typmod: i32) -> Self {
        PgType { base, typmod }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn map_to_oid(self) -> Oid {
        self.base.map_to_oid()
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
    Void,
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Oid(Oid),
    Int8(i64),
    Float4(f32),
    Float8(f64),
    Numeric(BigDecimal),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(uuid::Uuid),
    Date(PgDate),
    Time(PgTime),
    Timestamp(PgTimestamp),
    TimestampTz(PgTimestampTz),
    Interval(PgInterval),
    Json(String),
    Jsonb(crate::jsonb::Jsonb),
    PgLsn(PgLsn),
    Regclass(PgRegclass),
    Array {
        elem_type: BaseType,
        values: Vec<Value>,
    },
}

impl Value {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The base type of this value, or `None` for `Null`.
    ///
    /// For `Text` values the result is `BaseType::Text`; the catalog
    /// disambiguates `varchar`/`bpchar`.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_base_type(&self) -> Option<BaseType> {
        match self {
            Value::Null => None,
            Value::Void => Some(BaseType::Void),
            Value::Bool(_) => Some(BaseType::Bool),
            Value::Int2(_) => Some(BaseType::Int2),
            Value::Int4(_) => Some(BaseType::Int4),
            Value::Oid(_) => Some(BaseType::Oid),
            Value::Int8(_) => Some(BaseType::Int8),
            Value::Float4(_) => Some(BaseType::Float4),
            Value::Float8(_) => Some(BaseType::Float8),
            Value::Numeric(_) => Some(BaseType::Numeric),
            Value::Text(_) => Some(BaseType::Text),
            Value::Bytea(_) => Some(BaseType::Bytea),
            Value::Uuid(_) => Some(BaseType::Uuid),
            Value::Date(_) => Some(BaseType::Date),
            Value::Time(_) => Some(BaseType::Time),
            Value::Timestamp(_) => Some(BaseType::Timestamp),
            Value::TimestampTz(_) => Some(BaseType::TimestampTz),
            Value::Interval(_) => Some(BaseType::Interval),
            Value::Json(_) => Some(BaseType::Json),
            Value::Jsonb(_) => Some(BaseType::Jsonb),
            Value::PgLsn(_) => Some(BaseType::PgLsn),
            Value::Regclass(_) => Some(BaseType::Regclass),
            Value::Array { elem_type, .. } => Some(
                elem_type
                    .get_array_type()
                    .expect("arrays have supported element types"),
            ),
        }
    }

    /// Render to PostgreSQL text output form (the type's `typoutput` function).
    ///
    /// For `Null` returns an empty string; callers that need to distinguish
    /// NULL should check `is_null()` first.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn format_postgres_text(&self) -> String {
        match self {
            Value::Null | Value::Void => String::new(),
            Value::Bool(b) => {
                if *b {
                    "t".into()
                } else {
                    "f".into()
                }
            }
            Value::Int2(n) => n.to_string(),
            Value::Int4(n) => n.to_string(),
            Value::Oid(n) => n.to_string(),
            Value::Int8(n) => n.to_string(),
            Value::Float4(f) if f.is_nan() => "NaN".into(),
            Value::Float4(f) if f.is_infinite() => {
                if *f > 0.0 {
                    "Infinity".into()
                } else {
                    "-Infinity".into()
                }
            }
            Value::Float4(f) => f.to_string(),
            Value::Float8(f) => format_float8(*f),
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
            Value::Uuid(value) => value.to_string(),
            Value::Date(PgDate::NegInfinity) => "-infinity".into(),
            Value::Date(PgDate::Infinity) => "infinity".into(),
            Value::Date(PgDate::Finite(value)) => format_pg_date(*value),
            Value::Time(PgTime(value)) if *value == MICROSECONDS_PER_DAY => "24:00:00".into(),
            Value::Time(PgTime(value)) => {
                let hours = value / 3_600_000_000;
                let minutes = value / 60_000_000 % 60;
                let seconds = value / 1_000_000 % 60;
                let micros = value % 1_000_000;
                if micros == 0 {
                    format!("{hours:02}:{minutes:02}:{seconds:02}")
                } else {
                    format!("{hours:02}:{minutes:02}:{seconds:02}.{micros:06}")
                        .trim_end_matches('0')
                        .into()
                }
            }
            Value::Timestamp(PgTimestamp::NegInfinity)
            | Value::TimestampTz(PgTimestampTz::NegInfinity) => "-infinity".into(),
            Value::Timestamp(PgTimestamp::Infinity)
            | Value::TimestampTz(PgTimestampTz::Infinity) => "infinity".into(),
            Value::Timestamp(PgTimestamp::Finite(value)) => format_timestamp(*value),
            Value::TimestampTz(PgTimestampTz::Finite(value)) => {
                format!("{}+00", format_timestamp(value.naive_utc()))
            }
            Value::Interval(value) => format_interval(*value),
            Value::Json(value) => value.clone(),
            Value::Jsonb(value) => value.get_postgres_text().to_owned(),
            Value::PgLsn(PgLsn(value)) => {
                format!("{:X}/{:X}", value >> 32, value & 0xffff_ffff)
            }
            Value::Regclass(PgRegclass(oid)) => oid.to_string(),
            Value::Array { values, .. } => crate::text_array::format_array(values),
        }
    }

    /// Parse a text input literal into a `Value` of the given base type
    /// (the type's `typinput` function).
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn parse(base: BaseType, input: &str) -> Result<Value> {
        match base {
            BaseType::Void => Ok(Value::Void),
            BaseType::Bool => parse_bool(input).map(Value::Bool),
            BaseType::Int2 => parse_int::<i16>(input).map(Value::Int2),
            BaseType::Int4 => parse_int::<i32>(input).map(Value::Int4),
            BaseType::Oid => parse_int::<u32>(input).map(Value::Oid),
            BaseType::Int8 => parse_int::<i64>(input).map(Value::Int8),
            BaseType::Float4 => parse_float::<f32>(input).map(Value::Float4),
            BaseType::Float8 => parse_float::<f64>(input).map(Value::Float8),
            BaseType::Numeric => BigDecimal::from_str(input)
                .map(Value::Numeric)
                .map_err(|_| create_invalid_text_error(input, "numeric")),
            BaseType::Text | BaseType::Varchar | BaseType::Bpchar => {
                Ok(Value::Text(input.to_string()))
            }
            BaseType::Bytea => parse_bytea(input).map(Value::Bytea),
            BaseType::Uuid => uuid::Uuid::parse_str(input)
                .map(Value::Uuid)
                .map_err(|_| create_invalid_text_error(input, "uuid")),
            BaseType::Date => parse_date(input).map(Value::Date),
            BaseType::Time => parse_time(input).map(Value::Time),
            BaseType::Timestamp => parse_timestamp(input).map(Value::Timestamp),
            BaseType::TimestampTz => parse_timestamptz(input).map(Value::TimestampTz),
            BaseType::Interval => parse_interval(input).map(Value::Interval),
            BaseType::Json => validate_json(input)
                .map(|()| Value::Json(input.to_owned()))
                .map_err(|()| create_invalid_text_error(input, "json")),
            BaseType::Jsonb => crate::jsonb::Jsonb::parse(input).map(Value::Jsonb),
            BaseType::PgLsn => parse_pg_lsn(input).map(Value::PgLsn),
            BaseType::Regclass => {
                parse_int::<u32>(input).map(|oid| Value::Regclass(PgRegclass(oid)))
            }
            BaseType::Array(_) => {
                let elem_type = base
                    .get_array_element_type()
                    .expect("array base type has an element type");
                crate::text_array::parse_array(input, elem_type)
                    .map(|values| Value::Array { elem_type, values })
            }
        }
    }
}

fn parse_pg_lsn(input: &str) -> Result<PgLsn> {
    let Some((high, low)) = input.split_once('/') else {
        return Err(create_invalid_text_error(input, "pg_lsn"));
    };
    if high.is_empty()
        || low.is_empty()
        || high.len() > 8
        || low.len() > 8
        || !high.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !low.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(create_invalid_text_error(input, "pg_lsn"));
    }
    let high =
        u32::from_str_radix(high, 16).map_err(|_| create_invalid_text_error(input, "pg_lsn"))?;
    let low =
        u32::from_str_radix(low, 16).map_err(|_| create_invalid_text_error(input, "pg_lsn"))?;
    Ok(PgLsn((u64::from(high) << 32) | u64::from(low)))
}

fn validate_json(input: &str) -> std::result::Result<(), ()> {
    let bytes = input.as_bytes();
    let mut validated = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && bytes.get(index + 1) == Some(&b'u')
            && let Some(hex) = bytes.get(index + 2..index + 6)
            && hex.iter().all(u8::is_ascii_hexdigit)
        {
            let code = u16::from_str_radix(
                std::str::from_utf8(hex).expect("JSON escape digits are ASCII"),
                16,
            )
            .expect("JSON escape digits form a u16");
            if (0xd800..=0xdfff).contains(&code) {
                validated.extend_from_slice(b"\\u0041");
                index += 6;
                continue;
            }
        }
        validated.push(bytes[index]);
        index += 1;
    }
    let validated = std::str::from_utf8(&validated).expect("JSON validation preserves UTF-8");
    let mut deserializer = serde_json::Deserializer::from_str(validated);
    deserializer.disable_recursion_limit();
    serde::Deserialize::deserialize(&mut deserializer)
        .and_then(|_: serde::de::IgnoredAny| deserializer.end())
        .map_err(|_| ())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn format_interval(value: PgInterval) -> String {
    let mut fields = Vec::new();
    if value.months != 0 {
        let years = value.months / 12;
        let months = value.months % 12;
        if years != 0 {
            fields.push(format!(
                "{years} year{}",
                if years.abs() == 1 { "" } else { "s" }
            ));
        }
        if months != 0 {
            fields.push(format!(
                "{months} mon{}",
                if months.abs() == 1 { "" } else { "s" }
            ));
        }
    }
    if value.days != 0 {
        fields.push(format!(
            "{} day{}",
            value.days,
            if value.days.abs() == 1 { "" } else { "s" }
        ));
    }
    if value.micros != 0 || fields.is_empty() {
        let sign = if value.micros < 0 { "-" } else { "" };
        let micros = value.micros.unsigned_abs();
        let hours = micros / 3_600_000_000;
        let minutes = micros / 60_000_000 % 60;
        let seconds = micros / 1_000_000 % 60;
        let fraction = micros % 1_000_000;
        let time = if fraction == 0 {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        } else {
            format!("{hours:02}:{minutes:02}:{seconds:02}.{fraction:06}")
                .trim_end_matches('0')
                .to_string()
        };
        fields.push(format!("{sign}{time}"));
    }
    fields.join(" ")
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_interval(input: &str) -> Result<PgInterval> {
    let input = input.trim();
    if input.is_empty() {
        return Err(create_invalid_text_error(input, "interval"));
    }
    let mut value = PgInterval {
        months: 0,
        days: 0,
        micros: 0,
    };
    let parts: Vec<_> = input.split_whitespace().collect();
    let mut index = 0;
    while index < parts.len() {
        if parts[index].contains(':') {
            let sign = if parts[index].starts_with('-') {
                -1_i64
            } else {
                1
            };
            let time = parts[index].trim_start_matches(['+', '-']);
            let fields: Vec<_> = time.split(':').collect();
            if fields.len() != 3 {
                return Err(create_invalid_text_error(input, "interval"));
            }
            let hour = fields[0]
                .parse::<i64>()
                .map_err(|_| create_invalid_text_error(input, "interval"))?;
            let minute = fields[1]
                .parse::<i64>()
                .map_err(|_| create_invalid_text_error(input, "interval"))?;
            let second = fields[2]
                .parse::<f64>()
                .map_err(|_| create_invalid_text_error(input, "interval"))?;
            let second_micros = second * 1_000_000.0;
            if !second_micros.is_finite()
                || second_micros < i64::MIN as f64
                || second_micros > i64::MAX as f64
            {
                return Err(PgError::create(
                    SqlState::IntervalFieldOverflow,
                    "interval out of range",
                ));
            }
            let micros = hour
                .checked_mul(3_600_000_000)
                .and_then(|value| {
                    minute
                        .checked_mul(60_000_000)
                        .and_then(|minutes| value.checked_add(minutes))
                })
                .and_then(|value| value.checked_add(second_micros.round() as i64))
                .and_then(|value| value.checked_mul(sign))
                .ok_or_else(|| {
                    PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                })?;
            value.micros = value.micros.checked_add(micros).ok_or_else(|| {
                PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
            })?;
            index += 1;
            continue;
        }
        if index + 1 >= parts.len() {
            return Err(create_invalid_text_error(input, "interval"));
        }
        let number = parts[index]
            .parse::<i64>()
            .map_err(|_| create_invalid_text_error(input, "interval"))?;
        match parts[index + 1].to_ascii_lowercase().as_str() {
            "year" | "years" => {
                value.months = value
                    .months
                    .checked_add(
                        i32::try_from(number.checked_mul(12).ok_or_else(|| {
                            PgError::create(
                                SqlState::IntervalFieldOverflow,
                                "interval out of range",
                            )
                        })?)
                        .map_err(|_| {
                            PgError::create(
                                SqlState::IntervalFieldOverflow,
                                "interval out of range",
                            )
                        })?,
                    )
                    .ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?
            }
            "mon" | "mons" | "month" | "months" => {
                value.months = value
                    .months
                    .checked_add(i32::try_from(number).map_err(|_| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?
            }
            "day" | "days" => {
                value.days = value
                    .days
                    .checked_add(i32::try_from(number).map_err(|_| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?
            }
            "hour" | "hours" => {
                value.micros = value
                    .micros
                    .checked_add(number.checked_mul(3_600_000_000).ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?
            }
            "minute" | "minutes" | "min" | "mins" => {
                value.micros = value
                    .micros
                    .checked_add(number.checked_mul(60_000_000).ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?
            }
            "second" | "seconds" | "sec" | "secs" => {
                value.micros = value
                    .micros
                    .checked_add(number.checked_mul(1_000_000).ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::IntervalFieldOverflow, "interval out of range")
                    })?
            }
            _ => return Err(create_invalid_text_error(input, "interval")),
        }
        index += 2;
    }
    Ok(value)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn format_timestamp(value: NaiveDateTime) -> String {
    let time = value.format("%H:%M:%S%.6f").to_string();
    let time = time.trim_end_matches('0').trim_end_matches('.').to_string();
    let date = value.date();
    if date.year() <= 0 {
        format!(
            "{:04}-{:02}-{:02} {time} BC",
            1 - date.year(),
            date.month(),
            date.day()
        )
    } else {
        format!("{} {time}", format_pg_date(date))
    }
}

fn format_pg_date(value: chrono::NaiveDate) -> String {
    if value.year() <= 0 {
        format!(
            "{:04}-{:02}-{:02} BC",
            1 - value.year(),
            value.month(),
            value.day()
        )
    } else {
        value.format("%Y-%m-%d").to_string()
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn format_float8(f: f64) -> String {
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_date(input: &str) -> Result<PgDate> {
    let trimmed = input.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "infinity" => Ok(PgDate::Infinity),
        "-infinity" => Ok(PgDate::NegInfinity),
        _ if trimmed.to_ascii_lowercase().ends_with(" bc") => {
            let date_text = &trimmed[..trimmed.len() - 3];
            let mut parts = date_text.split('-');
            let (Some(year), Some(month), Some(day), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return Err(create_invalid_text_error(input, "date"));
            };
            let (Ok(year), Ok(month), Ok(day)) = (
                year.parse::<i32>(),
                month.parse::<u32>(),
                day.parse::<u32>(),
            ) else {
                return Err(create_invalid_text_error(input, "date"));
            };
            if year <= 0 {
                return Err(create_invalid_text_error(input, "date"));
            }
            chrono::NaiveDate::from_ymd_opt(1 - year, month, day)
                .map(PgDate::Finite)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::DatetimeFieldOverflow,
                        format!("date/time field value out of range: {input}"),
                    )
                })
        }
        _ => chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
            .map(PgDate::Finite)
            .map_err(|error| match error.kind() {
                chrono::format::ParseErrorKind::OutOfRange
                | chrono::format::ParseErrorKind::Impossible => PgError::create(
                    SqlState::DatetimeFieldOverflow,
                    format!("date/time field value out of range: {input}"),
                ),
                _ => create_invalid_text_error(input, "date"),
            }),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_time(input: &str) -> Result<PgTime> {
    let input = input.trim();
    if input == "24:00" || input == "24:00:00" || input == "24:00:00.0" {
        return Ok(PgTime(MICROSECONDS_PER_DAY));
    }
    let value = chrono::NaiveTime::parse_from_str(input, "%H:%M:%S%.f")
        .or_else(|_| chrono::NaiveTime::parse_from_str(input, "%H:%M"))
        .map_err(|_| create_invalid_text_error(input, "time"))?;
    Ok(PgTime(
        i64::from(value.num_seconds_from_midnight()) * 1_000_000
            + i64::from(value.nanosecond() / 1_000),
    ))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_timestamp(input: &str) -> Result<PgTimestamp> {
    let input = input.trim();
    match input.to_ascii_lowercase().as_str() {
        "infinity" => return Ok(PgTimestamp::Infinity),
        "-infinity" => return Ok(PgTimestamp::NegInfinity),
        _ => {}
    }
    // PostgreSQL accepts a time-zone suffix for timestamp input, validates it,
    // then discards it. RFC3339 covers the unambiguous offset spellings.
    if let Ok(value) = DateTime::parse_from_rfc3339(&normalize_rfc3339_input(input)) {
        return Ok(PgTimestamp::Finite(value.naive_local()));
    }
    parse_local_timestamp(input)
        .map(PgTimestamp::Finite)
        .ok_or_else(|| create_invalid_text_error(input, "timestamp"))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_timestamptz(input: &str) -> Result<PgTimestampTz> {
    let input = input.trim();
    match input.to_ascii_lowercase().as_str() {
        "infinity" => return Ok(PgTimestampTz::Infinity),
        "-infinity" => return Ok(PgTimestampTz::NegInfinity),
        _ => {}
    }
    let normalized = normalize_rfc3339_input(input);
    if let Ok(value) = DateTime::parse_from_rfc3339(&normalized)
        .or_else(|_| DateTime::parse_from_str(&normalized, "%Y-%m-%dT%H:%M:%S%.f%:z"))
    {
        return Ok(PgTimestampTz::Finite(value.with_timezone(&Utc)));
    }
    parse_local_timestamp(input)
        .map(|value| PgTimestampTz::Finite(value.and_utc()))
        .ok_or_else(|| {
            PgError::create(
                SqlState::InvalidDatetimeFormat,
                format!("invalid input syntax for type timestamp with time zone: {input}"),
            )
        })
}

pub(crate) fn parse_local_timestamp(input: &str) -> Option<NaiveDateTime> {
    let input = input.trim();
    [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(input, format).ok())
    .or_else(|| {
        chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn normalize_rfc3339_input(input: &str) -> String {
    let mut input = input.replacen(' ', "T", 1);
    if input.len() > 10
        && input.is_char_boundary(10)
        && input
            .as_bytes()
            .get(10)
            .is_some_and(|byte| matches!(byte, b'+' | b'-' | b'Z'))
    {
        input.insert_str(10, "T00:00:00");
    }
    if input.len() >= 3 {
        let suffix = &input[input.len() - 3..];
        if suffix.starts_with('+') || suffix.starts_with('-') {
            input.push_str(":00");
        }
    }
    input
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_bool(input: &str) -> Result<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "t" | "tr" | "tru" | "true" | "y" | "ye" | "yes" | "on" | "1" => Ok(true),
        "f" | "fa" | "fal" | "fals" | "false" | "n" | "no" | "of" | "off" | "0" => Ok(false),
        _ => Err(create_invalid_text_error(input, "boolean")),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_int<T: std::str::FromStr<Err = std::num::ParseIntError>>(input: &str) -> Result<T> {
    input.trim().parse::<T>().map_err(|e| match e.kind() {
        std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
            PgError::create(
                SqlState::NumericValueOutOfRange,
                format!("value out of range for type: {input}"),
            )
        }
        _ => create_invalid_text_error(input, "integer"),
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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
    let v = T::from_str(s).map_err(|_| create_invalid_text_error(input, "floating point"))?;
    if v.is_infinite() && !is_inf_literal {
        return Err(PgError::create(
            SqlState::NumericValueOutOfRange,
            format!("value out of range for type: {input}"),
        ));
    }
    if v.is_zero()
        && s.split(['e', 'E']).next().is_some_and(|significand| {
            significand
                .bytes()
                .any(|digit| matches!(digit, b'1'..=b'9'))
        })
    {
        return Err(PgError::create(
            SqlState::NumericValueOutOfRange,
            format!("value out of range for type: {input}"),
        ));
    }
    Ok(v)
}

trait FloatExt: Copy + std::str::FromStr<Err = std::num::ParseFloatError> {
    fn is_infinite(self) -> bool;
    fn is_zero(self) -> bool;
}
impl FloatExt for f32 {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn is_infinite(self) -> bool {
        f32::is_infinite(self)
    }
    fn is_zero(self) -> bool {
        self == 0.0
    }
}
impl FloatExt for f64 {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn is_infinite(self) -> bool {
        f64::is_infinite(self)
    }
    fn is_zero(self) -> bool {
        self == 0.0
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_bytea(input: &str) -> Result<Vec<u8>> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix("\\x") {
        decode_hex(hex).map_err(|_| create_invalid_text_error(input, "bytea"))
    } else {
        parse_bytea_escape(s).map_err(|_| create_invalid_text_error(input, "bytea"))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn decode_hex(hex: &str) -> std::result::Result<Vec<u8>, ()> {
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
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_invalid_text_error(input: &str, type_name: &str) -> PgError {
    PgError::create(
        SqlState::InvalidTextRepresentation,
        format!("invalid input syntax for type {type_name}: {input:?}"),
    )
}

#[cfg(test)]
#[path = "value_test.rs"]
mod tests;
