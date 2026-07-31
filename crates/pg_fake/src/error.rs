use thiserror::Error;

/// Postgres SQLSTATE error code (§11, Tier A).
///
/// Codes are added incrementally as features land. The code string is the
/// canonical 5-character Postgres error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlState {
    // 00 — Successful Completion
    SuccessfulCompletion, // 00000

    // 0A — Feature Not Supported
    FeatureNotSupported, // 0A000

    // 22 — Data Exception
    NumericValueOutOfRange,    // 22003
    DivisionByZero,            // 22012
    InvalidTextRepresentation, // 22P02

    // 23 — Integrity Constraint Violation
    NotNullViolation, // 23502
    UniqueViolation,  // 23505
    CheckViolation,   // 23514

    // 25 — Invalid Transaction State
    ActiveSqlTransaction,   // 25001
    InFailedSqlTransaction, // 25P02

    // 40 — Transaction Rollback
    SerializationFailure, // 40001
    DeadlockDetected,     // 40P01

    // 42 — Syntax Error / Access Rule
    UndefinedTable,    // 42P01
    DuplicateTable,    // 42P07
    SyntaxError,       // 42601
    UndefinedColumn,   // 42703
    UndefinedFunction, // 42883
    UndefinedObject,   // 42704
    CannotCoerce,      // 42846
    DatatypeMismatch,  // 42804

    // 55 — Object Not In Prerequisite State
    LockNotAvailable, // 55P03

    // XX — Internal Error
    InternalError, // XX000
}

impl SqlState {
    /// The 5-character Postgres error code string.
    pub fn code(self) -> &'static str {
        match self {
            SqlState::SuccessfulCompletion => "00000",
            SqlState::FeatureNotSupported => "0A000",
            SqlState::NumericValueOutOfRange => "22003",
            SqlState::DivisionByZero => "22012",
            SqlState::InvalidTextRepresentation => "22P02",
            SqlState::NotNullViolation => "23502",
            SqlState::UniqueViolation => "23505",
            SqlState::CheckViolation => "23514",
            SqlState::ActiveSqlTransaction => "25001",
            SqlState::InFailedSqlTransaction => "25P02",
            SqlState::SerializationFailure => "40001",
            SqlState::DeadlockDetected => "40P01",
            SqlState::UndefinedTable => "42P01",
            SqlState::DuplicateTable => "42P07",
            SqlState::SyntaxError => "42601",
            SqlState::UndefinedColumn => "42703",
            SqlState::UndefinedFunction => "42883",
            SqlState::UndefinedObject => "42704",
            SqlState::CannotCoerce => "42846",
            SqlState::DatatypeMismatch => "42804",
            SqlState::LockNotAvailable => "55P03",
            SqlState::InternalError => "XX000",
        }
    }
}

impl std::fmt::Display for SqlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// A Postgres-compatible error (§11).
///
/// `sqlstate` is Tier A (guaranteed to match Postgres).
/// `message`, `detail`, `hint`, `position` are Tier B (best effort).
#[derive(Debug, Error, PartialEq)]
#[error("{sqlstate}: {message}")]
pub struct PgError {
    pub sqlstate: SqlState,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<usize>,
}

impl PgError {
    pub fn new(sqlstate: SqlState, message: impl Into<String>) -> Self {
        PgError {
            sqlstate,
            message: message.into(),
            detail: None,
            hint: None,
            position: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_position(mut self, pos: usize) -> Self {
        self.position = Some(pos);
        self
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, PgError>;
