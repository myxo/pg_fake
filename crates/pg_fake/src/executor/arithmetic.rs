use super::*;
use bigdecimal::{BigDecimal, Signed};

pub(super) fn unary(operator: UnaryOperator, value: Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    match (operator, value) {
        (UnaryOperator::Plus, value @ (Value::Int2(_) | Value::Int4(_) | Value::Int8(_))) => {
            Ok(value)
        }
        (
            UnaryOperator::Plus,
            value @ (Value::Float4(_) | Value::Float8(_) | Value::Numeric(_)),
        ) => Ok(value),
        (UnaryOperator::Minus, Value::Int2(value)) => value
            .checked_neg()
            .map(Value::Int2)
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "smallint out of range")),
        (UnaryOperator::Minus, Value::Int4(value)) => value
            .checked_neg()
            .map(Value::Int4)
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "integer out of range")),
        (UnaryOperator::Minus, Value::Int8(value)) => value
            .checked_neg()
            .map(Value::Int8)
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "bigint out of range")),
        (UnaryOperator::Minus, Value::Float4(value)) => Ok(Value::Float4(-value)),
        (UnaryOperator::Minus, Value::Float8(value)) => Ok(Value::Float8(-value)),
        (UnaryOperator::Minus, Value::Numeric(value)) => Ok(Value::Numeric(-value)),
        (UnaryOperator::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible type",
        )),
    }
}

pub(super) fn boolean_binary(
    operator: &BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value> {
    match (operator, left, right) {
        (BinaryOperator::And, Value::Bool(false), _)
        | (BinaryOperator::And, _, Value::Bool(false)) => Ok(Value::Bool(false)),
        (BinaryOperator::And, Value::Bool(true), value)
        | (BinaryOperator::And, value, Value::Bool(true)) => Ok(value),
        (BinaryOperator::And, Value::Null, Value::Null) => Ok(Value::Null),
        (BinaryOperator::Or, Value::Bool(true), _) | (BinaryOperator::Or, _, Value::Bool(true)) => {
            Ok(Value::Bool(true))
        }
        (BinaryOperator::Or, Value::Bool(false), value)
        | (BinaryOperator::Or, value, Value::Bool(false)) => Ok(value),
        (BinaryOperator::Or, Value::Null, Value::Null) => Ok(Value::Null),
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

pub(super) fn distinct(left: Value, right: Value, equal: bool) -> Result<Value> {
    match (&left, &right) {
        (Value::Null, Value::Null) => Ok(Value::Bool(equal)),
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Bool(!equal)),
        _ => match comparison(&BinaryOperator::Eq, &left, &right)? {
            Value::Bool(value) => Ok(Value::Bool(value == equal)),
            _ => unreachable!("comparison always returns a boolean"),
        },
    }
}

pub(super) fn arithmetic(operator: &BinaryOperator, left: Value, right: Value) -> Result<Value> {
    macro_rules! integer {
        ($left:expr, $right:expr, $variant:ident, $name:literal) => {{
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && $right == 0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            let value = match operator {
                BinaryOperator::Plus => $left.checked_add($right),
                BinaryOperator::Minus => $left.checked_sub($right),
                BinaryOperator::Multiply => $left.checked_mul($right),
                BinaryOperator::Divide => $left.checked_div($right),
                BinaryOperator::Modulo => $left.checked_rem($right),
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            value.map(Value::$variant).ok_or_else(|| {
                PgError::new(
                    SqlState::NumericValueOutOfRange,
                    concat!($name, " out of range"),
                )
            })
        }};
    }

    match (left, right) {
        (Value::Int2(left), Value::Int2(right)) => integer!(left, right, Int2, "smallint"),
        (Value::Int4(left), Value::Int4(right)) => integer!(left, right, Int4, "integer"),
        (Value::Int8(left), Value::Int8(right)) => integer!(left, right, Int8, "bigint"),
        (Value::Float4(left), Value::Float4(right)) => {
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && right == 0.0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            let value = match operator {
                BinaryOperator::Plus => left + right,
                BinaryOperator::Minus => left - right,
                BinaryOperator::Multiply => left * right,
                BinaryOperator::Divide => left / right,
                BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            if value.is_infinite() && left.is_finite() && right.is_finite() {
                Err(PgError::new(
                    SqlState::NumericValueOutOfRange,
                    "real out of range",
                ))
            } else {
                Ok(Value::Float4(value))
            }
        }
        (Value::Float8(left), Value::Float8(right)) => {
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && right == 0.0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            let value = match operator {
                BinaryOperator::Plus => left + right,
                BinaryOperator::Minus => left - right,
                BinaryOperator::Multiply => left * right,
                BinaryOperator::Divide => left / right,
                BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            if value.is_infinite() && left.is_finite() && right.is_finite() {
                Err(PgError::new(
                    SqlState::NumericValueOutOfRange,
                    "double precision out of range",
                ))
            } else {
                Ok(Value::Float8(value))
            }
        }
        (Value::Numeric(left), Value::Numeric(right)) => {
            if matches!(operator, BinaryOperator::Divide | BinaryOperator::Modulo) && right == 0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            Ok(Value::Numeric(match operator {
                BinaryOperator::Plus => left + right,
                BinaryOperator::Minus => left - right,
                BinaryOperator::Multiply => left * right,
                BinaryOperator::Divide => divide_numeric(&left, &right),
                BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            }))
        }
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

// BigDecimal division chooses its own fixed precision. PostgreSQL instead derives
// NUMERIC division scale from normalized base-10000 weights, keeps at least 16
// significant decimal digits and both input scales, clamps it to 0..=1000, and
// rounds ties away from zero.
fn divide_numeric(left: &BigDecimal, right: &BigDecimal) -> BigDecimal {
    let (left_weight, left_first_digit) = describe_numeric_division_operand(left);
    let (right_weight, right_first_digit) = describe_numeric_division_operand(right);
    let mut quotient_weight = left_weight - right_weight;
    if left_first_digit <= right_first_digit {
        quotient_weight -= 1;
    }
    let result_scale = (16 - quotient_weight * 4)
        .max(left.fractional_digit_count())
        .max(right.fractional_digit_count())
        .clamp(0, 1000);

    let (left_integer, left_scale) = left.as_bigint_and_exponent();
    let (right_integer, right_scale) = right.as_bigint_and_exponent();
    let negative = left_integer.sign() != right_integer.sign();
    let mut numerator = left_integer.abs();
    let mut denominator = right_integer.abs();
    let exponent = right_scale + result_scale - left_scale;
    let power = |exponent: i64| {
        bigdecimal::num_bigint::BigInt::from(10_u8)
            .pow(u32::try_from(exponent).expect("numeric scale difference must fit u32"))
    };
    if exponent >= 0 {
        numerator *= power(exponent);
    } else {
        denominator *= power(-exponent);
    }
    let mut quotient = &numerator / &denominator;
    let remainder = numerator % &denominator;
    if remainder * 2 >= denominator {
        quotient += 1;
    }
    if negative {
        quotient = -quotient;
    }
    BigDecimal::new(quotient, result_scale)
}

fn describe_numeric_division_operand(value: &BigDecimal) -> (i64, u16) {
    if value == &BigDecimal::from(0) {
        return (0, 0);
    }
    let plain = value.normalized().abs().to_plain_string();
    let (integer, fraction) = plain.split_once('.').unwrap_or((&plain, ""));
    let integer = integer.trim_start_matches('0');
    if !integer.is_empty() {
        let weight = i64::try_from(integer.len() - 1).expect("numeric length fits i64") / 4;
        let first_length = integer.len() - usize::try_from(weight * 4).expect("weight is positive");
        return (
            weight,
            integer[..first_length]
                .parse()
                .expect("numeric digits are a base-10000 digit"),
        );
    }

    let first_nonzero = fraction
        .find(|character| character != '0')
        .expect("nonzero numeric has a nonzero digit");
    let group = first_nonzero / 4;
    let start = group * 4;
    let end = (start + 4).min(fraction.len());
    let mut first_digit = fraction[start..end].to_owned();
    first_digit.extend(std::iter::repeat_n('0', 4 - first_digit.len()));
    (
        -i64::try_from(group).expect("numeric group fits i64") - 1,
        first_digit
            .parse()
            .expect("numeric digits are a base-10000 digit"),
    )
}

pub(super) fn interval_arithmetic_type(
    operator: &BinaryOperator,
    left: BaseType,
    right: BaseType,
) -> Result<BaseType> {
    let numeric = |value| {
        matches!(
            value,
            BaseType::Int2
                | BaseType::Int4
                | BaseType::Int8
                | BaseType::Float4
                | BaseType::Float8
                | BaseType::Numeric
        )
    };
    match (operator, left, right) {
        (BinaryOperator::Plus | BinaryOperator::Minus, BaseType::Interval, BaseType::Interval) => {
            Ok(BaseType::Interval)
        }
        (
            BinaryOperator::Plus,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
            BaseType::Interval,
        )
        | (
            BinaryOperator::Plus,
            BaseType::Interval,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
        )
        | (
            BinaryOperator::Minus,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
            BaseType::Interval,
        ) => Ok(if left == BaseType::Interval {
            right
        } else {
            left
        }),
        (BinaryOperator::Multiply, BaseType::Interval, right) if numeric(right) => {
            Ok(BaseType::Interval)
        }
        (BinaryOperator::Multiply, left, BaseType::Interval) if numeric(left) => {
            Ok(BaseType::Interval)
        }
        (BinaryOperator::Divide, BaseType::Interval, right) if numeric(right) => {
            Ok(BaseType::Interval)
        }
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

pub(super) fn temporal_arithmetic(
    operator: &BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value> {
    use chrono::{Days, Months, TimeDelta};
    fn interval_scale(
        value: crate::value::PgInterval,
        factor: f64,
    ) -> Result<crate::value::PgInterval> {
        if !factor.is_finite() {
            return Err(PgError::new(
                SqlState::NumericValueOutOfRange,
                "interval out of range",
            ));
        }
        Ok(crate::value::PgInterval {
            months: (f64::from(value.months) * factor).round() as i32,
            days: (f64::from(value.days) * factor).round() as i32,
            micros: (value.micros as f64 * factor).round() as i64,
        })
    }
    fn signed_interval(
        mut interval: crate::value::PgInterval,
        negative: bool,
    ) -> crate::value::PgInterval {
        if negative {
            interval.months = -interval.months;
            interval.days = -interval.days;
            interval.micros = -interval.micros;
        }
        interval
    }
    fn add_naive(
        mut value: chrono::NaiveDateTime,
        interval: crate::value::PgInterval,
    ) -> Result<chrono::NaiveDateTime> {
        if interval.months != 0 {
            value = if interval.months > 0 {
                value.checked_add_months(Months::new(interval.months as u32))
            } else {
                value.checked_sub_months(Months::new(interval.months.unsigned_abs()))
            }
            .ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })?;
        }
        if interval.days != 0 {
            value = if interval.days > 0 {
                value.checked_add_days(Days::new(interval.days as u64))
            } else {
                value.checked_sub_days(Days::new(interval.days.unsigned_abs() as u64))
            }
            .ok_or_else(|| {
                PgError::new(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })?;
        }
        value
            .checked_add_signed(TimeDelta::microseconds(interval.micros))
            .ok_or_else(|| PgError::new(SqlState::NumericValueOutOfRange, "timestamp out of range"))
    }
    match (operator, left, right) {
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::Interval(left),
            Value::Interval(right),
        ) => {
            let right = signed_interval(right, matches!(operator, BinaryOperator::Minus));
            Ok(Value::Interval(crate::value::PgInterval {
                months: left.months.checked_add(right.months).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                days: left.days.checked_add(right.days).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                micros: left.micros.checked_add(right.micros).ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
            }))
        }
        (BinaryOperator::Multiply | BinaryOperator::Divide, Value::Interval(value), number) => {
            let factor = match number {
                Value::Int2(v) => f64::from(v),
                Value::Int4(v) => f64::from(v),
                Value::Int8(v) => v as f64,
                Value::Float4(v) => f64::from(v),
                Value::Float8(v) => v,
                Value::Numeric(v) => v.to_f64().ok_or_else(|| {
                    PgError::new(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                _ => {
                    return Err(PgError::new(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ));
                }
            };
            if matches!(operator, BinaryOperator::Divide) && factor == 0.0 {
                return Err(PgError::new(SqlState::DivisionByZero, "division by zero"));
            }
            interval_scale(
                value,
                if matches!(operator, BinaryOperator::Divide) {
                    1.0 / factor
                } else {
                    factor
                },
            )
            .map(Value::Interval)
        }
        (BinaryOperator::Multiply, number, Value::Interval(value)) => {
            temporal_arithmetic(operator, Value::Interval(value), number)
        }
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::Date(crate::value::PgDate::Finite(date)),
            Value::Interval(interval),
        ) => {
            let value = add_naive(
                date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
                signed_interval(interval, matches!(operator, BinaryOperator::Minus)),
            )?;
            Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(value)))
        }
        (BinaryOperator::Plus, Value::Interval(interval), value @ Value::Date(_)) => {
            temporal_arithmetic(operator, value, Value::Interval(interval))
        }
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::Timestamp(crate::value::PgTimestamp::Finite(value)),
            Value::Interval(interval),
        ) => Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(
            add_naive(
                value,
                signed_interval(interval, matches!(operator, BinaryOperator::Minus)),
            )?,
        ))),
        (
            BinaryOperator::Plus | BinaryOperator::Minus,
            Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)),
            Value::Interval(interval),
        ) => Ok(Value::TimestampTz(crate::value::PgTimestampTz::Finite(
            add_naive(
                value.naive_utc(),
                signed_interval(interval, matches!(operator, BinaryOperator::Minus)),
            )?
            .and_utc(),
        ))),
        (BinaryOperator::Plus, Value::Interval(interval), value @ Value::Timestamp(_))
        | (BinaryOperator::Plus, Value::Interval(interval), value @ Value::TimestampTz(_)) => {
            temporal_arithmetic(operator, value, Value::Interval(interval))
        }
        _ => Err(PgError::new(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}
