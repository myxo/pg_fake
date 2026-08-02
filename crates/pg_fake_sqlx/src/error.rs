use std::{borrow::Cow, error::Error as StdError, fmt};

use pg_fake::error::{PgError, SqlState};
use sqlx::error::{DatabaseError, ErrorKind};

#[derive(Debug)]
pub struct PgFakeDatabaseError {
    error: PgError,
}

impl PgFakeDatabaseError {
    pub fn detail(&self) -> Option<&str> {
        self.error.detail.as_deref()
    }

    pub fn hint(&self) -> Option<&str> {
        self.error.hint.as_deref()
    }

    pub fn position(&self) -> Option<usize> {
        self.error.position
    }
}

impl From<PgError> for PgFakeDatabaseError {
    fn from(error: PgError) -> Self {
        Self { error }
    }
}

impl fmt::Display for PgFakeDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl StdError for PgFakeDatabaseError {}

impl DatabaseError for PgFakeDatabaseError {
    fn message(&self) -> &str {
        &self.error.message
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(self.error.sqlstate.code()))
    }

    fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> ErrorKind {
        match self.error.sqlstate {
            SqlState::UniqueViolation => ErrorKind::UniqueViolation,
            SqlState::NotNullViolation => ErrorKind::NotNullViolation,
            SqlState::CheckViolation => ErrorKind::CheckViolation,
            _ => ErrorKind::Other,
        }
    }
}

pub(crate) fn database_error(error: PgError) -> sqlx::Error {
    sqlx::Error::Database(Box::new(PgFakeDatabaseError::from(error)))
}
