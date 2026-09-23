use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use rand_chacha::{ChaCha12Rng, rand_core::SeedableRng};

use crate::error::{PgError, Result, SqlState};

use super::Session;

mod state;
pub(crate) use state::DatabaseState;

#[derive(Clone)]
pub struct Db {
    pub(super) state: Arc<Mutex<DatabaseState>>,
    pub(super) condvar: Arc<Condvar>,
    default_lock_timeout: Duration,
    clock: Arc<Mutex<DatabaseClock>>,
    pub(super) rng: Arc<Mutex<ChaCha12Rng>>,
    pub(super) strict: bool,
}

pub struct DbBuilder {
    lock_timeout: Duration,
    mock_time: bool,
    seed: Option<u64>,
    strict: bool,
}

#[derive(Clone, Copy)]
enum DatabaseClock {
    Real,
    Mock(chrono::DateTime<chrono::Utc>),
}

impl Db {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn create() -> Self {
        Db::create_builder().build()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn create_builder() -> DbBuilder {
        DbBuilder {
            lock_timeout: Duration::from_secs(1),
            mock_time: false,
            seed: None,
            strict: false,
        }
    }

    pub fn snapshot(&self) -> Self {
        let source = self.state.lock().expect("database mutex is poisoned");
        let mut state = source.clone();
        state.serializable = Arc::new(Mutex::new(
            source
                .serializable
                .lock()
                .expect("dependency graph is poisoned")
                .clone(),
        ));
        state.sequence_values = Arc::new(Mutex::new(
            source
                .sequence_values
                .lock()
                .expect("sequence storage is poisoned")
                .clone(),
        ));
        state.row_locks = Default::default();
        state.relation_locks = Default::default();
        state.wait_for = Default::default();
        state.advisory_locks = Default::default();
        for xid in state.transactions.collect_active_transactions() {
            state.abort_transaction(xid);
        }
        for schema in state.catalog_history.collect_temporary_schema_ids() {
            let reclaimed = state.catalog_history.drop_temporary_schema(schema);
            for table in reclaimed.tables {
                state.tables.remove(&table);
            }
            let mut sequences = state
                .sequence_values
                .lock()
                .expect("sequence storage is poisoned");
            for sequence in reclaimed.sequences {
                sequences.remove(&sequence);
            }
        }
        state.prune_versions();
        state.load_catalog(
            None,
            crate::txn::Snapshot::create(&state.transactions),
            None,
        );
        state.catalog.set_search_path(&["public".into()]);
        Self {
            state: Arc::new(Mutex::new(state)),
            condvar: Arc::new(Condvar::new()),
            default_lock_timeout: self.default_lock_timeout,
            clock: Arc::new(Mutex::new(
                *self.clock.lock().expect("clock mutex is poisoned"),
            )),
            rng: Arc::new(Mutex::new(
                self.rng.lock().expect("RNG mutex is poisoned").clone(),
            )),
            strict: self.strict,
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn create_session(&self) -> Session {
        Session::create(self.clone(), self.default_lock_timeout)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn read_clock(&self) -> chrono::DateTime<chrono::Utc> {
        match *self.clock.lock().expect("clock mutex is poisoned") {
            DatabaseClock::Real => chrono::Utc::now(),
            DatabaseClock::Mock(value) => value,
        }
    }

    /// Set the frozen mock clock. Real-clock databases reject the operation.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_time(&self, time: chrono::DateTime<chrono::Utc>) -> Result<()> {
        let mut clock = self.clock.lock().expect("clock mutex is poisoned");
        match &mut *clock {
            DatabaseClock::Mock(value) => {
                *value = time;
                Ok(())
            }
            DatabaseClock::Real => Err(PgError::create(
                SqlState::InvalidParameterValue,
                "mock time is disabled",
            )),
        }
    }

    /// Advance the frozen mock clock by `duration`. Real-clock databases reject
    /// the operation.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn advance_time(&self, duration: chrono::Duration) -> Result<()> {
        let mut clock = self.clock.lock().expect("clock mutex is poisoned");
        match &mut *clock {
            DatabaseClock::Mock(value) => {
                *value = value.checked_add_signed(duration).ok_or_else(|| {
                    PgError::create(SqlState::NumericValueOutOfRange, "clock time out of range")
                })?;
                Ok(())
            }
            DatabaseClock::Real => Err(PgError::create(
                SqlState::InvalidParameterValue,
                "mock time is disabled",
            )),
        }
    }
}

impl DbBuilder {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }
    /// Enable a frozen, deterministic database clock. It begins at the Unix
    /// epoch and can subsequently be controlled through `Db::set_time` and
    /// `Db::advance_time`.
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_mock_time_enabled(mut self, enabled: bool) -> Self {
        self.mock_time = enabled;
        self
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_random_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn set_strict_mode_enabled(mut self, enabled: bool) -> Self {
        self.strict = enabled;
        self
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn build(self) -> Db {
        Db {
            state: Arc::new(Mutex::new(DatabaseState::create())),
            condvar: Arc::new(Condvar::new()),
            default_lock_timeout: self.lock_timeout,
            clock: Arc::new(Mutex::new(if self.mock_time {
                DatabaseClock::Mock(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
            } else {
                DatabaseClock::Real
            })),
            rng: Arc::new(Mutex::new(match self.seed {
                Some(seed) => ChaCha12Rng::seed_from_u64(seed),
                None => ChaCha12Rng::from_os_rng(),
            })),
            strict: self.strict,
        }
    }
}

impl Default for Db {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}

#[cfg(test)]
mod tests;
