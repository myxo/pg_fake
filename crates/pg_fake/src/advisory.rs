use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{catalog::SchemaId, txn::Xid};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy)]
pub(crate) enum AdvisoryFunction {
    XactLock(AdvisoryMode),
    TryXactLock(AdvisoryMode),
    SessionLock,
    SessionUnlock,
}

pub(crate) fn resolve_advisory_function(name: &str) -> Option<AdvisoryFunction> {
    Some(match name {
        "pg_advisory_xact_lock" => AdvisoryFunction::XactLock(AdvisoryMode::Exclusive),
        "pg_advisory_xact_lock_shared" => AdvisoryFunction::XactLock(AdvisoryMode::Shared),
        "pg_try_advisory_xact_lock" => AdvisoryFunction::TryXactLock(AdvisoryMode::Exclusive),
        "pg_try_advisory_xact_lock_shared" => AdvisoryFunction::TryXactLock(AdvisoryMode::Shared),
        "pg_advisory_lock" => AdvisoryFunction::SessionLock,
        "pg_advisory_unlock" => AdvisoryFunction::SessionUnlock,
        _ => return None,
    })
}

pub(crate) fn contains_advisory_function(value: &impl sqlparser::ast::Visit) -> bool {
    struct Detector(bool);
    impl sqlparser::ast::Visitor for Detector {
        type Break = ();
        fn pre_visit_expr(
            &mut self,
            expression: &sqlparser::ast::Expr,
        ) -> std::ops::ControlFlow<()> {
            if let sqlparser::ast::Expr::Function(function) = expression {
                self.0 |= crate::executor::normalize_function_name(&function.name)
                    .ok()
                    .and_then(|name| resolve_advisory_function(&name))
                    .is_some();
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut detector = Detector(false);
    let _ = value.visit(&mut detector);
    detector.0
}

#[derive(Clone)]
pub(crate) struct AdvisoryExecutionContext {
    pub(crate) enabled: bool,
    pub(crate) locks: Arc<Mutex<AdvisoryLockManager>>,
    pub(crate) session: SchemaId,
    pub(crate) xid: Xid,
    pub(crate) condvar: Arc<Condvar>,
    pub(crate) pending: Arc<Mutex<PendingAdvisory>>,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum PendingAdvisory {
    #[default]
    Idle,
    Waiting(AdvisoryRequest),
    Acquired(AdvisoryRequest),
}

impl AdvisoryExecutionContext {
    pub(crate) fn evaluate(
        &self,
        function: AdvisoryFunction,
        values: &[crate::value::Value],
    ) -> crate::Result<crate::value::Value> {
        use crate::value::Value;
        let key = match values {
            [Value::Int8(value)] => AdvisoryKey::BigInt(*value),
            [Value::Int4(first), Value::Int4(second)] => AdvisoryKey::IntegerPair(*first, *second),
            _ => unreachable!("advisory arguments were coerced"),
        };
        if matches!(function, AdvisoryFunction::SessionUnlock) {
            let released = self
                .locks
                .lock()
                .expect("advisory lock mutex is poisoned")
                .release_session_lock(key, self.session);
            self.condvar.notify_all();
            return Ok(Value::Bool(released));
        }
        let (mode, scope) = match function {
            AdvisoryFunction::XactLock(mode) | AdvisoryFunction::TryXactLock(mode) => {
                (mode, AdvisoryScope::Transaction(self.xid))
            }
            AdvisoryFunction::SessionLock => (AdvisoryMode::Exclusive, AdvisoryScope::Session),
            AdvisoryFunction::SessionUnlock => unreachable!(),
        };
        let request = AdvisoryRequest {
            key,
            mode,
            scope,
            session: self.session,
            xid: self.xid,
        };
        let mut pending = self
            .pending
            .lock()
            .expect("pending advisory mutex is poisoned");
        if let PendingAdvisory::Acquired(completed) = *pending {
            assert_eq!(completed, request);
            *pending = PendingAdvisory::Idle;
            return Ok(Value::Void);
        }
        let wait = !matches!(function, AdvisoryFunction::TryXactLock(_));
        let attempt = self
            .locks
            .lock()
            .expect("advisory lock mutex is poisoned")
            .acquire(request, wait);
        match attempt {
            AdvisoryAttempt::Acquired => {
                self.condvar.notify_all();
                Ok(if wait { Value::Void } else { Value::Bool(true) })
            }
            AdvisoryAttempt::Blocked(_) if !wait => Ok(Value::Bool(false)),
            AdvisoryAttempt::Blocked(_) => {
                *pending = PendingAdvisory::Waiting(request);
                Err(crate::error::PgError::create(
                    crate::error::SqlState::InternalError,
                    crate::executor::LOCK_PENDING,
                ))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AdvisoryKey {
    BigInt(i64),
    IntegerPair(i32, i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AdvisoryMode {
    Shared,
    Exclusive,
}

impl AdvisoryMode {
    fn conflicts_with(self, other: Self) -> bool {
        self == Self::Exclusive || other == Self::Exclusive
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AdvisoryScope {
    Transaction(Xid),
    Session,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdvisoryRequest {
    pub(crate) key: AdvisoryKey,
    pub(crate) mode: AdvisoryMode,
    pub(crate) scope: AdvisoryScope,
    pub(crate) session: SchemaId,
    pub(crate) xid: Xid,
}

#[derive(Clone, Debug, Default)]
struct AdvisoryLock {
    holders: BTreeMap<(SchemaId, AdvisoryScope, AdvisoryMode), usize>,
    waiters: VecDeque<AdvisoryRequest>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AdvisoryLockManager {
    locks: BTreeMap<AdvisoryKey, AdvisoryLock>,
    sessions: BTreeMap<SchemaId, Xid>,
}

pub(crate) enum AdvisoryAttempt {
    Acquired,
    Blocked(Vec<Xid>),
}

impl AdvisoryLockManager {
    pub(crate) fn register_transaction(&mut self, session: SchemaId, xid: Xid) {
        self.sessions.insert(session, xid);
    }

    pub(crate) fn acquire(&mut self, request: AdvisoryRequest, wait: bool) -> AdvisoryAttempt {
        let lock = self.locks.entry(request.key).or_default();
        let owns_lock = lock
            .holders
            .keys()
            .any(|(session, _, _)| *session == request.session);
        let mut blockers = lock
            .holders
            .keys()
            .filter(|(session, _, mode)| {
                *session != request.session && mode.conflicts_with(request.mode)
            })
            .map(|(session, _, _)| self.sessions[session])
            .collect::<BTreeSet<_>>();
        if !owns_lock {
            for waiter in &lock.waiters {
                if waiter.xid == request.xid {
                    break;
                }
                if waiter.mode.conflicts_with(request.mode) {
                    blockers.insert(waiter.xid);
                }
            }
        }
        if blockers.is_empty() {
            lock.waiters.retain(|waiter| waiter.xid != request.xid);
            let count = lock
                .holders
                .entry((request.session, request.scope, request.mode))
                .or_default();
            match request.scope {
                AdvisoryScope::Transaction(_) => *count = 1,
                AdvisoryScope::Session => *count += 1,
            }
            AdvisoryAttempt::Acquired
        } else {
            if wait && !lock.waiters.iter().any(|waiter| waiter.xid == request.xid) {
                lock.waiters.push_back(request);
            }
            AdvisoryAttempt::Blocked(blockers.into_iter().collect())
        }
    }

    pub(crate) fn release_session_lock(&mut self, key: AdvisoryKey, session: SchemaId) -> bool {
        let Some(lock) = self.locks.get_mut(&key) else {
            return false;
        };
        let owner = (session, AdvisoryScope::Session, AdvisoryMode::Exclusive);
        let Some(count) = lock.holders.get_mut(&owner) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            lock.holders.remove(&owner);
        }
        if lock.holders.is_empty() && lock.waiters.is_empty() {
            self.locks.remove(&key);
        }
        true
    }

    pub(crate) fn cancel_wait(&mut self, request: AdvisoryRequest) {
        if let Some(lock) = self.locks.get_mut(&request.key) {
            lock.waiters.retain(|waiter| waiter.xid != request.xid);
            if lock.holders.is_empty() && lock.waiters.is_empty() {
                self.locks.remove(&request.key);
            }
        }
    }

    pub(crate) fn release_transaction_locks(&mut self, xid: Xid) {
        self.locks.retain(|_, lock| {
            lock.holders
                .retain(|(_, scope, _), _| *scope != AdvisoryScope::Transaction(xid));
            lock.waiters.retain(|waiter| waiter.xid != xid);
            !lock.holders.is_empty() || !lock.waiters.is_empty()
        });
    }

    pub(crate) fn release_session_locks(&mut self, session: SchemaId) {
        self.locks.retain(|_, lock| {
            lock.holders.retain(|(owner, _, _), _| *owner != session);
            lock.waiters.retain(|waiter| waiter.session != session);
            !lock.holders.is_empty() || !lock.waiters.is_empty()
        });
        self.sessions.remove(&session);
    }

    #[cfg(test)]
    pub(crate) fn has_waiters(&self) -> bool {
        self.locks.values().any(|lock| !lock.waiters.is_empty())
    }
}
