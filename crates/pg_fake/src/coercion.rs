use bigdecimal::{BigDecimal, FromPrimitive, RoundingMode, ToPrimitive};
use chrono::Timelike;
use sqlparser::ast::{CharacterLength, DataType, ExactNumberInfo, TimezoneInfo};

use crate::{
    error::{PgError, Result, SqlState},
    value::{BaseType, PgType, Value},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CastContext {
    Implicit,
    Assignment,
    Explicit,
}

pub(crate) fn type_from_ast(data_type: &DataType) -> Result<PgType> {
    let (base, typmod) = match data_type {
        DataType::Bool | DataType::Boolean => (BaseType::Bool, PgType::NO_TYPEMOD),
        DataType::Int2(None) | DataType::SmallInt(None) => (BaseType::Int2, PgType::NO_TYPEMOD),
        DataType::Int(None) | DataType::Int4(None) | DataType::Integer(None) => {
            (BaseType::Int4, PgType::NO_TYPEMOD)
        }
        DataType::Int8(None) | DataType::BigInt(None) => (BaseType::Int8, PgType::NO_TYPEMOD),
        DataType::Real | DataType::Float4 => (BaseType::Float4, PgType::NO_TYPEMOD),
        DataType::Double | DataType::DoublePrecision | DataType::Float8 | DataType::Float(None) => {
            (BaseType::Float8, PgType::NO_TYPEMOD)
        }
        DataType::Numeric(info) | DataType::Decimal(info) => {
            (BaseType::Numeric, numeric_typmod(*info)?)
        }
        DataType::Text => (BaseType::Text, PgType::NO_TYPEMOD),
        DataType::Varchar(length)
        | DataType::CharacterVarying(length)
        | DataType::CharVarying(length) => (BaseType::Varchar, character_typmod(*length, false)?),
        DataType::Char(length) | DataType::Character(length) => {
            (BaseType::Bpchar, character_typmod(*length, true)?)
        }
        DataType::Bytea => (BaseType::Bytea, PgType::NO_TYPEMOD),
        DataType::Uuid => (BaseType::Uuid, PgType::NO_TYPEMOD),
        DataType::Date => (BaseType::Date, PgType::NO_TYPEMOD),
        DataType::Time(precision, TimezoneInfo::None | TimezoneInfo::WithoutTimeZone) => {
            (BaseType::Time, time_typmod(*precision)?)
        }
        DataType::Timestamp(precision, TimezoneInfo::None | TimezoneInfo::WithoutTimeZone) => {
            (BaseType::Timestamp, time_typmod(*precision)?)
        }
        DataType::Timestamp(precision, TimezoneInfo::WithTimeZone | TimezoneInfo::Tz) => {
            (BaseType::TimestampTz, time_typmod(*precision)?)
        }
        DataType::Interval => (BaseType::Interval, PgType::NO_TYPEMOD),
        DataType::Custom(_, _) => {
            let Some(base) = BaseType::from_name(&data_type.to_string()) else {
                return Err(PgError::new(
                    SqlState::UndefinedObject,
                    format!("type {data_type} does not exist"),
                ));
            };
            (base, PgType::NO_TYPEMOD)
        }
        _ => {
            return Err(PgError::new(
                SqlState::UndefinedObject,
                format!("type {data_type} does not exist"),
            ));
        }
    };
    Ok(PgType::with_typmod(base, typmod))
}

fn character_typmod(length: Option<CharacterLength>, default_one: bool) -> Result<i32> {
    let length = match length {
        Some(CharacterLength::IntegerLength { length, unit: None }) => Some(length),
        None if default_one => Some(1),
        None => None,
        _ => {
            return Err(PgError::new(
                SqlState::UndefinedObject,
                "character length is not supported",
            ));
        }
    };
    length
        .map(|length| {
            i32::try_from(length)
                .ok()
                .and_then(|length| length.checked_add(4))
                .ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "length is out of range")
                })
        })
        .transpose()
        .map(|typmod| typmod.unwrap_or(PgType::NO_TYPEMOD))
}

fn numeric_typmod(info: ExactNumberInfo) -> Result<i32> {
    let (precision, scale) = match info {
        ExactNumberInfo::None => return Ok(PgType::NO_TYPEMOD),
        ExactNumberInfo::Precision(precision) => (precision, 0),
        ExactNumberInfo::PrecisionAndScale(precision, scale) => (precision, scale),
    };
    if precision == 0 || precision > 1000 || scale > precision {
        return Err(PgError::new(
            SqlState::NumericValueOutOfRange,
            "numeric precision or scale is out of range",
        ));
    }
    Ok(((precision as i32) << 16) + scale as i32 + 4)
}

fn time_typmod(precision: Option<u64>) -> Result<i32> {
    match precision {
        None => Ok(PgType::NO_TYPEMOD),
        Some(precision) if precision <= 6 => Ok(precision as i32),
        Some(_) => Err(PgError::new(
            SqlState::InvalidParameterValue,
            "time precision must be between 0 and 6",
        )),
    }
}

pub(crate) fn common_type(left: BaseType, right: BaseType) -> Option<BaseType> {
    if left == right {
        return Some(left);
    }
    if string(left) && string(right) {
        return Some(BaseType::Text);
    }
    let left_rank = numeric_rank(left)?;
    let right_rank = numeric_rank(right)?;
    Some(if left_rank > right_rank { left } else { right })
}

pub(crate) fn can_cast(source: BaseType, target: BaseType, context: CastContext) -> bool {
    required_context(source, target).is_some_and(|required| context >= required)
}

fn required_context(source: BaseType, target: BaseType) -> Option<CastContext> {
    if source == target || string(source) && string(target) {
        return Some(CastContext::Implicit);
    }
    if numeric_rank(source).is_some() && numeric_rank(target).is_some() {
        return Some(if numeric_rank(source) <= numeric_rank(target) {
            CastContext::Implicit
        } else {
            CastContext::Assignment
        });
    }
    if string(target) {
        return Some(CastContext::Assignment);
    }
    if string(source) {
        if matches!(
            target,
            BaseType::Uuid
                | BaseType::Date
                | BaseType::Time
                | BaseType::Timestamp
                | BaseType::TimestampTz
                | BaseType::Interval
        ) {
            return Some(CastContext::Assignment);
        }
        return Some(CastContext::Explicit);
    }
    if matches!(
        source,
        BaseType::Uuid
            | BaseType::Date
            | BaseType::Time
            | BaseType::Timestamp
            | BaseType::TimestampTz
            | BaseType::Interval
    ) && string(target)
    {
        return Some(CastContext::Assignment);
    }
    if matches!(
        (source, target),
        (BaseType::Date, BaseType::Timestamp | BaseType::TimestampTz)
            | (BaseType::Timestamp, BaseType::Date | BaseType::TimestampTz)
            | (BaseType::TimestampTz, BaseType::Date | BaseType::Timestamp)
    ) {
        return Some(CastContext::Assignment);
    }
    if matches!(
        (source, target),
        (BaseType::Int4, BaseType::Bool)
            | (BaseType::Bool, BaseType::Int4)
            | (
                BaseType::Int2 | BaseType::Int4 | BaseType::Int8,
                BaseType::Bytea
            )
            | (
                BaseType::Bytea,
                BaseType::Int2 | BaseType::Int4 | BaseType::Int8
            )
    ) {
        return Some(CastContext::Explicit);
    }
    None
}

pub(crate) fn coerce(
    value: Value,
    source: BaseType,
    target: PgType,
    context: CastContext,
) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    if !can_cast(source, target.base, context) {
        return Err(cannot_cast(source, target.base));
    }
    let value = if source == target.base || string(source) && string(target.base) {
        value
    } else if string(target.base) {
        Value::Text(match value {
            Value::Bool(true) => "true".into(),
            Value::Bool(false) => "false".into(),
            value => value.to_text(),
        })
    } else if string(source) {
        let Value::Text(text) = value else {
            unreachable!("string values use Value::Text")
        };
        Value::parse(target.base, &text)?
    } else {
        convert_non_string(value, target.base)?
    };
    apply_typmod(value, target, context)
}

pub(crate) fn coerce_unknown(text: &str, target: PgType, context: CastContext) -> Result<Value> {
    let value = if string(target.base) {
        Value::Text(text.into())
    } else {
        Value::parse(target.base, text)?
    };
    apply_typmod(value, target, context)
}

fn convert_non_string(value: Value, target: BaseType) -> Result<Value> {
    match (value, target) {
        (Value::Date(crate::value::PgDate::Finite(value)), BaseType::Timestamp) => {
            Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(
                value.and_hms_opt(0, 0, 0).expect("midnight is valid"),
            )))
        }
        (Value::Date(crate::value::PgDate::Infinity), BaseType::Timestamp) => {
            Ok(Value::Timestamp(crate::value::PgTimestamp::Infinity))
        }
        (Value::Date(crate::value::PgDate::NegInfinity), BaseType::Timestamp) => {
            Ok(Value::Timestamp(crate::value::PgTimestamp::NegInfinity))
        }
        (Value::Date(crate::value::PgDate::Finite(value)), BaseType::TimestampTz) => {
            Ok(Value::TimestampTz(crate::value::PgTimestampTz::Finite(
                value
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is valid")
                    .and_utc(),
            )))
        }
        (Value::Date(crate::value::PgDate::Infinity), BaseType::TimestampTz) => {
            Ok(Value::TimestampTz(crate::value::PgTimestampTz::Infinity))
        }
        (Value::Date(crate::value::PgDate::NegInfinity), BaseType::TimestampTz) => {
            Ok(Value::TimestampTz(crate::value::PgTimestampTz::NegInfinity))
        }
        (Value::Timestamp(crate::value::PgTimestamp::Finite(value)), BaseType::Date) => {
            Ok(Value::Date(crate::value::PgDate::Finite(value.date())))
        }
        (Value::Timestamp(crate::value::PgTimestamp::Infinity), BaseType::Date) => {
            Ok(Value::Date(crate::value::PgDate::Infinity))
        }
        (Value::Timestamp(crate::value::PgTimestamp::NegInfinity), BaseType::Date) => {
            Ok(Value::Date(crate::value::PgDate::NegInfinity))
        }
        (Value::Timestamp(value), BaseType::TimestampTz) => Ok(Value::TimestampTz(match value {
            crate::value::PgTimestamp::NegInfinity => crate::value::PgTimestampTz::NegInfinity,
            crate::value::PgTimestamp::Infinity => crate::value::PgTimestampTz::Infinity,
            crate::value::PgTimestamp::Finite(value) => {
                crate::value::PgTimestampTz::Finite(value.and_utc())
            }
        })),
        (Value::TimestampTz(value), BaseType::Timestamp) => Ok(Value::Timestamp(match value {
            crate::value::PgTimestampTz::NegInfinity => crate::value::PgTimestamp::NegInfinity,
            crate::value::PgTimestampTz::Infinity => crate::value::PgTimestamp::Infinity,
            crate::value::PgTimestampTz::Finite(value) => {
                crate::value::PgTimestamp::Finite(value.naive_utc())
            }
        })),
        (Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)), BaseType::Date) => Ok(
            Value::Date(crate::value::PgDate::Finite(value.date_naive())),
        ),
        (Value::TimestampTz(crate::value::PgTimestampTz::Infinity), BaseType::Date) => {
            Ok(Value::Date(crate::value::PgDate::Infinity))
        }
        (Value::TimestampTz(crate::value::PgTimestampTz::NegInfinity), BaseType::Date) => {
            Ok(Value::Date(crate::value::PgDate::NegInfinity))
        }
        (Value::Int2(value), BaseType::Int4) => Ok(Value::Int4(value.into())),
        (Value::Int2(value), BaseType::Int8) => Ok(Value::Int8(value.into())),
        (Value::Int2(value), BaseType::Float4) => Ok(Value::Float4(value.into())),
        (Value::Int2(value), BaseType::Float8) => Ok(Value::Float8(value.into())),
        (Value::Int2(value), BaseType::Numeric) => Ok(Value::Numeric(value.into())),
        (Value::Int4(value), BaseType::Int2) => i16::try_from(value)
            .map(Value::Int2)
            .map_err(|_| out_of_range(BaseType::Int2)),
        (Value::Int4(value), BaseType::Int8) => Ok(Value::Int8(value.into())),
        (Value::Int4(value), BaseType::Float4) => Ok(Value::Float4(value as f32)),
        (Value::Int4(value), BaseType::Float8) => Ok(Value::Float8(value.into())),
        (Value::Int4(value), BaseType::Numeric) => Ok(Value::Numeric(value.into())),
        (Value::Int8(value), BaseType::Int2) => i16::try_from(value)
            .map(Value::Int2)
            .map_err(|_| out_of_range(BaseType::Int2)),
        (Value::Int8(value), BaseType::Int4) => i32::try_from(value)
            .map(Value::Int4)
            .map_err(|_| out_of_range(BaseType::Int4)),
        (Value::Int8(value), BaseType::Float4) => Ok(Value::Float4(value as f32)),
        (Value::Int8(value), BaseType::Float8) => Ok(Value::Float8(value as f64)),
        (Value::Int8(value), BaseType::Numeric) => Ok(Value::Numeric(value.into())),
        (Value::Float4(value), BaseType::Int2) => float_to_int(value as f64, BaseType::Int2),
        (Value::Float4(value), BaseType::Int4) => float_to_int(value as f64, BaseType::Int4),
        (Value::Float4(value), BaseType::Int8) => float_to_int(value as f64, BaseType::Int8),
        (Value::Float4(value), BaseType::Float8) => Ok(Value::Float8(value.into())),
        (Value::Float4(value), BaseType::Numeric) => BigDecimal::from_f32(value)
            .map(Value::Numeric)
            .ok_or_else(|| out_of_range(BaseType::Numeric)),
        (Value::Float8(value), BaseType::Int2) => float_to_int(value, BaseType::Int2),
        (Value::Float8(value), BaseType::Int4) => float_to_int(value, BaseType::Int4),
        (Value::Float8(value), BaseType::Int8) => float_to_int(value, BaseType::Int8),
        (Value::Float8(value), BaseType::Float4) => {
            let converted = value as f32;
            if converted.is_infinite() && value.is_finite() {
                Err(out_of_range(BaseType::Float4))
            } else {
                Ok(Value::Float4(converted))
            }
        }
        (Value::Float8(value), BaseType::Numeric) => BigDecimal::from_f64(value)
            .map(Value::Numeric)
            .ok_or_else(|| out_of_range(BaseType::Numeric)),
        (Value::Numeric(value), BaseType::Int2) => numeric_to_int(value, BaseType::Int2),
        (Value::Numeric(value), BaseType::Int4) => numeric_to_int(value, BaseType::Int4),
        (Value::Numeric(value), BaseType::Int8) => numeric_to_int(value, BaseType::Int8),
        (Value::Numeric(value), BaseType::Float4) => value
            .to_f32()
            .filter(|value| value.is_finite())
            .map(Value::Float4)
            .ok_or_else(|| out_of_range(BaseType::Float4)),
        (Value::Numeric(value), BaseType::Float8) => value
            .to_f64()
            .filter(|value| value.is_finite())
            .map(Value::Float8)
            .ok_or_else(|| out_of_range(BaseType::Float8)),
        (Value::Int4(value), BaseType::Bool) => Ok(Value::Bool(value != 0)),
        (Value::Bool(value), BaseType::Int4) => Ok(Value::Int4(i32::from(value))),
        (Value::Int2(value), BaseType::Bytea) => Ok(Value::Bytea(value.to_be_bytes().into())),
        (Value::Int4(value), BaseType::Bytea) => Ok(Value::Bytea(value.to_be_bytes().into())),
        (Value::Int8(value), BaseType::Bytea) => Ok(Value::Bytea(value.to_be_bytes().into())),
        (Value::Bytea(value), BaseType::Int2) => {
            let bytes = <[u8; 2]>::try_from(value).map_err(|_| out_of_range(BaseType::Int2))?;
            Ok(Value::Int2(i16::from_be_bytes(bytes)))
        }
        (Value::Bytea(value), BaseType::Int4) => {
            let bytes = <[u8; 4]>::try_from(value).map_err(|_| out_of_range(BaseType::Int4))?;
            Ok(Value::Int4(i32::from_be_bytes(bytes)))
        }
        (Value::Bytea(value), BaseType::Int8) => {
            let bytes = <[u8; 8]>::try_from(value).map_err(|_| out_of_range(BaseType::Int8))?;
            Ok(Value::Int8(i64::from_be_bytes(bytes)))
        }
        _ => unreachable!("cast matrix and conversion implementation must agree"),
    }
}

fn float_to_int(value: f64, target: BaseType) -> Result<Value> {
    if !value.is_finite() {
        return Err(out_of_range(target));
    }
    let rounded = value.round();
    match target {
        BaseType::Int2 => rounded
            .to_i16()
            .map(Value::Int2)
            .ok_or_else(|| out_of_range(target)),
        BaseType::Int4 => rounded
            .to_i32()
            .map(Value::Int4)
            .ok_or_else(|| out_of_range(target)),
        BaseType::Int8 => rounded
            .to_i64()
            .map(Value::Int8)
            .ok_or_else(|| out_of_range(target)),
        _ => unreachable!("float target must be an integer"),
    }
}

fn numeric_to_int(value: BigDecimal, target: BaseType) -> Result<Value> {
    let rounded = value.with_scale_round(0, RoundingMode::HalfUp);
    match target {
        BaseType::Int2 => rounded
            .to_i16()
            .map(Value::Int2)
            .ok_or_else(|| out_of_range(target)),
        BaseType::Int4 => rounded
            .to_i32()
            .map(Value::Int4)
            .ok_or_else(|| out_of_range(target)),
        BaseType::Int8 => rounded
            .to_i64()
            .map(Value::Int8)
            .ok_or_else(|| out_of_range(target)),
        _ => unreachable!("numeric target must be an integer"),
    }
}

fn apply_typmod(value: Value, target: PgType, context: CastContext) -> Result<Value> {
    if target.typmod == PgType::NO_TYPEMOD {
        return Ok(value);
    }
    match (value, target.base) {
        (Value::Text(value), BaseType::Varchar | BaseType::Bpchar) => {
            let maximum = usize::try_from(target.typmod - 4).expect("valid character typmod");
            let mut characters = value.chars().collect::<Vec<_>>();
            if characters.len() > maximum {
                if context == CastContext::Explicit
                    || characters[maximum..]
                        .iter()
                        .all(|character| *character == ' ')
                {
                    characters.truncate(maximum);
                } else {
                    return Err(PgError::new(
                        SqlState::StringDataRightTruncation,
                        "value too long for character type",
                    ));
                }
            }
            if target.base == BaseType::Bpchar {
                characters.resize(maximum, ' ');
            }
            Ok(Value::Text(characters.into_iter().collect()))
        }
        (Value::Numeric(value), BaseType::Numeric) => {
            let encoded = target.typmod - 4;
            let precision = encoded >> 16;
            let scale = encoded & 0xffff;
            let value = value.with_scale_round(scale.into(), RoundingMode::HalfUp);
            let plain = value.abs().to_plain_string();
            let integer_digits = plain
                .split('.')
                .next()
                .expect("numeric text has an integer part")
                .trim_start_matches('0')
                .len() as i32;
            if integer_digits > precision - scale {
                return Err(out_of_range(BaseType::Numeric));
            }
            Ok(Value::Numeric(value))
        }
        (Value::Time(value), BaseType::Time) => {
            let precision = u32::try_from(target.typmod).expect("time typmod is non-negative");
            let unit = 10_i64.pow(6 - precision);
            Ok(Value::Time(crate::value::PgTime(
                ((value.0 + unit / 2) / unit * unit).min(86_400_000_000),
            )))
        }
        (Value::Timestamp(value), BaseType::Timestamp) => {
            Ok(Value::Timestamp(round_timestamp(value, target.typmod)?))
        }
        (Value::TimestampTz(value), BaseType::TimestampTz) => {
            Ok(Value::TimestampTz(round_timestamptz(value, target.typmod)?))
        }
        (value, _) => Ok(value),
    }
}

fn round_timestamp(
    value: crate::value::PgTimestamp,
    typmod: i32,
) -> Result<crate::value::PgTimestamp> {
    let precision = u32::try_from(typmod).map_err(|_| out_of_range(BaseType::Timestamp))?;
    let unit = 10_u32.pow(6 - precision);
    Ok(match value {
        crate::value::PgTimestamp::Finite(value) => crate::value::PgTimestamp::Finite(
            value
                .with_nanosecond((value.nanosecond() / (unit * 1_000)) * unit * 1_000)
                .expect("rounded timestamp remains valid"),
        ),
        value => value,
    })
}

fn round_timestamptz(
    value: crate::value::PgTimestampTz,
    typmod: i32,
) -> Result<crate::value::PgTimestampTz> {
    let precision = u32::try_from(typmod).map_err(|_| out_of_range(BaseType::TimestampTz))?;
    let unit = 10_u32.pow(6 - precision);
    Ok(match value {
        crate::value::PgTimestampTz::Finite(value) => crate::value::PgTimestampTz::Finite(
            value
                .with_nanosecond((value.nanosecond() / (unit * 1_000)) * unit * 1_000)
                .expect("rounded timestamp remains valid"),
        ),
        value => value,
    })
}

fn numeric_rank(base: BaseType) -> Option<u8> {
    match base {
        BaseType::Int2 => Some(0),
        BaseType::Int4 => Some(1),
        BaseType::Int8 => Some(2),
        BaseType::Numeric => Some(3),
        BaseType::Float4 => Some(4),
        BaseType::Float8 => Some(5),
        _ => None,
    }
}

fn string(base: BaseType) -> bool {
    matches!(base, BaseType::Text | BaseType::Varchar | BaseType::Bpchar)
}

fn cannot_cast(source: BaseType, target: BaseType) -> PgError {
    PgError::new(
        SqlState::CannotCoerce,
        format!("cannot cast type {} to {}", source.name(), target.name()),
    )
}

fn out_of_range(target: BaseType) -> PgError {
    PgError::new(
        SqlState::NumericValueOutOfRange,
        format!("{} out of range", target.name()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_cast_contexts_follow_postgres_direction() {
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
    fn numeric_assignment_rounds_and_checks_ranges() {
        assert_eq!(
            coerce(
                Value::Numeric("2.5".parse().unwrap()),
                BaseType::Numeric,
                PgType::new(BaseType::Int4),
                CastContext::Assignment,
            )
            .unwrap(),
            Value::Int4(3)
        );
        assert_eq!(
            coerce(
                Value::Int4(32768),
                BaseType::Int4,
                PgType::new(BaseType::Int2),
                CastContext::Assignment,
            )
            .unwrap_err()
            .sqlstate,
            SqlState::NumericValueOutOfRange
        );
    }

    #[test]
    fn every_numeric_pair_has_an_assignment_conversion() {
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
                    PgType::new(target),
                    CastContext::Assignment,
                )
                .unwrap();
                assert_eq!(
                    converted.base_type(),
                    Some(target),
                    "{source:?} -> {target:?}"
                );
            }
        }
    }
}
