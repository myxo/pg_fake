use std::time::Duration;

use sqlparser::ast;

use crate::{
    error::{PgError, Result, SqlState},
    value::Value,
};

use super::{
    ColumnMeta, IsolationLevel, QueryResult, Session, SessionTransactionState, StatementResult,
};

#[derive(Clone)]
pub(super) struct SessionSettings {
    pub(super) default_isolation: IsolationLevel,
    pub(super) lock_timeout: Duration,
    pub(super) statement_timeout: Duration,
    pub(super) timezone: String,
}

impl Session {
    pub(super) fn try_execute_setting(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<Option<StatementResult>> {
        match statement {
            ast::Statement::Reset(reset)
                if !self.db.strict && is_tolerated_planner_reset(&reset.reset) =>
            {
                return Ok(Some(StatementResult::Affected(0)));
            }
            ast::Statement::Set(ast::Set::SetTimeZone { local, value }) => {
                self.timezone = parse_timezone(value)?;
                if !local {
                    self.settings_on_commit
                        .as_mut()
                        .expect("SET runs in a transaction")
                        .timezone = self.timezone.clone();
                }
                return Ok(Some(StatementResult::Affected(0)));
            }
            ast::Statement::ShowVariable { variable }
                if variable.len() == 1 && variable[0].value.eq_ignore_ascii_case("timezone") =>
            {
                return Ok(Some(StatementResult::Query(QueryResult {
                    columns: vec![ColumnMeta {
                        name: "TimeZone".into(),
                        type_oid: crate::value::BaseType::Text.map_to_oid(),
                        typmod: -1,
                    }],
                    rows: vec![vec![Value::Text(self.timezone.clone())]],
                })));
            }
            ast::Statement::Set(ast::Set::SingleAssignment {
                scope,
                hivevar,
                variable,
                values,
            }) => {
                if variable.to_string().eq_ignore_ascii_case("search_path") {
                    return self.abort_with_error(PgError::create(
                        SqlState::FeatureNotSupported,
                        "changing search_path is not implemented",
                    ));
                }
                if variable.to_string().eq_ignore_ascii_case("timezone") {
                    if *hivevar || values.len() != 1 {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "TimeZone setting variant is not implemented",
                        ));
                    }
                    self.timezone = parse_timezone(&values[0])?;
                    if *scope != Some(ast::ContextModifier::Local) {
                        self.settings_on_commit
                            .as_mut()
                            .expect("SET runs in a transaction")
                            .timezone = self.timezone.clone();
                    }
                    return Ok(Some(StatementResult::Affected(0)));
                }
                if variable.to_string().eq_ignore_ascii_case("lock_timeout") {
                    if matches!(
                        self.transaction,
                        Some(SessionTransactionState::Aborted { .. })
                    ) {
                        return Err(PgError::create(
                            SqlState::InFailedSqlTransaction,
                            "current transaction is aborted",
                        ));
                    }
                    if *hivevar || values.len() != 1 {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "lock_timeout setting variant is not implemented",
                        ));
                    }
                    self.lock_timeout = match parse_timeout(&values[0], "lock_timeout") {
                        Ok(timeout) => timeout,
                        Err(error) => return self.abort_with_error(error),
                    };
                    if *scope != Some(ast::ContextModifier::Local) {
                        self.settings_on_commit
                            .as_mut()
                            .expect("SET runs in a transaction")
                            .lock_timeout = self.lock_timeout;
                    }
                    return Ok(Some(StatementResult::Affected(0)));
                }
                if variable
                    .to_string()
                    .eq_ignore_ascii_case("statement_timeout")
                {
                    if matches!(
                        self.transaction,
                        Some(SessionTransactionState::Aborted { .. })
                    ) {
                        return Err(PgError::create(
                            SqlState::InFailedSqlTransaction,
                            "current transaction is aborted",
                        ));
                    }
                    if *scope != Some(ast::ContextModifier::Local) || *hivevar || values.len() != 1
                    {
                        return self.abort_with_error(PgError::create(
                            SqlState::FeatureNotSupported,
                            "statement_timeout setting variant is not implemented",
                        ));
                    }
                    self.statement_timeout = match parse_timeout(&values[0], "statement_timeout") {
                        Ok(timeout) => timeout,
                        Err(error) => return self.abort_with_error(error),
                    };
                    return Ok(Some(StatementResult::Affected(0)));
                }
                if !self.db.strict && is_tolerated_planner_setting(variable) {
                    return Ok(Some(StatementResult::Affected(0)));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    pub(super) fn capture_settings(&self) -> SessionSettings {
        SessionSettings {
            default_isolation: self.default_isolation,
            lock_timeout: self.lock_timeout,
            statement_timeout: self.statement_timeout,
            timezone: self.timezone.clone(),
        }
    }

    pub(super) fn restore_settings(&mut self, settings: SessionSettings) {
        self.default_isolation = settings.default_isolation;
        self.lock_timeout = settings.lock_timeout;
        self.statement_timeout = settings.statement_timeout;
        self.timezone = settings.timezone;
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_invalid_timeout_error(parameter: &str) -> PgError {
    PgError::create(
        SqlState::InvalidParameterValue,
        format!("invalid value for parameter {parameter}"),
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_timeout(expression: &ast::Expr, parameter: &str) -> Result<Duration> {
    let text = match expression {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Number(value, _) => value.as_str(),
            ast::Value::SingleQuotedString(value) => value.trim(),
            _ => return Err(create_invalid_timeout_error(parameter)),
        },
        _ => return Err(create_invalid_timeout_error(parameter)),
    };
    let text = text.trim();
    let unsigned = text
        .strip_prefix('-')
        .or_else(|| text.strip_prefix('+'))
        .unwrap_or(text);
    let hexadecimal = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"));
    let (value, unit) = if let Some(hexadecimal) = hexadecimal {
        let digit_count = hexadecimal
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count();
        let value_end = text.len() - hexadecimal.len() + digit_count;
        let unit = text[value_end..].trim();
        (&text[..value_end], (!unit.is_empty()).then_some(unit))
    } else if let Some(value) = text.strip_suffix("min") {
        (value, Some("min"))
    } else if let Some(value) = text.strip_suffix("ms") {
        (value, Some("ms"))
    } else if let Some(value) = text.strip_suffix("us") {
        (value, Some("us"))
    } else if let Some(value) = text.strip_suffix('s') {
        (value, Some("s"))
    } else if let Some(value) = text.strip_suffix('h') {
        (value, Some("h"))
    } else if let Some(value) = text.strip_suffix('d') {
        (value, Some("d"))
    } else {
        (text, None)
    };
    let (multiplier, next_smaller_multiplier) = match unit {
        Some("d") => (86_400_000.0, Some(3_600_000.0)),
        Some("h") => (3_600_000.0, Some(60_000.0)),
        Some("min") => (60_000.0, Some(1_000.0)),
        Some("s") => (1_000.0, Some(1.0)),
        Some("ms") => (1.0, Some(0.001)),
        Some("us") => (0.001, None),
        Some(_) => return Err(create_invalid_timeout_error(parameter)),
        None => (1.0, None),
    };
    let value = value.trim();
    let (negative, digits) = if let Some(digits) = value.strip_prefix('-') {
        (true, digits)
    } else if let Some(digits) = value.strip_prefix('+') {
        (false, digits)
    } else {
        (false, value)
    };
    let (digits, radix) = if let Some(digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (digits, 16)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (digits, 8)
    } else {
        (digits, 10)
    };
    let digit_count = digits
        .bytes()
        .take_while(|digit| char::from(*digit).to_digit(radix).is_some())
        .count();
    let retry_as_float = radix != 16
        && digits
            .as_bytes()
            .get(digit_count)
            .is_some_and(|digit| matches!(digit, b'.' | b'e' | b'E'));
    let integer = (!retry_as_float && digit_count == digits.len() && digit_count != 0)
        .then(|| u64::from_str_radix(&digits[..digit_count], radix).ok())
        .flatten()
        .map(|value| {
            if negative {
                -(value as f64)
            } else {
                value as f64
            }
        });
    let value = integer.or_else(|| retry_as_float.then(|| value.parse::<f64>().ok()).flatten());
    let milliseconds = value
        .filter(|value| value.is_finite())
        .map(|value| value * multiplier)
        .map(|value| {
            next_smaller_multiplier
                .map(|next| (value / next).round_ties_even() * next)
                .unwrap_or(value)
                .round_ties_even()
        })
        .filter(|milliseconds| *milliseconds >= 0.0 && *milliseconds <= i32::MAX as f64)
        .ok_or_else(|| create_invalid_timeout_error(parameter))?;
    Ok(Duration::from_millis(milliseconds as u64))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_timezone(expression: &ast::Expr) -> Result<String> {
    let value = match expression {
        ast::Expr::Value(value) => {
            let ast::Value::SingleQuotedString(value) = &value.value else {
                return Err(PgError::create(
                    SqlState::InvalidParameterValue,
                    "invalid value for parameter TimeZone",
                ));
            };
            value
        }
        ast::Expr::Identifier(ast::Ident { value, .. }) => value,
        _ => {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "invalid value for parameter TimeZone",
            ));
        }
    };
    // UTC and numeric offsets are accepted here. Named-zone interpretation is
    // intentionally validated by the timestamp input layer when it is used.
    if value.eq_ignore_ascii_case("utc") || value.parse::<chrono::FixedOffset>().is_ok() {
        Ok(value.to_string())
    } else {
        Err(PgError::create(
            SqlState::InvalidParameterValue,
            "invalid value for parameter TimeZone",
        ))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_tolerated_planner_setting(variable: &ast::ObjectName) -> bool {
    let variable = variable.to_string().to_ascii_lowercase();
    matches!(
        variable.as_str(),
        "work_mem"
            | "effective_cache_size"
            | "random_page_cost"
            | "seq_page_cost"
            | "cpu_tuple_cost"
            | "cpu_index_tuple_cost"
            | "cpu_operator_cost"
            | "parallel_setup_cost"
            | "parallel_tuple_cost"
            | "min_parallel_table_scan_size"
            | "min_parallel_index_scan_size"
            | "join_collapse_limit"
            | "from_collapse_limit"
            | "plan_cache_mode"
            | "geqo"
    ) || variable.starts_with("enable_")
        || variable.starts_with("jit_")
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_tolerated_planner_reset(reset: &ast::Reset) -> bool {
    match reset {
        ast::Reset::ALL | ast::Reset::SessionAuthorization => false,
        ast::Reset::ConfigurationParameter(variable) => is_tolerated_planner_setting(variable),
    }
}
