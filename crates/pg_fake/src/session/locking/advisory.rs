use crate::{
    Result,
    advisory::{AdvisoryAttempt, AdvisoryRequest, PendingAdvisory},
    executor::{DatabaseState, StatementContext},
};
use std::{
    sync::{Condvar, MutexGuard},
    time::{Duration, Instant},
};

pub(in crate::session) fn acquire_advisory_lock<'a>(
    condvar: &Condvar,
    timeout: Duration,
    statement_deadline: Option<Instant>,
    mut state: MutexGuard<'a, DatabaseState>,
    request: AdvisoryRequest,
    context: &StatementContext,
) -> Result<MutexGuard<'a, DatabaseState>> {
    let lock_deadline = (timeout != Duration::ZERO).then(|| Instant::now() + timeout);
    let deadline = match (lock_deadline, statement_deadline) {
        (Some(lock), Some(statement)) => Some(lock.min(statement)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    };
    loop {
        let error = if state.wait_for.take_victim(request.xid) {
            Some(super::create_deadlock_error())
        } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            Some(
                if statement_deadline.is_some_and(|statement| Some(statement) == deadline) {
                    super::create_statement_timeout_error()
                } else {
                    super::create_lock_timeout_error()
                },
            )
        } else {
            None
        };
        if let Some(error) = error {
            state
                .advisory_locks
                .lock()
                .expect("advisory lock mutex is poisoned")
                .cancel_wait(request);
            state.wait_for.clear_wait(request.xid);
            condvar.notify_all();
            return Err(error);
        }
        let attempt = state
            .advisory_locks
            .lock()
            .expect("advisory lock mutex is poisoned")
            .acquire(request, true);
        match attempt {
            AdvisoryAttempt::Acquired => {
                state.wait_for.clear_wait(request.xid);
                *context
                    .advisory
                    .pending
                    .lock()
                    .expect("pending advisory mutex is poisoned") =
                    PendingAdvisory::Acquired(request);
                condvar.notify_all();
                return Ok(state);
            }
            AdvisoryAttempt::Blocked(blockers) => {
                if let Some(victim) = state
                    .wait_for
                    .register_wait_dependencies(request.xid, &blockers)
                {
                    condvar.notify_all();
                    if victim == request.xid {
                        continue;
                    }
                }
            }
        }
        state = if let Some(deadline) = deadline {
            condvar
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                .expect("database mutex is poisoned")
                .0
        } else {
            condvar.wait(state).expect("database mutex is poisoned")
        };
    }
}
