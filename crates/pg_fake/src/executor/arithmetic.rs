use super::*;
use bigdecimal::{BigDecimal, Signed};
use sqlparser::ast;

pub(super) fn evaluate_unary_operator(operator: ast::UnaryOperator, value: Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    match (operator, value) {
        (ast::UnaryOperator::Plus, value @ (Value::Int2(_) | Value::Int4(_) | Value::Int8(_))) => {
            Ok(value)
        }
        (
            ast::UnaryOperator::Plus,
            value @ (Value::Float4(_) | Value::Float8(_) | Value::Numeric(_)),
        ) => Ok(value),
        (ast::UnaryOperator::Minus, Value::Int2(value)) => {
            value.checked_neg().map(Value::Int2).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "smallint out of range")
            })
        }
        (ast::UnaryOperator::Minus, Value::Int4(value)) => {
            value.checked_neg().map(Value::Int4).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "integer out of range")
            })
        }
        (ast::UnaryOperator::Minus, Value::Int8(value)) => {
            value.checked_neg().map(Value::Int8).ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "bigint out of range")
            })
        }
        (ast::UnaryOperator::Minus, Value::Float4(value)) => Ok(Value::Float4(-value)),
        (ast::UnaryOperator::Minus, Value::Float8(value)) => Ok(Value::Float8(-value)),
        (ast::UnaryOperator::Minus, Value::Numeric(value)) => Ok(Value::Numeric(-value)),
        (ast::UnaryOperator::Not, Value::Bool(value)) => Ok(Value::Bool(!value)),
        _ => Err(PgError::create(
            SqlState::DatatypeMismatch,
            "operator has incompatible type",
        )),
    }
}

pub(super) fn evaluate_boolean_operator(
    operator: &ast::BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value> {
    match (operator, left, right) {
        (ast::BinaryOperator::And, Value::Bool(false), _)
        | (ast::BinaryOperator::And, _, Value::Bool(false)) => Ok(Value::Bool(false)),
        (ast::BinaryOperator::And, Value::Bool(true), value)
        | (ast::BinaryOperator::And, value, Value::Bool(true)) => Ok(value),
        (ast::BinaryOperator::And, Value::Null, Value::Null) => Ok(Value::Null),
        (ast::BinaryOperator::Or, Value::Bool(true), _)
        | (ast::BinaryOperator::Or, _, Value::Bool(true)) => Ok(Value::Bool(true)),
        (ast::BinaryOperator::Or, Value::Bool(false), value)
        | (ast::BinaryOperator::Or, value, Value::Bool(false)) => Ok(value),
        (ast::BinaryOperator::Or, Value::Null, Value::Null) => Ok(Value::Null),
        _ => Err(PgError::create(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

pub(super) fn evaluate_distinctness(left: Value, right: Value, equal: bool) -> Result<Value> {
    match (&left, &right) {
        (Value::Null, Value::Null) => Ok(Value::Bool(equal)),
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Bool(!equal)),
        _ => match evaluate_comparison(&ast::BinaryOperator::Eq, &left, &right)? {
            Value::Bool(value) => Ok(Value::Bool(value == equal)),
            _ => unreachable!("evaluate_comparison always returns a boolean"),
        },
    }
}

pub(super) fn evaluate_numeric_operator(
    operator: &ast::BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value> {
    macro_rules! integer {
        ($left:expr, $right:expr, $variant:ident, $name:literal) => {{
            if matches!(
                operator,
                ast::BinaryOperator::Divide | ast::BinaryOperator::Modulo
            ) && $right == 0
            {
                return Err(PgError::create(
                    SqlState::DivisionByZero,
                    "division by zero",
                ));
            }
            let value = match operator {
                ast::BinaryOperator::Plus => $left.checked_add($right),
                ast::BinaryOperator::Minus => $left.checked_sub($right),
                ast::BinaryOperator::Multiply => $left.checked_mul($right),
                ast::BinaryOperator::Divide => $left.checked_div($right),
                ast::BinaryOperator::Modulo => $left.checked_rem($right),
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            value.map(Value::$variant).ok_or_else(|| {
                PgError::create(
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
            if matches!(
                operator,
                ast::BinaryOperator::Divide | ast::BinaryOperator::Modulo
            ) && right == 0.0
            {
                return Err(PgError::create(
                    SqlState::DivisionByZero,
                    "division by zero",
                ));
            }
            let value = match operator {
                ast::BinaryOperator::Plus => left + right,
                ast::BinaryOperator::Minus => left - right,
                ast::BinaryOperator::Multiply => left * right,
                ast::BinaryOperator::Divide => left / right,
                ast::BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            if value.is_infinite() && left.is_finite() && right.is_finite() {
                Err(PgError::create(
                    SqlState::NumericValueOutOfRange,
                    "real out of range",
                ))
            } else {
                Ok(Value::Float4(value))
            }
        }
        (Value::Float8(left), Value::Float8(right)) => {
            if matches!(
                operator,
                ast::BinaryOperator::Divide | ast::BinaryOperator::Modulo
            ) && right == 0.0
            {
                return Err(PgError::create(
                    SqlState::DivisionByZero,
                    "division by zero",
                ));
            }
            let value = match operator {
                ast::BinaryOperator::Plus => left + right,
                ast::BinaryOperator::Minus => left - right,
                ast::BinaryOperator::Multiply => left * right,
                ast::BinaryOperator::Divide => left / right,
                ast::BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            };
            if value.is_infinite() && left.is_finite() && right.is_finite() {
                Err(PgError::create(
                    SqlState::NumericValueOutOfRange,
                    "double precision out of range",
                ))
            } else {
                Ok(Value::Float8(value))
            }
        }
        (Value::Numeric(left), Value::Numeric(right)) => {
            if matches!(
                operator,
                ast::BinaryOperator::Divide | ast::BinaryOperator::Modulo
            ) && right == 0
            {
                return Err(PgError::create(
                    SqlState::DivisionByZero,
                    "division by zero",
                ));
            }
            Ok(Value::Numeric(match operator {
                ast::BinaryOperator::Plus => left + right,
                ast::BinaryOperator::Minus => left - right,
                ast::BinaryOperator::Multiply => left * right,
                ast::BinaryOperator::Divide => divide_numeric(&left, &right),
                ast::BinaryOperator::Modulo => left % right,
                _ => unreachable!("arithmetic operator was checked by caller"),
            }))
        }
        _ => Err(PgError::create(
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

pub(super) fn infer_interval_arithmetic_type(
    operator: &ast::BinaryOperator,
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
        (
            ast::BinaryOperator::Plus | ast::BinaryOperator::Minus,
            BaseType::Interval,
            BaseType::Interval,
        ) => Ok(BaseType::Interval),
        (
            ast::BinaryOperator::Plus,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
            BaseType::Interval,
        )
        | (
            ast::BinaryOperator::Plus,
            BaseType::Interval,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
        )
        | (
            ast::BinaryOperator::Minus,
            BaseType::Date | BaseType::Timestamp | BaseType::TimestampTz,
            BaseType::Interval,
        ) => Ok(if left == BaseType::Interval {
            right
        } else {
            left
        }),
        (ast::BinaryOperator::Multiply, BaseType::Interval, right) if numeric(right) => {
            Ok(BaseType::Interval)
        }
        (ast::BinaryOperator::Multiply, left, BaseType::Interval) if numeric(left) => {
            Ok(BaseType::Interval)
        }
        (ast::BinaryOperator::Divide, BaseType::Interval, right) if numeric(right) => {
            Ok(BaseType::Interval)
        }
        _ => Err(PgError::create(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}

pub(super) fn evaluate_temporal_arithmetic(
    operator: &ast::BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value> {
    use chrono::{Days, Months, TimeDelta};
    fn scale_interval(
        value: crate::value::PgInterval,
        factor: f64,
    ) -> Result<crate::value::PgInterval> {
        if !factor.is_finite() {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                "interval out of range",
            ));
        }
        let scaled_months = f64::from(value.months) * factor;
        let months = scaled_months.trunc();
        let scaled_days =
            f64::from(value.days) * factor + (scaled_months - months) * f64::from(DAYS_PER_MONTH);
        let days = scaled_days.trunc();
        let scaled_micros =
            value.micros as f64 * factor + (scaled_days - days) * MICROSECONDS_PER_DAY as f64;
        if months < f64::from(i32::MIN)
            || months > f64::from(i32::MAX)
            || days < f64::from(i32::MIN)
            || days > f64::from(i32::MAX)
            || scaled_micros < i64::MIN as f64
            || scaled_micros > i64::MAX as f64
        {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                "interval out of range",
            ));
        }
        Ok(crate::value::PgInterval {
            months: months as i32,
            days: days as i32,
            micros: scaled_micros.round() as i64,
        })
    }
    fn negate_interval_if(
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
    fn add_interval_to_timestamp(
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
                PgError::create(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })?;
        }
        if interval.days != 0 {
            value = if interval.days > 0 {
                value.checked_add_days(Days::new(interval.days as u64))
            } else {
                value.checked_sub_days(Days::new(interval.days.unsigned_abs() as u64))
            }
            .ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })?;
        }
        value
            .checked_add_signed(TimeDelta::microseconds(interval.micros))
            .ok_or_else(|| {
                PgError::create(SqlState::NumericValueOutOfRange, "timestamp out of range")
            })
    }
    match (operator, left, right) {
        (
            ast::BinaryOperator::Plus | ast::BinaryOperator::Minus,
            Value::Interval(left),
            Value::Interval(right),
        ) => {
            let right = negate_interval_if(right, matches!(operator, ast::BinaryOperator::Minus));
            Ok(Value::Interval(crate::value::PgInterval {
                months: left.months.checked_add(right.months).ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                days: left.days.checked_add(right.days).ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                micros: left.micros.checked_add(right.micros).ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
            }))
        }
        (
            ast::BinaryOperator::Multiply | ast::BinaryOperator::Divide,
            Value::Interval(value),
            number,
        ) => {
            let factor = match number {
                Value::Int2(v) => f64::from(v),
                Value::Int4(v) => f64::from(v),
                Value::Int8(v) => v as f64,
                Value::Float4(v) => f64::from(v),
                Value::Float8(v) => v,
                Value::Numeric(v) => v.to_f64().ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "interval out of range")
                })?,
                _ => {
                    return Err(PgError::create(
                        SqlState::DatatypeMismatch,
                        "operator has incompatible types",
                    ));
                }
            };
            if matches!(operator, ast::BinaryOperator::Divide) && factor == 0.0 {
                return Err(PgError::create(
                    SqlState::DivisionByZero,
                    "division by zero",
                ));
            }
            scale_interval(
                value,
                if matches!(operator, ast::BinaryOperator::Divide) {
                    1.0 / factor
                } else {
                    factor
                },
            )
            .map(Value::Interval)
        }
        (ast::BinaryOperator::Multiply, number, Value::Interval(value)) => {
            evaluate_temporal_arithmetic(operator, Value::Interval(value), number)
        }
        (
            ast::BinaryOperator::Plus | ast::BinaryOperator::Minus,
            Value::Date(crate::value::PgDate::Finite(date)),
            Value::Interval(interval),
        ) => {
            let value = add_interval_to_timestamp(
                date.and_hms_opt(0, 0, 0).expect("midnight is valid"),
                negate_interval_if(interval, matches!(operator, ast::BinaryOperator::Minus)),
            )?;
            Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(value)))
        }
        (ast::BinaryOperator::Plus, Value::Interval(interval), value @ Value::Date(_)) => {
            evaluate_temporal_arithmetic(operator, value, Value::Interval(interval))
        }
        (
            ast::BinaryOperator::Plus | ast::BinaryOperator::Minus,
            Value::Timestamp(crate::value::PgTimestamp::Finite(value)),
            Value::Interval(interval),
        ) => Ok(Value::Timestamp(crate::value::PgTimestamp::Finite(
            add_interval_to_timestamp(
                value,
                negate_interval_if(interval, matches!(operator, ast::BinaryOperator::Minus)),
            )?,
        ))),
        (
            ast::BinaryOperator::Plus | ast::BinaryOperator::Minus,
            Value::TimestampTz(crate::value::PgTimestampTz::Finite(value)),
            Value::Interval(interval),
        ) => Ok(Value::TimestampTz(crate::value::PgTimestampTz::Finite(
            add_interval_to_timestamp(
                value.naive_utc(),
                negate_interval_if(interval, matches!(operator, ast::BinaryOperator::Minus)),
            )?
            .and_utc(),
        ))),
        (ast::BinaryOperator::Plus, Value::Interval(interval), value @ Value::Timestamp(_))
        | (ast::BinaryOperator::Plus, Value::Interval(interval), value @ Value::TimestampTz(_)) => {
            evaluate_temporal_arithmetic(operator, value, Value::Interval(interval))
        }
        _ => Err(PgError::create(
            SqlState::DatatypeMismatch,
            "operator has incompatible types",
        )),
    }
}
