use crate::coercion::time_zones::{convert_local, convert_utc, parse_zone};
use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{PgTimestamp, PgTimestampTz, Value},
};
use chrono::{Datelike, Duration, NaiveDate, Timelike};

pub(super) fn convert_epoch(seconds: f64) -> Result<Value> {
    if seconds.is_infinite() {
        return Ok(Value::TimestampTz(if seconds.is_sign_positive() {
            PgTimestampTz::Infinity
        } else {
            PgTimestampTz::NegInfinity
        }));
    }
    if seconds.is_nan() || !(-210866803200.0..9224318016000.0).contains(&seconds) {
        return Err(create_overflow());
    }
    // Round after shifting to PostgreSQL's epoch, matching its float arithmetic.
    let micros = ((seconds - 946684800.0) * 1_000_000.0).round_ties_even();
    if !(-211813488000000000.0..9223371331200000000.0).contains(&micros) {
        return Err(create_overflow());
    }
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let Some(value) = epoch.checked_add_signed(Duration::microseconds(micros as i64)) else {
        return reject_unsupported("timestamp exceeds the supported calendar range");
    };
    Ok(Value::TimestampTz(PgTimestampTz::Finite(value.and_utc())))
}

pub(super) fn truncate_timestamp(unit: &str, value: Value, zone: &str) -> Result<Value> {
    let unit = unit.to_ascii_lowercase();
    let unit = match unit.as_str() {
        "microsecond" | "microseconds" => 0,
        "millisecond" | "milliseconds" => 1,
        "second" | "seconds" => 2,
        "minute" | "minutes" => 3,
        "hour" | "hours" => 4,
        "day" | "days" => 5,
        "week" | "weeks" => 6,
        "month" | "months" => 7,
        "quarter" => 8,
        "year" | "years" => 9,
        "decade" | "century" | "millennium" | "timezone" | "timezone_hour" | "timezone_minute" => {
            return reject_unsupported("truncation unit is not implemented");
        }
        _ => {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "unrecognized truncation unit",
            ));
        }
    };
    let (timestamp, zone, original_offset) = match value {
        Value::Timestamp(PgTimestamp::Finite(timestamp)) => (timestamp, None, 0),
        Value::TimestampTz(PgTimestampTz::Finite(timestamp)) => {
            let zone = parse_zone(zone)?;
            let (local, offset) = convert_utc(zone, timestamp)?;
            (local, Some(zone), offset)
        }
        Value::TimestampTz(_) => {
            parse_zone(zone)?;
            return Ok(value);
        }
        _ => return Ok(value),
    };
    let mut date = timestamp.date();
    if unit == 6 {
        date = date
            .checked_sub_signed(Duration::days(i64::from(
                date.weekday().num_days_from_monday(),
            )))
            .ok_or_else(create_overflow)?;
    }
    if unit >= 7 {
        let month = if unit == 9 {
            1
        } else if unit == 8 {
            (date.month() - 1) / 3 * 3 + 1
        } else {
            date.month()
        };
        date = NaiveDate::from_ymd_opt(date.year(), month, 1).ok_or_else(create_overflow)?;
    }
    let hour = if unit >= 5 { 0 } else { timestamp.hour() };
    let minute = if unit >= 4 { 0 } else { timestamp.minute() };
    let second = if unit >= 3 { 0 } else { timestamp.second() };
    let micros = match unit {
        0 => timestamp.nanosecond() / 1000,
        1 => timestamp.nanosecond() / 1_000_000 * 1000,
        _ => 0,
    };
    let result = date
        .and_hms_micro_opt(hour, minute, second, micros)
        .ok_or_else(create_overflow)?;
    Ok(if let Some(zone) = zone {
        let instant = if unit >= 5 {
            convert_local(zone, result)?
        } else {
            result
                .checked_sub_signed(Duration::seconds(i64::from(original_offset)))
                .ok_or_else(create_overflow)?
                .and_utc()
        };
        Value::TimestampTz(PgTimestampTz::Finite(instant))
    } else {
        Value::Timestamp(PgTimestamp::Finite(result))
    })
}

pub(super) fn format_timestamp(value: &Value, format: &str, zone: &str) -> Result<Value> {
    let (timestamp, offset) = match value {
        Value::Timestamp(PgTimestamp::Finite(value)) => (*value, 0),
        Value::TimestampTz(PgTimestampTz::Finite(value)) => convert_utc(parse_zone(zone)?, *value)?,
        _ => return Ok(Value::Null),
    };
    if format.is_empty() {
        return Ok(Value::Null);
    }
    let mut output = String::new();
    let mut rest = format;
    while !rest.is_empty() {
        if rest.starts_with("\\\"") {
            output.push('"');
            rest = &rest[2..];
            continue;
        }
        if rest.starts_with('"') {
            rest = &rest[1..];
            while let Some(character) = rest.chars().next() {
                rest = &rest[character.len_utf8()..];
                if character == '"' {
                    break;
                }
                if character == '\\'
                    && let Some(escaped) = rest.chars().next()
                {
                    output.push(escaped);
                    rest = &rest[escaped.len_utf8()..];
                } else {
                    output.push(character);
                }
            }
            continue;
        }
        let token = [
            "HH24", "YYYY", "TZH", "TZM", "MM", "DD", "MI", "SS", "MS", "US", "OF",
        ]
        .into_iter()
        .find(|token| rest.starts_with(token));
        if let Some(token) = token {
            let text = match token {
                "YYYY" => format!(
                    "{:04}",
                    if timestamp.year() <= 0 {
                        1 - timestamp.year()
                    } else {
                        timestamp.year()
                    }
                ),
                "MM" => format!("{:02}", timestamp.month()),
                "DD" => format!("{:02}", timestamp.day()),
                "HH24" => format!("{:02}", timestamp.hour()),
                "MI" => format!("{:02}", timestamp.minute()),
                "SS" => format!("{:02}", timestamp.second()),
                "MS" => format!("{:03}", timestamp.nanosecond() / 1_000_000),
                "US" => format!("{:06}", timestamp.nanosecond() / 1000),
                "TZH" => format!(
                    "{}{:02}",
                    if offset < 0 { '-' } else { '+' },
                    offset.abs() / 3600
                ),
                "TZM" => format!("{:02}", offset.abs() / 60 % 60),
                "OF" => {
                    let hours = format!(
                        "{}{:02}",
                        if offset < 0 { '-' } else { '+' },
                        offset.abs() / 3600
                    );
                    if offset.abs() / 60 % 60 == 0 {
                        hours
                    } else {
                        format!("{hours}:{:02}", offset.abs() / 60 % 60)
                    }
                }
                _ => unreachable!(),
            };
            output.push_str(&text);
            rest = &rest[token.len()..];
        } else {
            let character = rest.chars().next().unwrap();
            if character.is_alphabetic() {
                return reject_unsupported(
                    "timestamp format token is not implemented; quote literal text",
                );
            }
            output.push(character);
            rest = &rest[character.len_utf8()..];
        }
    }
    Ok(Value::Text(output))
}

fn create_overflow() -> PgError {
    PgError::create(SqlState::DatetimeFieldOverflow, "timestamp out of range")
}
