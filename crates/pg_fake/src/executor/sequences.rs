use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use sqlparser::ast;

use crate::{
    catalog::{Catalog, SequenceId, SequenceSchema},
    coercion,
    error::{PgError, Result, SqlState, reject_unsupported},
    value::BaseType,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SequenceValueState {
    pub(crate) last_value: i64,
    pub(crate) is_called: bool,
}

pub(crate) type SequenceStorage = Arc<Mutex<BTreeMap<SequenceId, SequenceValueState>>>;
pub(crate) type SequenceSessionStorage = Arc<Mutex<SequenceSessionState>>;

#[derive(Debug, Default)]
pub(crate) struct SequenceSessionState {
    current_values: BTreeMap<SequenceId, i64>,
    last_used: Option<SequenceId>,
}

#[derive(Clone)]
pub(crate) struct SequenceExecutionContext {
    sequences: BTreeMap<String, SequenceSchema>,
    tables: BTreeSet<String>,
    values: SequenceStorage,
    session: SequenceSessionStorage,
}

impl SequenceExecutionContext {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create(
        catalog: &Catalog,
        values: SequenceStorage,
        session: SequenceSessionStorage,
    ) -> Self {
        SequenceExecutionContext {
            sequences: catalog
                .iterate_sequences()
                .map(|sequence| (sequence.name.clone(), sequence.clone()))
                .collect(),
            tables: catalog
                .iterate_tables()
                .map(|table| table.name.clone())
                .collect(),
            values,
            session,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_empty(values: SequenceStorage, session: SequenceSessionStorage) -> Self {
        SequenceExecutionContext {
            sequences: BTreeMap::new(),
            tables: BTreeSet::new(),
            values,
            session,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn require_sequence(&self, name: &str) -> Result<&SequenceSchema> {
        let name = normalize_sequence_name(name)?;
        if self.tables.contains(&name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a sequence"),
            ));
        }
        self.sequences.get(&name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn get_next_value(&self, name: &str) -> Result<i64> {
        let sequence = self.require_sequence(name)?;
        let value = {
            let mut values = self.values.lock().expect("sequence storage is poisoned");
            let state = values
                .get_mut(&sequence.id)
                .expect("catalog sequence must have storage");
            if !state.is_called {
                state.is_called = true;
                state.last_value
            } else {
                let next = i128::from(state.last_value) + i128::from(sequence.increment);
                if next > i128::from(sequence.max_value) {
                    if !sequence.cycle {
                        return Err(create_limit_error(sequence));
                    }
                    state.last_value = sequence.min_value;
                } else if next < i128::from(sequence.min_value) {
                    if !sequence.cycle {
                        return Err(create_limit_error(sequence));
                    }
                    state.last_value = sequence.max_value;
                } else {
                    state.last_value = next as i64;
                }
                state.last_value
            }
        };
        let mut session = self.session.lock().expect("sequence session is poisoned");
        session.current_values.insert(sequence.id, value);
        session.last_used = Some(sequence.id);
        Ok(value)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn get_current_value(&self, name: &str) -> Result<i64> {
        let sequence = self.require_sequence(name)?;
        self.session
            .lock()
            .expect("sequence session is poisoned")
            .current_values
            .get(&sequence.id)
            .copied()
            .ok_or_else(|| {
                PgError::create(
                    SqlState::ObjectNotInPrerequisiteState,
                    format!(
                        "currval of sequence {:?} is not yet defined in this session",
                        sequence.name
                    ),
                )
            })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn get_last_value(&self) -> Result<i64> {
        let session = self.session.lock().expect("sequence session is poisoned");
        let Some(id) = session.last_used else {
            return Err(create_lastval_error());
        };
        if !self.sequences.values().any(|sequence| sequence.id == id) {
            return Err(create_lastval_error());
        }
        session
            .current_values
            .get(&id)
            .copied()
            .ok_or_else(create_lastval_error)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn set_value(&self, name: &str, value: i64, is_called: bool) -> Result<i64> {
        let sequence = self.require_sequence(name)?;
        if !(sequence.min_value..=sequence.max_value).contains(&value) {
            return Err(PgError::create(
                SqlState::NumericValueOutOfRange,
                format!(
                    "setval: value {value} is out of bounds for sequence {:?} ({}, {})",
                    sequence.name, sequence.min_value, sequence.max_value
                ),
            ));
        }
        let mut values = self.values.lock().expect("sequence storage is poisoned");
        let state = values
            .get_mut(&sequence.id)
            .expect("catalog sequence must have storage");
        state.last_value = value;
        state.is_called = is_called;
        drop(values);
        if is_called {
            self.session
                .lock()
                .expect("sequence session is poisoned")
                .current_values
                .insert(sequence.id, value);
        }
        Ok(value)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn get_owned_sequence(&self, table: &str, column: &str) -> Result<Option<String>> {
        let table = normalize_sequence_name(table)?;
        let column = normalize_sequence_name(column)?;
        Ok(self.sequences.values().find_map(|sequence| {
            (sequence.owned_by.as_ref() == Some(&(table.clone(), column.clone())))
                .then(|| sequence.name.clone())
        }))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn create_sequence_schema(
    name: String,
    data_type: Option<&ast::DataType>,
    options: &[ast::SequenceOptions],
) -> Result<SequenceSchema> {
    let data_type = match data_type {
        Some(data_type) => coercion::convert_ast_data_type(data_type)?.base,
        None => BaseType::Int8,
    };
    create_sequence_schema_for_type(name, data_type, options)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn create_sequence_schema_for_type(
    name: String,
    data_type: BaseType,
    options: &[ast::SequenceOptions],
) -> Result<SequenceSchema> {
    let (type_min, type_max) = match data_type {
        BaseType::Int2 => (i64::from(i16::MIN), i64::from(i16::MAX)),
        BaseType::Int4 => (i64::from(i32::MIN), i64::from(i32::MAX)),
        BaseType::Int8 => (i64::MIN, i64::MAX),
        _ => {
            return Err(PgError::create(
                SqlState::InvalidParameterValue,
                "sequence type must be smallint, integer, or bigint",
            ));
        }
    };
    let mut increment = 1;
    let mut min_value = None;
    let mut max_value = None;
    let mut start_value = None;
    let mut cache = 1;
    let mut cycle = false;
    for option in options {
        match option {
            ast::SequenceOptions::IncrementBy(value, _) => increment = parse_i64(value)?,
            ast::SequenceOptions::MinValue(value) => {
                min_value = value.as_ref().map(parse_i64).transpose()?
            }
            ast::SequenceOptions::MaxValue(value) => {
                max_value = value.as_ref().map(parse_i64).transpose()?
            }
            ast::SequenceOptions::StartWith(value, _) => start_value = Some(parse_i64(value)?),
            ast::SequenceOptions::Cache(value) => cache = parse_i64(value)?,
            ast::SequenceOptions::Cycle(no_cycle) => cycle = !no_cycle,
        }
    }
    if increment == 0 {
        return Err(PgError::create(
            SqlState::InvalidParameterValue,
            "INCREMENT must not be zero",
        ));
    }
    if !(type_min..=type_max).contains(&increment) {
        return Err(PgError::create(
            SqlState::InvalidParameterValue,
            "INCREMENT must be within the sequence type range",
        ));
    }
    if cache < 1 {
        return Err(PgError::create(
            SqlState::InvalidParameterValue,
            "CACHE must be greater than zero",
        ));
    }
    let ascending = increment > 0;
    let min_value = min_value.unwrap_or(if ascending { 1 } else { type_min });
    let max_value = max_value.unwrap_or(if ascending { type_max } else { -1 });
    if min_value < type_min || max_value > type_max || min_value >= max_value {
        return Err(PgError::create(
            SqlState::InvalidParameterValue,
            "MINVALUE must be less than MAXVALUE and within the sequence type range",
        ));
    }
    let start_value = start_value.unwrap_or(if ascending { min_value } else { max_value });
    if !(min_value..=max_value).contains(&start_value) {
        return Err(PgError::create(
            SqlState::InvalidParameterValue,
            "START value must be between MINVALUE and MAXVALUE",
        ));
    }
    Ok(SequenceSchema {
        id: SequenceId(0),
        name,
        data_type,
        increment,
        min_value,
        max_value,
        start_value,
        cycle,
        cache,
        owned_by: None,
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn parse_i64(expression: &ast::Expr) -> Result<i64> {
    let text = match expression {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Number(value, _) => value.to_string(),
            _ => return reject_unsupported("sequence option must be an integer"),
        },
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Plus,
            expr,
        } => format!("+{}", extract_unsigned_integer(expr)?),
        ast::Expr::UnaryOp {
            op: ast::UnaryOperator::Minus,
            expr,
        } => format!("-{}", extract_unsigned_integer(expr)?),
        ast::Expr::Nested(expression) => return parse_i64(expression),
        _ => return reject_unsupported("sequence option must be an integer"),
    };
    text.parse::<i64>().map_err(|_| {
        PgError::create(
            SqlState::NumericValueOutOfRange,
            "sequence option is out of range for bigint",
        )
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn extract_unsigned_integer(expression: &ast::Expr) -> Result<&str> {
    match expression {
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Number(value, _) => Ok(value),
            _ => reject_unsupported("sequence option must be an integer"),
        },
        _ => reject_unsupported("sequence option must be an integer"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn normalize_sequence_name(name: &str) -> Result<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut part_quoted = false;
    let mut characters = name.trim().chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' if quoted && characters.peek() == Some(&'"') => {
                current.push('"');
                characters.next();
            }
            '"' => {
                quoted = !quoted;
                part_quoted = true;
            }
            '.' if !quoted => {
                parts.push((current, part_quoted));
                current = String::new();
                part_quoted = false;
            }
            character => current.push(character),
        }
    }
    if quoted || current.is_empty() {
        return Err(PgError::create(
            SqlState::InvalidTextRepresentation,
            format!("invalid name syntax: {name}"),
        ));
    }
    parts.push((current, part_quoted));
    let normalize = |part: &(String, bool)| {
        let (part, quoted) = part;
        let part = part.trim();
        if *quoted {
            part.to_string()
        } else {
            part.to_ascii_lowercase()
        }
    };
    match parts.as_slice() {
        [name] => Ok(normalize(name)),
        [schema, name] if normalize(schema) == "public" => Ok(normalize(name)),
        _ => reject_unsupported("schemas are not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_limit_error(sequence: &SequenceSchema) -> PgError {
    PgError::create(
        SqlState::SequenceGeneratorLimitExceeded,
        format!(
            "nextval: reached {} value of sequence {:?} ({})",
            if sequence.increment > 0 {
                "maximum"
            } else {
                "minimum"
            },
            sequence.name,
            if sequence.increment > 0 {
                sequence.max_value
            } else {
                sequence.min_value
            }
        ),
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_lastval_error() -> PgError {
    PgError::create(
        SqlState::ObjectNotInPrerequisiteState,
        "lastval is not yet defined in this session",
    )
}
