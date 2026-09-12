use crate::{
    error::{PgError, Result, SqlState},
    value::{PgTimestamp, PgTimestampTz, Value},
};
use chrono::{DateTime, Duration, FixedOffset, LocalResult, NaiveDateTime, Offset, TimeZone, Utc};

#[derive(Clone, Copy)]
pub(crate) enum Zone {
    Fixed(FixedOffset),
    Named(chrono_tz::Tz),
}

pub(crate) fn parse_zone(zone: &str) -> Result<Zone> {
    let upper = zone.to_ascii_uppercase();
    if matches!(upper.as_str(), "UTC" | "GMT" | "Z") {
        return Ok(Zone::Fixed(FixedOffset::east_opt(0).unwrap()));
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
        return Ok(Zone::Fixed(FixedOffset::east_opt(seconds).unwrap()));
    }
    let offset = upper
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
        .unwrap_or(&upper);
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
        if hour > 15 || minute > 59 || second > 59 {
            return Err(invalid());
        }
        // PostgreSQL's text zone offsets use POSIX's west-positive sign.
        let seconds =
            (hour * 3600 + minute * 60 + second) * if offset.starts_with('-') { 1 } else { -1 };
        return Ok(Zone::Fixed(
            FixedOffset::east_opt(seconds).expect("validated zone offset"),
        ));
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
        Zone::Fixed(zone) => zone
            .from_local_datetime(&value)
            .map(|date| date.with_timezone(&Utc)),
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
        Zone::Named(zone) => zone.offset_from_utc_datetime(&value.naive_utc()).fix(),
    };
    let local = value
        .naive_utc()
        .checked_add_signed(Duration::seconds(i64::from(offset.local_minus_utc())))
        .ok_or_else(create_overflow)?;
    Ok((local, offset.local_minus_utc()))
}

pub(crate) fn convert_time_zone(value: Value, zone: &str) -> Result<Value> {
    match value {
        Value::Null => Ok(Value::Null),
        Value::Timestamp(PgTimestamp::Finite(value)) => Ok(Value::TimestampTz(
            PgTimestampTz::Finite(convert_local(parse_zone(zone)?, value)?),
        )),
        Value::TimestampTz(PgTimestampTz::Finite(value)) => Ok(Value::Timestamp(
            PgTimestamp::Finite(convert_utc(parse_zone(zone)?, value)?.0),
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
