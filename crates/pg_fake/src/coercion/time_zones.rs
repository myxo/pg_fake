use crate::{
    error::{PgError, Result, SqlState},
    value::{PgTimestamp, PgTimestampTz, Value},
};
use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};

#[derive(Clone, Copy)]
pub(crate) enum Zone {
    Fixed(i32),
    Named(chrono_tz::Tz),
}

pub(crate) fn parse_zone(zone: &str) -> Result<Zone> {
    let upper = zone.to_ascii_uppercase();
    if matches!(upper.as_str(), "UTC" | "GMT" | "Z") {
        return Ok(Zone::Fixed(0));
    }
    let abbreviation = match upper.as_str() {
        "CET" | "MET" => Some(3600),
        "CEST" | "MEST" | "EET" => Some(7200),
        "EEST" => Some(10800),
        "WET" => Some(0),
        "WEST" => Some(3600),
        "EST" => Some(-18000),
        "EDT" | "AST" => Some(-14400),
        "CST" => Some(-21600),
        "CDT" => Some(-18000),
        "MST" => Some(-25200),
        "MDT" => Some(-21600),
        "PST" => Some(-28800),
        "PDT" => Some(-25200),
        "HST" => Some(-36000),
        _ => None,
    };
    if let Some(seconds) = abbreviation {
        return Ok(Zone::Fixed(seconds));
    }
    let upper_offset = upper
        .strip_prefix('<')
        .and_then(|value| value.split_once('>'))
        .map_or(upper.as_str(), |(_, offset)| offset);
    let offset = upper_offset
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
        .unwrap_or(upper_offset);
    if offset.starts_with(['+', '-']) {
        let invalid = || {
            PgError::create(
                SqlState::InvalidParameterValue,
                format!("time zone {zone:?} not recognized"),
            )
        };
        let components = offset[1..].split(':').collect::<Vec<_>>();
        if components.is_empty()
            || components.len() > 3
            || components
                .iter()
                .any(|c| c.is_empty() || !c.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(invalid());
        }
        let numbers = components
            .iter()
            .map(|c| c.parse::<i32>().map_err(|_| invalid()))
            .collect::<Result<Vec<_>>>()?;
        let hour = numbers[0];
        let minute = numbers.get(1).copied().unwrap_or(0);
        let second = numbers.get(2).copied().unwrap_or(0);
        if hour > 167 || minute > 59 || second > 59 {
            return Err(invalid());
        }
        // PostgreSQL's text zone offsets use POSIX's west-positive sign.
        let seconds =
            (hour * 3600 + minute * 60 + second) * if offset.starts_with('-') { 1 } else { -1 };
        return Ok(Zone::Fixed(seconds));
    }
    if let Some(zone) = chrono_tz::TZ_VARIANTS
        .iter()
        .find(|candidate| candidate.name().eq_ignore_ascii_case(zone))
    {
        return Ok(Zone::Named(*zone));
    }
    Err(PgError::create(
        SqlState::InvalidParameterValue,
        format!("time zone {zone:?} not recognized"),
    ))
}

pub(crate) fn convert_local(zone: Zone, value: NaiveDateTime) -> Result<DateTime<Utc>> {
    let result = match zone {
        Zone::Fixed(seconds) => {
            return value
                .checked_sub_signed(Duration::seconds(i64::from(seconds)))
                .map(|value| value.and_utc())
                .ok_or_else(create_overflow);
        }
        Zone::Named(zone) => zone
            .from_local_datetime(&value)
            .map(|date| date.with_timezone(&Utc)),
    };
    match result {
        LocalResult::Single(value) => Ok(value),
        LocalResult::Ambiguous(first, second) => Ok(first.max(second)),
        LocalResult::None => {
            // Gaps use the pre-transition offset; overlaps use the later instant.
            let Zone::Named(zone) = zone else {
                return Err(create_overflow());
            };
            for hours in 1..=48 {
                let before = value
                    .checked_sub_signed(Duration::hours(hours))
                    .ok_or_else(create_overflow)?;
                if let Some(before) = zone.from_local_datetime(&before).latest() {
                    let offset = before.offset().fix();
                    return offset
                        .from_local_datetime(&value)
                        .single()
                        .map(|date| date.with_timezone(&Utc))
                        .ok_or_else(create_overflow);
                }
            }
            Err(create_overflow())
        }
    }
}

pub(crate) fn convert_utc(zone: Zone, value: DateTime<Utc>) -> Result<(NaiveDateTime, i32)> {
    let offset = match zone {
        Zone::Fixed(zone) => zone,
        Zone::Named(zone) => zone
            .offset_from_utc_datetime(&value.naive_utc())
            .fix()
            .local_minus_utc(),
    };
    let local = value
        .naive_utc()
        .checked_add_signed(Duration::seconds(i64::from(offset)))
        .ok_or_else(create_overflow)?;
    Ok((local, offset))
}

pub(crate) fn convert_time_zone(value: Value, zone: &str) -> Result<Value> {
    convert_zone(value, || parse_zone(zone))
}

pub(crate) fn parse_session_zone(zone: &str) -> Result<Zone> {
    chrono_tz::TZ_VARIANTS
        .iter()
        .find(|candidate| candidate.name().eq_ignore_ascii_case(zone))
        .map(|zone| Ok(Zone::Named(*zone)))
        .unwrap_or_else(|| parse_zone(zone))
}

pub(crate) fn convert_session_time_zone(value: Value, zone: &str) -> Result<Value> {
    convert_zone(value, || parse_session_zone(zone))
}

fn convert_zone(value: Value, parse: impl FnOnce() -> Result<Zone>) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Timestamp(PgTimestamp::Finite(value)) => Ok(Value::TimestampTz(
            PgTimestampTz::Finite(convert_local(parse()?, value)?),
        )),
        Value::TimestampTz(PgTimestampTz::Finite(value)) => Ok(Value::Timestamp(
            PgTimestamp::Finite(convert_utc(parse()?, value)?.0),
        )),
        Value::Timestamp(PgTimestamp::Infinity) => Ok(Value::TimestampTz(PgTimestampTz::Infinity)),
        Value::Timestamp(PgTimestamp::NegInfinity) => {
            Ok(Value::TimestampTz(PgTimestampTz::NegInfinity))
        }
        Value::TimestampTz(PgTimestampTz::Infinity) => Ok(Value::Timestamp(PgTimestamp::Infinity)),
        Value::TimestampTz(PgTimestampTz::NegInfinity) => {
            Ok(Value::Timestamp(PgTimestamp::NegInfinity))
        }
        _ => unreachable!("time zone argument was coerced"),
    }
}

fn create_overflow() -> PgError {
    PgError::create(SqlState::DatetimeFieldOverflow, "timestamp out of range")
}
