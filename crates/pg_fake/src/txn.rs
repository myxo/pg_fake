use std::collections::{BTreeMap, VecDeque};

use crate::{
    catalog::TableId,
    storage::{RowId, Version, VersionChain},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Xid(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitSeq(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    InFlight,
    Committed(CommitSeq),
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    pub commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowLockKey {
    pub table_id: TableId,
    pub row_id: RowId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowLockMode {
    Share,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowLock {
    holders: BTreeMap<Xid, RowLockMode>,
    waiters: VecDeque<Xid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowLockManager {
    locks: BTreeMap<RowLockKey, RowLock>,
}

pub enum LockAttempt {
    Acquired,
    Blocked(Vec<Xid>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionManager {
    next_xid: u64,
    commit_seq: CommitSeq,
    statuses: BTreeMap<Xid, TransactionStatus>,
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for RowLockManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RowLockManager {
    pub fn new() -> Self {
        RowLockManager {
            locks: BTreeMap::new(),
        }
    }

    pub fn acquire(&mut self, key: RowLockKey, xid: Xid, mode: RowLockMode) -> LockAttempt {
        let lock = self.locks.entry(key).or_insert_with(|| RowLock {
            holders: BTreeMap::new(),
            waiters: VecDeque::new(),
        });
        if let Some(held) = lock.holders.get(&xid)
            && (*held == RowLockMode::Update || mode == RowLockMode::Share)
        {
            return LockAttempt::Acquired;
        }
        let holder_conflicts = lock
            .holders
            .iter()
            .filter_map(|(holder, held)| {
                (*holder != xid && (*held == RowLockMode::Update || mode == RowLockMode::Update))
                    .then_some(*holder)
            })
            .collect::<Vec<_>>();
        let first_waiter = lock.waiters.front().copied();
        if holder_conflicts.is_empty() && first_waiter.is_none_or(|waiter| waiter == xid) {
            if first_waiter == Some(xid) {
                lock.waiters.pop_front();
            }
            lock.holders.insert(xid, mode);
            return LockAttempt::Acquired;
        }
        if !lock.waiters.contains(&xid) {
            lock.waiters.push_back(xid);
        }
        LockAttempt::Blocked(holder_conflicts)
    }

    pub fn cancel_wait(&mut self, key: RowLockKey, xid: Xid) {
        let Some(lock) = self.locks.get_mut(&key) else {
            return;
        };
        lock.waiters.retain(|waiter| *waiter != xid);
        if lock.holders.is_empty() && lock.waiters.is_empty() {
            self.locks.remove(&key);
        }
    }

    pub fn release(&mut self, xid: Xid) {
        self.locks.retain(|_, lock| {
            lock.holders.remove(&xid);
            lock.waiters.retain(|waiter| *waiter != xid);
            !lock.holders.is_empty() || !lock.waiters.is_empty()
        });
    }

    pub fn has_waiters(&self) -> bool {
        self.locks.values().any(|lock| !lock.waiters.is_empty())
    }
}

impl TransactionManager {
    pub fn new() -> Self {
        TransactionManager {
            next_xid: 1,
            commit_seq: CommitSeq(0),
            statuses: BTreeMap::new(),
        }
    }

    pub fn begin(&mut self) -> Xid {
        let xid = Xid(self.next_xid);
        self.next_xid += 1;
        let previous = self.statuses.insert(xid, TransactionStatus::InFlight);
        assert!(previous.is_none());
        xid
    }

    pub fn commit(&mut self, xid: Xid) -> CommitSeq {
        assert!(matches!(
            self.status(xid),
            Some(TransactionStatus::InFlight)
        ));
        self.commit_seq.0 += 1;
        let commit_seq = self.commit_seq;
        *self.statuses.get_mut(&xid).expect("transaction must exist") =
            TransactionStatus::Committed(commit_seq);
        commit_seq
    }

    pub fn abort(&mut self, xid: Xid) {
        assert!(matches!(
            self.status(xid),
            Some(TransactionStatus::InFlight)
        ));
        *self.statuses.get_mut(&xid).expect("transaction must exist") = TransactionStatus::Aborted;
    }

    pub fn status(&self, xid: Xid) -> Option<TransactionStatus> {
        self.statuses.get(&xid).copied()
    }

    pub fn commit_seq(&self) -> CommitSeq {
        self.commit_seq
    }
}

impl Snapshot {
    pub fn new(manager: &TransactionManager) -> Self {
        Snapshot {
            commit_seq: manager.commit_seq,
        }
    }
}

pub fn is_visible(
    version: &Version,
    snapshot: &Snapshot,
    current_xid: Xid,
    manager: &TransactionManager,
) -> bool {
    let xmin_visible = version.xmin == current_xid
        || matches!(
            manager.status(version.xmin),
            Some(TransactionStatus::Committed(commit_seq))
                if commit_seq <= snapshot.commit_seq
        );
    let xmax_invisible = matches!(
        version.xmax,
        Some(xmax) if xmax == current_xid
            || matches!(
                manager.status(xmax),
                Some(TransactionStatus::Committed(commit_seq))
                    if commit_seq <= snapshot.commit_seq
            )
    );

    xmin_visible && !xmax_invisible
}

pub fn visible_version<'a>(
    chain: &'a VersionChain,
    snapshot: &Snapshot,
    current_xid: Xid,
    manager: &TransactionManager,
) -> Option<&'a Version> {
    chain
        .versions
        .iter()
        .rev()
        .find(|version| is_visible(version, snapshot, current_xid, manager))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_statuses_transition_from_in_flight_to_final() {
        let mut manager = TransactionManager::new();
        let committed = manager.begin();
        let aborted = manager.begin();

        assert_eq!(manager.status(committed), Some(TransactionStatus::InFlight));
        assert_eq!(manager.status(aborted), Some(TransactionStatus::InFlight));

        manager.commit(committed);
        manager.abort(aborted);

        assert_eq!(
            manager.status(committed),
            Some(TransactionStatus::Committed(CommitSeq(1)))
        );
        assert_eq!(manager.status(aborted), Some(TransactionStatus::Aborted));
    }

    #[test]
    fn xids_and_commit_sequences_are_monotonic() {
        let mut manager = TransactionManager::new();
        let first = manager.begin();
        let second = manager.begin();

        assert_eq!(first, Xid(1));
        assert_eq!(second, Xid(2));
        assert_eq!(manager.commit(second), CommitSeq(1));
        assert_eq!(manager.commit(first), CommitSeq(2));
        assert_eq!(manager.commit_seq(), CommitSeq(2));
    }

    fn version(xmin: Xid, xmax: Option<Xid>) -> Version {
        Version {
            xmin,
            xmax,
            row: vec![],
        }
    }

    #[test]
    fn own_uncommitted_insert_is_visible_only_to_its_transaction() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);
        let inserted = version(writer, None);

        assert!(is_visible(&inserted, &snapshot, writer, &manager));
        assert!(!is_visible(&inserted, &snapshot, reader, &manager));
    }

    #[test]
    fn committed_before_snapshot_is_visible() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        manager.commit(writer);
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);

        assert!(is_visible(
            &version(writer, None),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    fn committed_after_snapshot_is_invisible() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);
        manager.commit(writer);

        assert!(!is_visible(
            &version(writer, None),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    fn delete_committed_before_snapshot_hides_the_version() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        manager.commit(writer);
        let deleter = manager.begin();
        manager.commit(deleter);
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);

        assert!(!is_visible(
            &version(writer, Some(deleter)),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    fn in_flight_delete_leaves_the_version_visible() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        manager.commit(writer);
        let deleter = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);

        assert!(is_visible(
            &version(writer, Some(deleter)),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    fn visible_version_selects_one_version_from_a_chain() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        manager.commit(writer);
        let updater = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);
        let chain = VersionChain {
            versions: vec![version(writer, Some(updater)), version(updater, None)],
        };

        assert_eq!(
            visible_version(&chain, &snapshot, reader, &manager),
            Some(&chain.versions[0])
        );
    }
}
