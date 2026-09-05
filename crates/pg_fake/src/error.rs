use thiserror::Error;

/// Postgres SQLSTATE error code (§11, Tier A).
///
/// Codes are added incrementally as features land. The code string is the
/// canonical 5-character Postgres error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlState {
    // 00 — Successful Completion
    SuccessfulCompletion, // 00000

    // 08 — Connection Exception
    ProtocolViolation, // 08P01

    // 0A — Feature Not Supported
    FeatureNotSupported, // 0A000

    // 22 — Data Exception
    NumericValueOutOfRange,              // 22003
    NullValueNotAllowed,                 // 22004
    DivisionByZero,                      // 22012
    InvalidRowCountInLimitClause,        // 2201W
    InvalidRowCountInResultOffsetClause, // 2201X
    InvalidTextRepresentation,           // 22P02
    InvalidParameterValue,               // 22023
    StringDataRightTruncation,           // 22001
    SequenceGeneratorLimitExceeded,      // 2200H

    // 21 — Cardinality Violation
    CardinalityViolation, // 21000

    // 23 — Integrity Constraint Violation
    NotNullViolation,    // 23502
    UniqueViolation,     // 23505
    CheckViolation,      // 23514
    ForeignKeyViolation, // 23503

    // 25 — Invalid Transaction State
    ActiveSqlTransaction,   // 25001
    NoActiveSqlTransaction, // 25P01
    InFailedSqlTransaction, // 25P02

    // 2B — Dependent Objects Still Exist
    DependentObjectsStillExist, // 2BP01

    // 3F — Invalid Schema Name
    InvalidSchemaName, // 3F000

    // 40 — Transaction Rollback
    SerializationFailure, // 40001
    DeadlockDetected,     // 40P01

    // 57 — Operator Intervention
    QueryCanceled, // 57014

    // 42 — Syntax Error / Access Rule
    UndefinedTable,            // 42P01
    DuplicateTable,            // 42P07
    DuplicateSchema,           // 42P06
    DuplicateColumn,           // 42701
    DuplicateFunction,         // 42723
    DuplicateObject,           // 42710
    AmbiguousColumn,           // 42702
    SyntaxError,               // 42601
    UndefinedColumn,           // 42703
    UndefinedFunction,         // 42883
    UndefinedObject,           // 42704
    UndefinedParameter,        // 42P02
    AmbiguousParameter,        // 42P08
    CannotCoerce,              // 42846
    DatatypeMismatch,          // 42804
    InvalidColumnReference,    // 42P10
    InvalidRecursion,          // 42P19
    GroupingError,             // 42803
    WrongObjectType,           // 42809
    GeneratedAlways,           // 428C9
    InvalidTableDefinition,    // 42P16
    InvalidObjectDefinition,   // 42P17
    InvalidFunctionDefinition, // 42P13

    // 2F — SQL Routine Exception
    FunctionExecutedNoReturnStatement, // 2F005

    // P0 — PL/pgSQL Error
    RaiseException, // P0001

    // 55 — Object Not In Prerequisite State
    ObjectNotInPrerequisiteState, // 55000
    LockNotAvailable,             // 55P03

    // XX — Internal Error
    InternalError, // XX000
}

impl SqlState {
    /// The 5-character Postgres error code string.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_code(self) -> &'static str {
        match self {
            SqlState::SuccessfulCompletion => "00000",
            SqlState::ProtocolViolation => "08P01",
            SqlState::FeatureNotSupported => "0A000",
            SqlState::NumericValueOutOfRange => "22003",
            SqlState::NullValueNotAllowed => "22004",
            SqlState::DivisionByZero => "22012",
            SqlState::InvalidRowCountInLimitClause => "2201W",
            SqlState::InvalidRowCountInResultOffsetClause => "2201X",
            SqlState::InvalidTextRepresentation => "22P02",
            SqlState::InvalidParameterValue => "22023",
            SqlState::StringDataRightTruncation => "22001",
            SqlState::SequenceGeneratorLimitExceeded => "2200H",
            SqlState::CardinalityViolation => "21000",
            SqlState::NotNullViolation => "23502",
            SqlState::UniqueViolation => "23505",
            SqlState::CheckViolation => "23514",
            SqlState::ForeignKeyViolation => "23503",
            SqlState::ActiveSqlTransaction => "25001",
            SqlState::NoActiveSqlTransaction => "25P01",
            SqlState::InFailedSqlTransaction => "25P02",
            SqlState::DependentObjectsStillExist => "2BP01",
            SqlState::InvalidSchemaName => "3F000",
            SqlState::SerializationFailure => "40001",
            SqlState::DeadlockDetected => "40P01",
            SqlState::QueryCanceled => "57014",
            SqlState::UndefinedTable => "42P01",
            SqlState::DuplicateTable => "42P07",
            SqlState::DuplicateSchema => "42P06",
            SqlState::DuplicateColumn => "42701",
            SqlState::DuplicateFunction => "42723",
            SqlState::DuplicateObject => "42710",
            SqlState::AmbiguousColumn => "42702",
            SqlState::SyntaxError => "42601",
            SqlState::UndefinedColumn => "42703",
            SqlState::UndefinedFunction => "42883",
            SqlState::UndefinedObject => "42704",
            SqlState::UndefinedParameter => "42P02",
            SqlState::AmbiguousParameter => "42P08",
            SqlState::CannotCoerce => "42846",
            SqlState::DatatypeMismatch => "42804",
            SqlState::InvalidColumnReference => "42P10",
            SqlState::InvalidRecursion => "42P19",
            SqlState::GroupingError => "42803",
            SqlState::WrongObjectType => "42809",
            SqlState::GeneratedAlways => "428C9",
            SqlState::InvalidTableDefinition => "42P16",
            SqlState::InvalidObjectDefinition => "42P17",
            SqlState::InvalidFunctionDefinition => "42P13",
            SqlState::FunctionExecutedNoReturnStatement => "2F005",
            SqlState::RaiseException => "P0001",
            SqlState::ObjectNotInPrerequisiteState => "55000",
            SqlState::LockNotAvailable => "55P03",
            SqlState::InternalError => "XX000",
        }
    }
}

impl std::fmt::Display for SqlState {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.get_code())
    }
}

/// A Postgres-compatible error (§11).
///
/// `sqlstate` is Tier A (guaranteed to match Postgres).
/// `message`, `detail`, `hint`, `position` are Tier B (best effort).
#[derive(Debug, Clone, Error, PartialEq)]
#[error("{sqlstate}: {message}")]
pub struct PgError {
    pub sqlstate: SqlState,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<usize>,
}

impl PgError {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create(sqlstate: SqlState, message: impl Into<String>) -> Self {
        PgError {
            sqlstate,
            message: message.into(),
            detail: None,
            hint: None,
            position: None,
        }
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, PgError>;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn reject_unsupported<T>(message: impl Into<String>) -> Result<T> {
    Err(PgError::create(SqlState::FeatureNotSupported, message))
}
