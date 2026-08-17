use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDateTime, Timelike, Utc};
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
    Uuid,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
}

impl BaseType {
    /// The `pg_type` OID for this base type.
    pub fn map_to_oid(self) -> Oid {
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
            BaseType::Uuid => 2950,
            BaseType::Date => 1082,
            BaseType::Time => 1083,
            BaseType::Timestamp => 1114,
            BaseType::TimestampTz => 1184,
            BaseType::Interval => 1186,
        }
    }

    /// The canonical (internal) PostgreSQL type name, e.g. `int4`, `bpchar`.
    pub fn get_postgres_name(self) -> &'static str {
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
            BaseType::Uuid => "uuid",
            BaseType::Date => "date",
            BaseType::Time => "time",
            BaseType::Timestamp => "timestamp",
            BaseType::TimestampTz => "timestamptz",
            BaseType::Interval => "interval",
        }
    }

    /// Look up a base type by OID.
    pub fn resolve_oid(oid: Oid) -> Option<BaseType> {
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
            2950 => Some(BaseType::Uuid),
            1082 => Some(BaseType::Date),
            1083 => Some(BaseType::Time),
            1114 => Some(BaseType::Timestamp),
            1184 => Some(BaseType::TimestampTz),
            1186 => Some(BaseType::Interval),
            _ => None,
        }
    }

    /// Look up a base type by SQL type name, accepting common aliases.
    /// Matching is case-insensitive.
    pub(crate) fn parse_sql_name(name: &str) -> Option<BaseType> {
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
            "uuid" => Some(BaseType::Uuid),
            "date" => Some(BaseType::Date),
            "time" | "time without time zone" => Some(BaseType::Time),
            "timestamp" | "timestamp without time zone" => Some(BaseType::Timestamp),
            "timestamptz" | "timestamp with time zone" => Some(BaseType::TimestampTz),
            "interval" => Some(BaseType::Interval),
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

    pub(crate) fn create(base: BaseType) -> Self {
        PgType {
            base,
            typmod: Self::NO_TYPEMOD,
        }
    }

    pub(crate) fn create_with_typmod(base: BaseType, typmod: i32) -> Self {
        PgType { base, typmod }
    }

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
    Bool(bool),
    Int2(i16),
    Int4(i32),
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
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The base type of this value, or `None` for `Null`.
    ///
    /// For `Text` values the result is `BaseType::Text`; the catalog
    /// disambiguates `varchar`/`bpchar`.
    pub fn get_base_type(&self) -> Option<BaseType> {
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
            Value::Uuid(_) => Some(BaseType::Uuid),
            Value::Date(_) => Some(BaseType::Date),
            Value::Time(_) => Some(BaseType::Time),
            Value::Timestamp(_) => Some(BaseType::Timestamp),
            Value::TimestampTz(_) => Some(BaseType::TimestampTz),
            Value::Interval(_) => Some(BaseType::Interval),
        }
    }

    /// Render to PostgreSQL text output form (the type's `typoutput` function).
    ///
    /// For `Null` returns an empty string; callers that need to distinguish
    /// NULL should check `is_null()` first.
    pub fn format_postgres_text(&self) -> String {
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
            Value::Date(PgDate::Finite(value)) => value.format("%Y-%m-%d").to_string(),
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
        }
    }

    /// Parse a text input literal into a `Value` of the given base type
    /// (the type's `typinput` function). Invalid syntax yields `22P02`;
    /// out-of-range values yield `22003`.
    pub(crate) fn parse(base: BaseType, input: &str) -> Result<Value> {
        match base {
            BaseType::Bool => parse_bool(input).map(Value::Bool),
            BaseType::Int2 => parse_int::<i16>(input).map(Value::Int2),
            BaseType::Int4 => parse_int::<i32>(input).map(Value::Int4),
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
        }
    }
}

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
            value.micros = value
                .micros
                .checked_add(
                    sign * (hour * 3_600_000_000
                        + minute * 60_000_000
                        + (second * 1_000_000.0).round() as i64),
                )
                .ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
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
                                SqlState::NumericValueOutOfRange,
                                "interval out of range",
                            )
                        })?)
                        .map_err(|_| {
                            PgError::create(
                                SqlState::NumericValueOutOfRange,
                                "interval out of range",
                            )
                        })?,
                    )
                    .ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?
            }
            "mon" | "mons" | "month" | "months" => {
                value.months = value
                    .months
                    .checked_add(i32::try_from(number).map_err(|_| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?
            }
            "day" | "days" => {
                value.days = value
                    .days
                    .checked_add(i32::try_from(number).map_err(|_| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?
            }
            "hour" | "hours" => {
                value.micros = value
                    .micros
                    .checked_add(number.checked_mul(3_600_000_000).ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?
            }
            "minute" | "minutes" | "min" | "mins" => {
                value.micros = value
                    .micros
                    .checked_add(number.checked_mul(60_000_000).ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?
            }
            "second" | "seconds" | "sec" | "secs" => {
                value.micros = value
                    .micros
                    .checked_add(number.checked_mul(1_000_000).ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?)
                    .ok_or_else(|| {
                        PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                    })?
            }
            _ => return Err(create_invalid_text_error(input, "interval")),
        }
        index += 2;
    }
    Ok(value)
}

fn format_timestamp(value: NaiveDateTime) -> String {
    let output = value.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    output
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

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

fn parse_date(input: &str) -> Result<PgDate> {
    match input.trim().to_ascii_lowercase().as_str() {
        "infinity" => Ok(PgDate::Infinity),
        "-infinity" => Ok(PgDate::NegInfinity),
        _ => chrono::NaiveDate::parse_from_str(input.trim(), "%Y-%m-%d")
            .map(PgDate::Finite)
            .map_err(|_| create_invalid_text_error(input, "date")),
    }
}

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
    [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(input, format).ok())
    .map(PgTimestamp::Finite)
    .ok_or_else(|| create_invalid_text_error(input, "timestamp"))
}

fn parse_timestamptz(input: &str) -> Result<PgTimestampTz> {
    let input = input.trim();
    match input.to_ascii_lowercase().as_str() {
        "infinity" => return Ok(PgTimestampTz::Infinity),
        "-infinity" => return Ok(PgTimestampTz::NegInfinity),
        _ => {}
    }
    if let Ok(value) = DateTime::parse_from_rfc3339(&normalize_rfc3339_input(input)) {
        return Ok(PgTimestampTz::Finite(value.with_timezone(&Utc)));
    }
    [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(input, format).ok())
    .map(|value| PgTimestampTz::Finite(value.and_utc()))
    .ok_or_else(|| create_invalid_text_error(input, "timestamp with time zone"))
}

fn normalize_rfc3339_input(input: &str) -> String {
    let mut input = input.replacen(' ', "T", 1);
    if input.len() >= 3 {
        let suffix = &input[input.len() - 3..];
        if suffix.starts_with('+') || suffix.starts_with('-') {
            input.push_str(":00");
        }
    }
    input
}

fn parse_bool(input: &str) -> Result<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "y" | "yes" | "on" | "1" => Ok(true),
        "f" | "false" | "n" | "no" | "off" | "0" => Ok(false),
        _ => Err(create_invalid_text_error(input, "boolean")),
    }
}

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
        decode_hex(hex).map_err(|_| create_invalid_text_error(input, "bytea"))
    } else {
        parse_bytea_escape(s).map_err(|_| create_invalid_text_error(input, "bytea"))
    }
}

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

fn create_invalid_text_error(input: &str, type_name: &str) -> PgError {
    PgError::create(
        SqlState::InvalidTextRepresentation,
        format!("invalid input syntax for type {type_name}: {input:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_pg_type_typmod() {
        let t = PgType::create(BaseType::Varchar);
        assert_eq!(t.typmod, PgType::NO_TYPEMOD);
        let t = PgType::create_with_typmod(BaseType::Varchar, 14);
        assert_ne!(t.typmod, PgType::NO_TYPEMOD);
        assert_eq!(t.map_to_oid(), 1043);
    }

    #[test]
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
    fn reports_22003_for_integer_overflow() {
        let err = Value::parse(BaseType::Int2, "40000").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
        let err = Value::parse(BaseType::Int4, "3000000000").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    }

    #[test]
    fn reports_22p02_for_invalid_integer_syntax() {
        let err = Value::parse(BaseType::Int4, "abc").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn accepts_postgres_boolean_forms() {
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
    fn reports_22003_for_float_overflow() {
        let err = Value::parse(BaseType::Float4, "1e999").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::NumericValueOutOfRange);
    }

    #[test]
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
    fn preserves_numeric_scale() {
        let v = Value::parse(BaseType::Numeric, "1.10").unwrap();
        assert_eq!(v.format_postgres_text(), "1.10");
        let v = Value::parse(BaseType::Numeric, "0.001").unwrap();
        assert_eq!(v.format_postgres_text(), "0.001");
    }

    #[test]
    fn reports_22p02_for_invalid_numeric() {
        let err = Value::parse(BaseType::Numeric, "1.2.3").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }

    #[test]
    fn roundtrips_text() {
        for base in [BaseType::Text, BaseType::Varchar, BaseType::Bpchar] {
            let v = Value::parse(base, "hello world").unwrap();
            assert_eq!(v, Value::Text("hello world".into()));
            assert_eq!(v.format_postgres_text(), "hello world");
        }
    }

    #[test]
    fn roundtrips_hex_bytea() {
        let v = Value::parse(BaseType::Bytea, "\\x414243").unwrap();
        assert_eq!(v, Value::Bytea(vec![0x41, 0x42, 0x43]));
        assert_eq!(v.format_postgres_text(), "\\x414243");
        // empty
        let v = Value::parse(BaseType::Bytea, "\\x").unwrap();
        assert_eq!(v, Value::Bytea(vec![]));
    }

    #[test]
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
    fn reports_22p02_for_invalid_bytea() {
        let err = Value::parse(BaseType::Bytea, "\\xZZ").unwrap_err();
        assert_eq!(err.sqlstate, SqlState::InvalidTextRepresentation);
    }
}
