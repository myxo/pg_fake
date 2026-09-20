use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use sqlparser::ast;

use super::{
    ColumnMeta, IsolationLevel, QueryResult, Session, SessionTransactionState, StatementResult,
};
use crate::{
    error::{PgError, Result, SqlState},
    value::Value,
};

mod registry;
use registry::{
    MemoryUnit, SettingContext, SettingDefault, SettingEffect, SettingSpec, SettingType,
    list_settings, resolve_setting,
};

#[derive(Clone)]
pub(super) struct SessionSettings {
    pub(super) default_isolation: IsolationLevel,
    pub(super) lock_timeout: Duration,
    pub(super) statement_timeout: Duration,
    pub(super) timezone: String,
    pub(super) search_path: Vec<String>,
    search_path_text: String,
    values: BTreeMap<&'static str, SettingValue>,
    custom: BTreeMap<String, String>,
}

#[derive(Clone)]
enum SettingValue {
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Text(String),
    SearchPath { names: Vec<String>, text: String },
    Isolation(IsolationLevel),
}

impl SessionSettings {
    pub(super) fn create(lock_timeout: Duration) -> Self {
        let mut settings = Self {
            default_isolation: IsolationLevel::ReadCommitted,
            lock_timeout,
            statement_timeout: Duration::ZERO,
            timezone: String::new(),
            search_path: Vec::new(),
            search_path_text: String::new(),
            values: BTreeMap::new(),
            custom: BTreeMap::new(),
        };
        for spec in list_settings() {
            let value = match spec.default {
                SettingDefault::SearchPath(values) => {
                    let names = values
                        .iter()
                        .map(|value| (*value).into())
                        .collect::<Vec<_>>();
                    SettingValue::SearchPath {
                        text: format_search_path(&names),
                        names,
                    }
                }
                SettingDefault::LockTimeout => SettingValue::Integer(
                    lock_timeout
                        .as_millis()
                        .try_into()
                        .expect("configured timeout fits milliseconds"),
                ),
                SettingDefault::Text(value) => {
                    parse_setting_value(spec, value).expect("registry defaults must be valid")
                }
                SettingDefault::Transaction => continue,
            };
            settings.assign_value(spec, value);
        }
        settings
    }

    fn assign_value(&mut self, spec: &SettingSpec, value: SettingValue) {
        match (&spec.effect, &value) {
            (SettingEffect::TimeZone, SettingValue::Text(value)) => self.timezone = value.clone(),
            (SettingEffect::LockTimeout, SettingValue::Integer(value)) => {
                self.lock_timeout = Duration::from_millis(*value as u64)
            }
            (SettingEffect::StatementTimeout, SettingValue::Integer(value)) => {
                self.statement_timeout = Duration::from_millis(*value as u64)
            }
            (SettingEffect::Isolation, SettingValue::Isolation(value)) => {
                self.default_isolation = *value
            }
            (SettingEffect::SearchPath, SettingValue::SearchPath { names, text }) => {
                self.search_path = names.clone();
                self.search_path_text = text.clone();
            }
            (SettingEffect::Compatibility | SettingEffect::Planner, _) => {
                self.values.insert(spec.name, value);
            }
            _ => unreachable!("registry type must agree with its execution effect"),
        }
    }

    fn format_value(&self, spec: &SettingSpec) -> String {
        let value = match spec.effect {
            SettingEffect::TimeZone => return self.timezone.clone(),
            SettingEffect::LockTimeout => {
                return format_units(
                    self.lock_timeout.as_millis() as i64,
                    &[
                        (86400000, "d"),
                        (3600000, "h"),
                        (60000, "min"),
                        (1000, "s"),
                        (1, "ms"),
                    ],
                );
            }
            SettingEffect::StatementTimeout => {
                return format_units(
                    self.statement_timeout.as_millis() as i64,
                    &[
                        (86400000, "d"),
                        (3600000, "h"),
                        (60000, "min"),
                        (1000, "s"),
                        (1, "ms"),
                    ],
                );
            }
            SettingEffect::Isolation => {
                return match self.default_isolation {
                    IsolationLevel::ReadCommitted => "read committed",
                    IsolationLevel::RepeatableRead => "repeatable read",
                }
                .into();
            }
            SettingEffect::TransactionIsolation => {
                unreachable!("transaction isolation is formatted by the session")
            }
            SettingEffect::SearchPath => return self.search_path_text.clone(),
            SettingEffect::Compatibility | SettingEffect::Planner => self
                .values
                .get(spec.name)
                .expect("registered setting has a value"),
        };
        match value {
            SettingValue::Boolean(value) => if *value { "on" } else { "off" }.into(),
            SettingValue::Integer(value) => {
                let multiplier = match spec.kind {
                    SettingType::Integer {
                        unit: MemoryUnit::Kilobytes,
                        ..
                    } => 1024,
                    SettingType::Integer {
                        unit: MemoryUnit::Blocks,
                        ..
                    } => 8192,
                    _ => return value.to_string(),
                };
                format_units(
                    *value * multiplier,
                    &[
                        (1099511627776, "TB"),
                        (1073741824, "GB"),
                        (1048576, "MB"),
                        (1024, "kB"),
                        (1, "B"),
                    ],
                )
            }
            SettingValue::Real(value) => value.to_string(),
            SettingValue::Text(value) => value.clone(),
            _ => unreachable!("semantic settings use their execution fields"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct GucExecutionContext {
    current: SessionSettings,
    on_commit: SessionSettings,
    defaults: SessionSettings,
    custom_settings: BTreeSet<String>,
    isolation: IsolationLevel,
    strict: bool,
}

impl GucExecutionContext {
    pub(crate) fn get_timezone(&self) -> String {
        self.current.timezone.clone()
    }

    pub(crate) fn get_lock_timeout(&self) -> Duration {
        self.current.lock_timeout
    }

    pub(crate) fn get_setting(&self, name: &str, missing_ok: bool) -> Result<Option<String>> {
        let spec = match resolve_setting(name) {
            Ok(spec) => spec,
            Err(error) => {
                let name = name.to_ascii_lowercase();
                if let Some(value) = self.current.custom.get(&name) {
                    return Ok(Some(value.clone()));
                }
                if self.custom_settings.contains(&name) {
                    return Ok(Some(String::new()));
                }
                if missing_ok {
                    return Ok(None);
                }
                return Err(error);
            }
        };
        spec.validate_access(self.strict, false)?;
        Ok(Some(match spec.effect {
            SettingEffect::TransactionIsolation => format_isolation(self.isolation),
            _ => self.current.format_value(spec),
        }))
    }

    pub(crate) fn set_setting(
        &mut self,
        name: &str,
        text: Option<&str>,
        local: bool,
    ) -> Result<String> {
        let spec = match resolve_setting(name) {
            Ok(spec) => spec,
            Err(error) => {
                if !is_custom_setting_name(name) {
                    if name.contains('.') {
                        return Err(create_invalid_custom_setting_name_error(name));
                    }
                    return Err(error);
                }
                let name = name.to_ascii_lowercase();
                self.custom_settings.insert(name.clone());
                let text = text.unwrap_or_default();
                self.current.custom.insert(name.clone(), text.into());
                if !local {
                    self.on_commit.custom.insert(name, text.into());
                }
                return Ok(text.into());
            }
        };
        if matches!(spec.effect, SettingEffect::TransactionIsolation) {
            let Some(text) = text else {
                return Err(create_cannot_reset_setting_error(spec.name));
            };
            let SettingValue::Isolation(isolation) = parse_function_setting_value(spec, text)?
            else {
                unreachable!("transaction isolation uses the isolation setting type")
            };
            if isolation != self.isolation {
                return Err(PgError::create(
                    SqlState::ActiveSqlTransaction,
                    "transaction isolation level must be set before any query",
                ));
            }
            return Ok(format_isolation(isolation));
        }
        spec.validate_access(self.strict, true)?;
        let value = match text {
            Some(text) => parse_function_setting_value(spec, text)?,
            None => self.defaults.read_value(spec),
        };
        self.current.assign_value(spec, value.clone());
        if !local {
            self.on_commit.assign_value(spec, value);
        }
        Ok(self.current.format_value(spec))
    }
}

impl Session {
    pub(super) fn describe_setting_statement(
        &self,
        statement: &ast::Statement,
    ) -> Result<Option<Vec<ColumnMeta>>> {
        let ast::Statement::ShowVariable { variable } = statement else {
            return Ok(None);
        };
        let spec = resolve_show_setting(variable)?;
        spec.validate_access(self.db.strict, false)?;
        Ok(Some(vec![ColumnMeta {
            name: spec.name.into(),
            type_oid: crate::value::BaseType::Text.map_to_oid(),
            typmod: -1,
        }]))
    }

    pub(super) fn try_execute_setting(
        &mut self,
        statement: &ast::Statement,
    ) -> Result<Option<StatementResult>> {
        let (name, values, local) = match statement {
            ast::Statement::ShowVariable { variable } => {
                let spec = resolve_show_setting(variable)?;
                let columns = self
                    .describe_setting_statement(statement)?
                    .expect("SHOW has columns");
                return Ok(Some(StatementResult::Query(QueryResult {
                    columns,
                    rows: vec![vec![Value::Text(self.format_setting(spec))]],
                })));
            }
            ast::Statement::Reset(reset) => {
                match &reset.reset {
                    ast::Reset::ALL => {
                        let defaults = SessionSettings::create(self.default_lock_timeout);
                        // RESET ALL skips parameters that cannot change in an established session.
                        for spec in list_settings()
                            .iter()
                            .filter(|spec| matches!(spec.context, SettingContext::Session))
                        {
                            if self.db.strict && matches!(spec.effect, SettingEffect::Planner) {
                                continue;
                            }
                            self.reset_setting(spec, &defaults);
                        }
                        self.settings.custom.values_mut().for_each(String::clear);
                        self.settings_on_commit
                            .as_mut()
                            .expect("RESET runs in a transaction")
                            .custom
                            .values_mut()
                            .for_each(String::clear);
                    }
                    ast::Reset::ConfigurationParameter(name) => {
                        let name = normalize_setting_name(name)?;
                        let spec = match resolve_setting(&name) {
                            Ok(spec) => spec,
                            Err(_) if is_custom_setting_name(&name) => {
                                let name = name.to_ascii_lowercase();
                                self.custom_settings.insert(name.clone());
                                self.settings.custom.insert(name.clone(), String::new());
                                self.settings_on_commit
                                    .as_mut()
                                    .expect("RESET runs in a transaction")
                                    .custom
                                    .insert(name, String::new());
                                return Ok(Some(StatementResult::Affected(0)));
                            }
                            Err(error) => return Err(error),
                        };
                        if matches!(spec.effect, SettingEffect::TransactionIsolation) {
                            return self
                                .abort_with_error(create_cannot_reset_setting_error(spec.name));
                        }
                        spec.validate_access(self.db.strict, true)?;
                        self.reset_setting(
                            spec,
                            &SessionSettings::create(self.default_lock_timeout),
                        );
                    }
                    _ => return Ok(None),
                }
                return Ok(Some(StatementResult::Affected(0)));
            }
            ast::Statement::Set(ast::Set::SingleAssignment {
                scope,
                hivevar,
                variable,
                values,
            }) => {
                if *hivevar
                    || !matches!(
                        scope,
                        None | Some(ast::ContextModifier::Session | ast::ContextModifier::Local)
                    )
                {
                    return Err(PgError::create(SqlState::SyntaxError, "invalid SET scope"));
                }
                (
                    normalize_setting_name(variable)?,
                    values.clone(),
                    *scope == Some(ast::ContextModifier::Local),
                )
            }
            ast::Statement::Set(ast::Set::SetTimeZone { local, value }) => {
                ("TimeZone".into(), vec![value.clone()], *local)
            }
            ast::Statement::Set(ast::Set::SetNames {
                charset_name,
                collation_name: None,
            }) => (
                "client_encoding".into(),
                vec![ast::Expr::Identifier(charset_name.clone())],
                false,
            ),
            ast::Statement::Set(ast::Set::SetNamesDefault {}) => (
                "client_encoding".into(),
                vec![ast::Expr::Identifier(ast::Ident::new("DEFAULT"))],
                false,
            ),
            _ => return Ok(None),
        };
        let spec = resolve_setting(&name)?;
        if matches!(spec.effect, SettingEffect::TransactionIsolation) {
            spec.validate_access(self.db.strict, false)?;
            if values.len() != 1 {
                return self.abort_with_error(PgError::create(
                    SqlState::InvalidParameterValue,
                    "SET requires a single value",
                ));
            }
            if matches!(values.as_slice(), [ast::Expr::Identifier(ident)] if ident.quote_style.is_none() && ident.value.eq_ignore_ascii_case("default"))
            {
                return self.abort_with_error(create_cannot_reset_setting_error(spec.name));
            }
            let SettingValue::Isolation(isolation) =
                parse_setting_value(spec, &parse_setting_text(&values[0], false)?)?
            else {
                unreachable!("transaction isolation uses the isolation setting type")
            };
            self.set_transaction_isolation(isolation)?;
            return Ok(Some(StatementResult::Affected(0)));
        }
        spec.validate_access(self.db.strict, true)?;
        let reset = matches!(values.as_slice(), [ast::Expr::Identifier(ident)] if ident.quote_style.is_none() && (ident.value.eq_ignore_ascii_case("default") || (matches!(spec.kind, SettingType::TimeZone) && ident.value.eq_ignore_ascii_case("local"))));
        if reset {
            let defaults = SessionSettings::create(self.default_lock_timeout);
            let value = defaults.read_value(spec);
            self.settings.assign_value(spec, value.clone());
            if !local {
                self.settings_on_commit
                    .as_mut()
                    .expect("SET runs in a transaction")
                    .assign_value(spec, value);
            }
        } else {
            let value = if matches!(spec.kind, SettingType::SearchPath) {
                if values.is_empty() {
                    return Err(create_invalid_setting_error(spec.name));
                }
                let names = values
                    .iter()
                    .map(|expr| parse_setting_text(expr, true))
                    .collect::<Result<Vec<_>>>()?;
                SettingValue::SearchPath {
                    text: format_search_path(&names),
                    names,
                }
            } else {
                if values.len() != 1 {
                    return Err(PgError::create(
                        SqlState::InvalidParameterValue,
                        "SET requires a single value",
                    ));
                }
                parse_setting_value(spec, &parse_setting_text(&values[0], false)?)?
            };
            self.settings.assign_value(spec, value.clone());
            if !local {
                self.settings_on_commit
                    .as_mut()
                    .expect("SET runs in a transaction")
                    .assign_value(spec, value);
            }
        }
        Ok(Some(StatementResult::Affected(0)))
    }

    fn reset_setting(&mut self, spec: &SettingSpec, defaults: &SessionSettings) {
        let value = defaults.read_value(spec);
        self.settings.assign_value(spec, value.clone());
        self.settings_on_commit
            .as_mut()
            .expect("RESET runs in a transaction")
            .assign_value(spec, value);
    }

    pub(super) fn capture_settings(&self) -> SessionSettings {
        self.settings.clone()
    }
    pub(super) fn restore_settings(&mut self, settings: SessionSettings) {
        self.settings = settings;
    }

    fn format_setting(&self, spec: &SettingSpec) -> String {
        if matches!(spec.effect, SettingEffect::TransactionIsolation) {
            let isolation = match self.transaction {
                Some(SessionTransactionState::Active(transaction))
                | Some(SessionTransactionState::Aborted { transaction }) => transaction.isolation,
                None => self.settings.default_isolation,
            };
            format_isolation(isolation)
        } else {
            self.settings.format_value(spec)
        }
    }

    pub(super) fn create_guc_execution_context(&self) -> Arc<Mutex<GucExecutionContext>> {
        Arc::new(Mutex::new(GucExecutionContext {
            current: self.settings.clone(),
            on_commit: self
                .settings_on_commit
                .clone()
                .expect("statement has transaction settings"),
            defaults: SessionSettings::create(self.default_lock_timeout),
            custom_settings: self.custom_settings.clone(),
            isolation: match self.transaction.expect("statement has a transaction") {
                SessionTransactionState::Active(transaction)
                | SessionTransactionState::Aborted { transaction } => transaction.isolation,
            },
            strict: self.db.strict,
        }))
    }

    pub(super) fn apply_guc_execution_context(
        &mut self,
        context: &Arc<Mutex<GucExecutionContext>>,
    ) {
        let context = context.lock().expect("GUC context mutex is poisoned");
        self.settings = context.current.clone();
        self.settings_on_commit = Some(context.on_commit.clone());
        self.custom_settings
            .extend(context.custom_settings.iter().cloned());
    }

    pub(super) fn apply_guc_custom_settings(&mut self, context: &Arc<Mutex<GucExecutionContext>>) {
        let context = context.lock().expect("GUC context mutex is poisoned");
        self.custom_settings
            .extend(context.custom_settings.iter().cloned());
    }

    pub(super) fn abort_with_guc_error<T>(
        &mut self,
        context: &Arc<Mutex<GucExecutionContext>>,
        error: PgError,
    ) -> Result<T> {
        self.apply_guc_custom_settings(context);
        self.abort_with_error(error)
    }
}

impl SessionSettings {
    fn read_value(&self, spec: &SettingSpec) -> SettingValue {
        match spec.effect {
            SettingEffect::TimeZone => SettingValue::Text(self.timezone.clone()),
            SettingEffect::LockTimeout => {
                SettingValue::Integer(self.lock_timeout.as_millis() as i64)
            }
            SettingEffect::StatementTimeout => {
                SettingValue::Integer(self.statement_timeout.as_millis() as i64)
            }
            SettingEffect::Isolation => SettingValue::Isolation(self.default_isolation),
            SettingEffect::TransactionIsolation => {
                unreachable!("transaction isolation has no session value")
            }
            SettingEffect::SearchPath => SettingValue::SearchPath {
                names: self.search_path.clone(),
                text: self.search_path_text.clone(),
            },
            SettingEffect::Compatibility | SettingEffect::Planner => self
                .values
                .get(spec.name)
                .expect("registered value exists")
                .clone(),
        }
    }
}

fn normalize_setting_name(name: &ast::ObjectName) -> Result<String> {
    name.0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|ident| ident.value.clone())
                .ok_or_else(|| PgError::create(SqlState::SyntaxError, "invalid setting name"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("."))
}

fn parse_setting_text(expression: &ast::Expr, identifier: bool) -> Result<String> {
    match expression {
        ast::Expr::Identifier(ident) => Ok(if identifier {
            crate::executor::normalize_identifier(ident)
        } else {
            ident.value.clone()
        }),
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value)
            | ast::Value::EscapedStringLiteral(value)
            | ast::Value::NationalStringLiteral(value)
            | ast::Value::DoubleQuotedString(value)
            | ast::Value::Number(value, _) => Ok(value.clone()),
            ast::Value::Boolean(value) => Ok(value.to_string()),
            ast::Value::DollarQuotedString(value) => Ok(value.value.clone()),
            _ => Err(PgError::create(SqlState::SyntaxError, "invalid SET value")),
        },
        ast::Expr::Interval(_) => Err(PgError::create(
            SqlState::FeatureNotSupported,
            "interval-valued settings are not implemented",
        )),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } => Ok(format!("-{}", parse_setting_text(expr, false)?)),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Plus,
            expr,
        } => parse_setting_text(expr, false),
        _ => Err(PgError::create(SqlState::SyntaxError, "invalid SET value")),
    }
}

fn create_invalid_setting_error(name: &str) -> PgError {
    PgError::create(
        SqlState::InvalidParameterValue,
        format!("invalid value for parameter {name}"),
    )
}

fn create_cannot_reset_setting_error(name: &str) -> PgError {
    PgError::create(
        SqlState::FeatureNotSupported,
        format!("parameter {name:?} cannot be reset"),
    )
}

fn create_invalid_custom_setting_name_error(name: &str) -> PgError {
    PgError::create(
        SqlState::InvalidName,
        format!("invalid configuration parameter name {name:?}"),
    )
}

fn parse_setting_value(spec: &SettingSpec, text: &str) -> Result<SettingValue> {
    let invalid = || create_invalid_setting_error(spec.name);
    match spec.kind {
        SettingType::Timeout => parse_timeout(text, spec.name)
            .map(|value| SettingValue::Integer(value.as_millis() as i64)),
        SettingType::TimeZone => {
            if let Some(zone) = chrono_tz::TZ_VARIANTS
                .iter()
                .find(|zone| zone.name().eq_ignore_ascii_case(text))
            {
                return Ok(SettingValue::Text(zone.name().into()));
            }
            if (text.starts_with(['+', '-']) && text.contains(':'))
                || text.to_ascii_uppercase().starts_with("UTC")
                || text.to_ascii_uppercase().starts_with("GMT")
            {
                crate::coercion::time_zones::parse_zone(text).map_err(|_| invalid())?;
                return Ok(SettingValue::Text(text.into()));
            }
            if let Ok(hours) = text.parse::<f64>() {
                if !hours.is_finite() || hours.abs() >= 168.0 {
                    return Err(invalid());
                }
                let seconds = (hours * 3600.0) as i32;
                let sign = if seconds < 0 { '-' } else { '+' };
                let inverse = if seconds < 0 { '+' } else { '-' };
                let seconds = seconds.abs();
                let mut offset = format!("{:02}", seconds / 3600);
                if seconds % 3600 != 0 {
                    offset.push_str(&format!(":{:02}", (seconds / 60) % 60));
                }
                if seconds % 60 != 0 {
                    offset.push_str(&format!(":{:02}", seconds % 60));
                }
                return Ok(SettingValue::Text(format!(
                    "<{sign}{offset}>{inverse}{offset}"
                )));
            }
            Err(invalid())
        }
        SettingType::Isolation => match text.to_ascii_lowercase().as_str() {
            "read committed" => Ok(SettingValue::Isolation(IsolationLevel::ReadCommitted)),
            "repeatable read" => Ok(SettingValue::Isolation(IsolationLevel::RepeatableRead)),
            "read uncommitted" | "serializable" => Err(PgError::create(
                SqlState::FeatureNotSupported,
                "isolation level is not implemented",
            )),
            _ => Err(invalid()),
        },
        SettingType::Encoding => {
            let name = text
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_uppercase();
            if matches!(name.as_str(), "UTF8" | "UNICODE") {
                Ok(SettingValue::Text("UTF8".into()))
            } else if matches!(
                name.as_str(),
                "SQLASCII" | "LATIN1" | "LATIN2" | "WIN1252" | "EUCJP" | "SJIS" | "GBK" | "BIG5"
            ) {
                Err(PgError::create(
                    SqlState::FeatureNotSupported,
                    "only UTF8 client encoding is supported",
                ))
            } else {
                Err(invalid())
            }
        }
        SettingType::ApplicationName => {
            let mut end = text.len().min(63);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let text = text.as_bytes()[..end]
                .iter()
                .map(|byte| {
                    if (32..=126).contains(byte) {
                        (*byte as char).to_string()
                    } else {
                        format!("\\x{byte:02x}")
                    }
                })
                .collect::<String>();
            Ok(SettingValue::Text(text))
        }
        SettingType::Boolean => {
            let text = text.to_ascii_lowercase();
            if text == "1" {
                return Ok(SettingValue::Boolean(true));
            }
            if text == "0" {
                return Ok(SettingValue::Boolean(false));
            }
            let matches = [
                ("true", true),
                ("false", false),
                ("yes", true),
                ("no", false),
                ("on", true),
                ("off", false),
            ]
            .into_iter()
            .filter(|(name, _)| !text.is_empty() && name.starts_with(&text))
            .collect::<Vec<_>>();
            if matches.len() == 1 {
                Ok(SettingValue::Boolean(matches[0].1))
            } else {
                Err(invalid())
            }
        }
        SettingType::Integer { min, max, unit } => {
            let text = text.trim();
            let (base, units): (f64, &[(f64, &str)]) = match unit {
                MemoryUnit::None => (1.0, &[]),
                MemoryUnit::Kilobytes => (
                    1024.0,
                    &[
                        (1099511627776.0, "TB"),
                        (1073741824.0, "GB"),
                        (1048576.0, "MB"),
                        (1024.0, "kB"),
                        (1.0, "B"),
                    ],
                ),
                MemoryUnit::Blocks => (
                    8192.0,
                    &[
                        (1099511627776.0, "TB"),
                        (1073741824.0, "GB"),
                        (1048576.0, "MB"),
                        (1024.0, "kB"),
                        (1.0, "B"),
                    ],
                ),
            };
            let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
            let hexadecimal = unsigned
                .strip_prefix("0x")
                .or_else(|| unsigned.strip_prefix("0X"));
            let is_hex_without_unit = hexadecimal
                .is_some_and(|digits| digits.bytes().all(|digit| digit.is_ascii_hexdigit()));
            let (number, multiplier, next) = units
                .iter()
                .enumerate()
                .filter(|_| !is_hex_without_unit)
                .find_map(|(index, (size, suffix))| {
                    text.strip_suffix(suffix).map(|number| {
                        (
                            number.trim(),
                            *size / base,
                            units.get(index + 1).map(|(size, _)| *size / base),
                        )
                    })
                })
                .unwrap_or((text, 1.0, None));
            let value = parse_number(number).ok_or_else(invalid)? * multiplier;
            let value = next
                .map_or(value, |next| (value / next).round_ties_even() * next)
                .round_ties_even();
            if !value.is_finite() || value < min as f64 || value > max as f64 {
                return Err(invalid());
            }
            Ok(SettingValue::Integer(value as i64))
        }
        SettingType::Real { min } => {
            let value = text.trim().parse::<f64>().map_err(|_| invalid())?;
            if !value.is_finite() || value < min {
                return Err(invalid());
            }
            Ok(SettingValue::Real(value))
        }
        SettingType::PlanCacheMode => {
            let value = text.to_ascii_lowercase();
            if ["auto", "force_generic_plan", "force_custom_plan"].contains(&value.as_str()) {
                Ok(SettingValue::Text(value))
            } else {
                Err(invalid())
            }
        }
        SettingType::Text => Ok(SettingValue::Text(text.into())),
        SettingType::SearchPath => {
            unreachable!("search path is parsed from its SQL identifier list")
        }
    }
}

fn parse_function_setting_value(spec: &SettingSpec, text: &str) -> Result<SettingValue> {
    if !matches!(spec.kind, SettingType::SearchPath) {
        return parse_setting_value(spec, text);
    }
    let names = parse_search_path(text).ok_or_else(|| create_invalid_setting_error(spec.name))?;
    Ok(SettingValue::SearchPath {
        names,
        text: text.into(),
    })
}

fn parse_search_path(text: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.next_if(|ch| ch.is_whitespace()).is_some() {}
        let Some(first) = chars.peek().copied() else {
            return Some(values);
        };
        let value = if first == '"' {
            chars.next();
            let mut value = String::new();
            loop {
                match chars.next()? {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        value.push('"');
                    }
                    '"' => break,
                    ch => value.push(ch),
                }
            }
            value
        } else {
            let mut value = String::new();
            while let Some(ch) = chars.peek().copied() {
                if ch == ',' || ch.is_whitespace() {
                    break;
                }
                if ch == '"' {
                    return None;
                }
                chars.next();
                value.push(ch);
            }
            if value.is_empty() {
                return None;
            }
            value.to_ascii_lowercase()
        };
        values.push(value);
        while chars.next_if(|ch| ch.is_whitespace()).is_some() {}
        match chars.next() {
            None => return Some(values),
            Some(',') => {
                let mut remaining = chars.clone();
                while remaining.next_if(|ch| ch.is_whitespace()).is_some() {}
                remaining.peek()?;
            }
            Some(_) => return None,
        }
    }
}

fn format_isolation(isolation: IsolationLevel) -> String {
    match isolation {
        IsolationLevel::ReadCommitted => "read committed",
        IsolationLevel::RepeatableRead => "repeatable read",
    }
    .into()
}

fn is_custom_setting_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let valid = |part: &str| {
        let mut bytes = part.bytes();
        bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_' || !byte.is_ascii())
            && bytes.all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$') || !byte.is_ascii()
            })
    };
    parts.clone().count() >= 2 && parts.all(valid)
}

fn format_search_path(names: &[String]) -> String {
    names
        .iter()
        .map(|name| {
            if !name.is_empty()
                && !is_quoted_keyword(name)
                && name.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_lowercase()
                        || byte == b'_'
                        || (index > 0 && (byte.is_ascii_digit() || byte == b'$'))
                })
            {
                name.clone()
            } else {
                format!("\"{}\"", name.replace('"', "\"\""))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_units(value: i64, units: &[(i64, &str)]) -> String {
    if value == 0 {
        return "0".into();
    }
    let (size, suffix) = units
        .iter()
        .find(|(size, _)| value % size == 0)
        .expect("unit list includes base unit");
    format!("{}{suffix}", value / size)
}

fn parse_number(text: &str) -> Option<f64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (digits, radix) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (hex, 16)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (digits, 8)
    } else {
        (digits, 10)
    };
    if digits.chars().all(|ch| ch.is_digit(radix)) {
        u64::from_str_radix(digits, radix)
            .ok()
            .map(|number| number as f64 * if text.starts_with('-') { -1.0 } else { 1.0 })
    } else if radix != 16 && digits.contains(['.', 'e', 'E']) {
        text.parse::<f64>().ok()
    } else {
        None
    }
}

fn parse_timeout(text: &str, parameter: &str) -> Result<Duration> {
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
        Some(_) => return Err(create_invalid_setting_error(parameter)),
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
        .take_while(|digit| char::from(*digit).is_digit(radix))
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
        .ok_or_else(|| create_invalid_setting_error(parameter))?;
    Ok(Duration::from_millis(milliseconds as u64))
}

fn is_quoted_keyword(name: &str) -> bool {
    "all analyse analyze and any array as asc asymmetric authorization between bigint binary bit boolean both case cast char character check coalesce collate collation column concurrently constraint create cross current_catalog current_date current_role current_schema current_time current_timestamp current_user dec decimal default deferrable desc distinct do else end except exists extract false fetch float for foreign freeze from full grant greatest group grouping having ilike in initially inner inout int integer intersect interval into is isnull join json json_array json_arrayagg json_exists json_object json_objectagg json_query json_scalar json_serialize json_table json_value lateral leading least left like limit localtime localtimestamp merge_action national natural nchar none normalize not notnull null nullif numeric offset on only or order out outer overlaps overlay placing position precision primary real references returning right row select session_user setof similar smallint some substring symmetric system_user table tablesample then time timestamp to trailing treat trim true union unique user using values varchar variadic verbose when where window with xmlattributes xmlconcat xmlelement xmlexists xmlforest xmlnamespaces xmlparse xmlpi xmlroot xmlserialize xmltable".split_whitespace().any(|keyword| keyword == name)
}

fn resolve_show_setting(variable: &[ast::Ident]) -> Result<&'static SettingSpec> {
    let name = variable
        .iter()
        .map(|ident| ident.value.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if variable.len() > 1
        && variable.iter().all(|ident| ident.quote_style.is_none())
        && let Some(spec) = list_settings().iter().find(|spec| {
            spec.aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(&name))
        })
    {
        return Ok(spec);
    }
    resolve_setting(&name)
}
