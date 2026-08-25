use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{
    catalog::TableId,
    storage::{RowId, RowVersion, RowVersionChain},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Xid(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CommitSeq(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionStatus {
    InFlight,
    Committed(CommitSeq),
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) commit_seq: CommitSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RowLockKey {
    pub(crate) table_id: TableId,
    pub(crate) row_id: RowId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowLockMode {
    Share,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowLock {
    holders: BTreeMap<Xid, RowLockMode>,
    waiters: VecDeque<Xid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RowLockManager {
    locks: BTreeMap<RowLockKey, RowLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaitForGraph {
    edges: BTreeMap<Xid, BTreeSet<Xid>>,
    victims: BTreeSet<Xid>,
}

pub(crate) enum RowLockAttempt {
    Acquired,
    Blocked(Vec<Xid>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransactionRegistry {
    next_xid: u64,
    commit_seq: CommitSeq,
    statuses: BTreeMap<Xid, TransactionStatus>,
    retained_snapshots: BTreeMap<Xid, CommitSeq>,
}

impl Default for TransactionRegistry {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}

impl Default for RowLockManager {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}

impl Default for WaitForGraph {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}

impl RowLockManager {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        RowLockManager {
            locks: BTreeMap::new(),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn acquire(
        &mut self,
        key: RowLockKey,
        xid: Xid,
        mode: RowLockMode,
    ) -> RowLockAttempt {
        let lock = self.locks.entry(key).or_insert_with(|| RowLock {
            holders: BTreeMap::new(),
            waiters: VecDeque::new(),
        });
        if let Some(held) = lock.holders.get(&xid)
            && (*held == RowLockMode::Update || mode == RowLockMode::Share)
        {
            return RowLockAttempt::Acquired;
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
            return RowLockAttempt::Acquired;
        }
        if !lock.waiters.contains(&xid) {
            lock.waiters.push_back(xid);
        }
        let mut blockers = holder_conflicts.into_iter().collect::<BTreeSet<_>>();
        if let Some(waiter) = first_waiter.filter(|waiter| *waiter != xid) {
            blockers.insert(waiter);
        }
        RowLockAttempt::Blocked(blockers.into_iter().collect())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn cancel_wait(&mut self, key: RowLockKey, xid: Xid) {
        let Some(lock) = self.locks.get_mut(&key) else {
            return;
        };
        lock.waiters.retain(|waiter| *waiter != xid);
        if lock.holders.is_empty() && lock.waiters.is_empty() {
            self.locks.remove(&key);
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn release_transaction_locks(&mut self, xid: Xid) {
        self.locks.retain(|_, lock| {
            lock.holders.remove(&xid);
            lock.waiters.retain(|waiter| *waiter != xid);
            !lock.holders.is_empty() || !lock.waiters.is_empty()
        });
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_waiters(&self) -> bool {
        self.locks.values().any(|lock| !lock.waiters.is_empty())
    }
}

impl WaitForGraph {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        WaitForGraph {
            edges: BTreeMap::new(),
            victims: BTreeSet::new(),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn register_wait_dependencies(
        &mut self,
        waiter: Xid,
        blockers: &[Xid],
    ) -> Option<Xid> {
        let blockers = blockers
            .iter()
            .copied()
            .filter(|blocker| *blocker != waiter)
            .collect::<BTreeSet<_>>();
        if blockers.is_empty() {
            self.edges.remove(&waiter);
            return None;
        }
        self.edges.insert(waiter, blockers);
        let cycle = self.find_cycle_containing(waiter)?;
        let victim = *cycle
            .iter()
            .next_back()
            .expect("a cycle must contain a transaction");
        self.victims.insert(victim);
        Some(victim)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn clear_wait(&mut self, waiter: Xid) {
        self.edges.remove(&waiter);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn remove_transaction(&mut self, xid: Xid) {
        self.edges.remove(&xid);
        for blockers in self.edges.values_mut() {
            blockers.remove(&xid);
        }
        self.edges.retain(|_, blockers| !blockers.is_empty());
        self.victims.remove(&xid);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn take_victim(&mut self, xid: Xid) -> bool {
        self.victims.remove(&xid)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn find_cycle_containing(&self, xid: Xid) -> Option<BTreeSet<Xid>> {
        let collect_reachable_transactions = self.collect_reachable_transactions(xid, false);
        let reaches_xid = self.collect_reachable_transactions(xid, true);
        let cycle = collect_reachable_transactions
            .intersection(&reaches_xid)
            .copied()
            .collect::<BTreeSet<_>>();
        (cycle.len() > 1).then_some(cycle)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn collect_reachable_transactions(&self, start: Xid, reverse: bool) -> BTreeSet<Xid> {
        let mut reached = BTreeSet::from([start]);
        let mut pending = vec![start];
        while let Some(xid) = pending.pop() {
            if reverse {
                for (waiter, blockers) in &self.edges {
                    if blockers.contains(&xid) && reached.insert(*waiter) {
                        pending.push(*waiter);
                    }
                }
            } else if let Some(blockers) = self.edges.get(&xid) {
                for blocker in blockers {
                    if reached.insert(*blocker) {
                        pending.push(*blocker);
                    }
                }
            }
        }
        reached
    }
}

impl TransactionRegistry {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        TransactionRegistry {
            next_xid: 1,
            commit_seq: CommitSeq(0),
            statuses: BTreeMap::new(),
            retained_snapshots: BTreeMap::new(),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn begin(&mut self) -> Xid {
        let xid = Xid(self.next_xid);
        self.next_xid += 1;
        let previous = self.statuses.insert(xid, TransactionStatus::InFlight);
        assert!(previous.is_none());
        xid
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn commit(&mut self, xid: Xid) -> CommitSeq {
        assert!(matches!(
            self.get_status(xid),
            Some(TransactionStatus::InFlight)
        ));
        self.commit_seq.0 += 1;
        let commit_seq = self.commit_seq;
        *self.statuses.get_mut(&xid).expect("transaction must exist") =
            TransactionStatus::Committed(commit_seq);
        self.retained_snapshots.remove(&xid);
        commit_seq
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn finish_read_only(&mut self, xid: Xid) {
        assert!(matches!(
            self.get_status(xid),
            Some(TransactionStatus::InFlight)
        ));
        self.statuses.remove(&xid);
        self.retained_snapshots.remove(&xid);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn abort(&mut self, xid: Xid) {
        assert!(matches!(
            self.get_status(xid),
            Some(TransactionStatus::InFlight)
        ));
        *self.statuses.get_mut(&xid).expect("transaction must exist") = TransactionStatus::Aborted;
        self.retained_snapshots.remove(&xid);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn get_status(&self, xid: Xid) -> Option<TransactionStatus> {
        self.statuses.get(&xid).copied()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn retain_snapshot(&mut self, xid: Xid, snapshot: Snapshot) {
        assert!(matches!(
            self.get_status(xid),
            Some(TransactionStatus::InFlight)
        ));
        let retained = self
            .retained_snapshots
            .entry(xid)
            .or_insert(snapshot.commit_seq);
        assert_eq!(*retained, snapshot.commit_seq);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_reclamation_horizon(&self) -> CommitSeq {
        self.retained_snapshots
            .values()
            .copied()
            .min()
            .unwrap_or(self.commit_seq)
    }
}

impl Snapshot {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create(manager: &TransactionRegistry) -> Self {
        Snapshot {
            commit_seq: manager.commit_seq,
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn is_visible(
    version: &RowVersion,
    snapshot: &Snapshot,
    current_xid: Xid,
    manager: &TransactionRegistry,
) -> bool {
    let xmin_visible = version.xmin == current_xid
        || matches!(
            manager.get_status(version.xmin),
            Some(TransactionStatus::Committed(commit_seq))
                if commit_seq <= snapshot.commit_seq
        );
    let xmax_invisible = matches!(
        version.xmax,
        Some(xmax) if xmax == current_xid
            || matches!(
                manager.get_status(xmax),
                Some(TransactionStatus::Committed(commit_seq))
                    if commit_seq <= snapshot.commit_seq
            )
    );

    xmin_visible && !xmax_invisible
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn find_visible_version<'a>(
    chain: &'a RowVersionChain,
    snapshot: &Snapshot,
    current_xid: Xid,
    manager: &TransactionRegistry,
) -> Option<&'a RowVersion> {
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn transitions_transaction_statuses_from_in_flight_to_final() {
        let mut manager = TransactionRegistry::create();
        let committed = manager.begin();
        let aborted = manager.begin();

        assert_eq!(
            manager.get_status(committed),
            Some(TransactionStatus::InFlight)
        );
        assert_eq!(
            manager.get_status(aborted),
            Some(TransactionStatus::InFlight)
        );

        manager.commit(committed);
        manager.abort(aborted);

        assert_eq!(
            manager.get_status(committed),
            Some(TransactionStatus::Committed(CommitSeq(1)))
        );
        assert_eq!(
            manager.get_status(aborted),
            Some(TransactionStatus::Aborted)
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn allocates_monotonic_xids_and_commit_sequences() {
        let mut manager = TransactionRegistry::create();
        let first = manager.begin();
        let second = manager.begin();

        assert_eq!(first, Xid(1));
        assert_eq!(second, Xid(2));
        assert_eq!(manager.commit(second), CommitSeq(1));
        assert_eq!(manager.commit(first), CommitSeq(2));
        assert_eq!(manager.commit_seq, CommitSeq(2));
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn create_version(xmin: Xid, xmax: Option<Xid>) -> RowVersion {
        RowVersion {
            xmin,
            xmax,
            row: vec![],
        }
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn shows_own_uncommitted_insert_only_to_its_transaction() {
        let mut manager = TransactionRegistry::create();
        let writer = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::create(&manager);
        let inserted = create_version(writer, None);

        assert!(is_visible(&inserted, &snapshot, writer, &manager));
        assert!(!is_visible(&inserted, &snapshot, reader, &manager));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn shows_version_committed_before_snapshot() {
        let mut manager = TransactionRegistry::create();
        let writer = manager.begin();
        manager.commit(writer);
        let reader = manager.begin();
        let snapshot = Snapshot::create(&manager);

        assert!(is_visible(
            &create_version(writer, None),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn hides_version_committed_after_snapshot() {
        let mut manager = TransactionRegistry::create();
        let writer = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::create(&manager);
        manager.commit(writer);

        assert!(!is_visible(
            &create_version(writer, None),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn hides_version_deleted_before_snapshot() {
        let mut manager = TransactionRegistry::create();
        let writer = manager.begin();
        manager.commit(writer);
        let deleter = manager.begin();
        manager.commit(deleter);
        let reader = manager.begin();
        let snapshot = Snapshot::create(&manager);

        assert!(!is_visible(
            &create_version(writer, Some(deleter)),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn keeps_version_visible_during_in_flight_delete() {
        let mut manager = TransactionRegistry::create();
        let writer = manager.begin();
        manager.commit(writer);
        let deleter = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::create(&manager);

        assert!(is_visible(
            &create_version(writer, Some(deleter)),
            &snapshot,
            reader,
            &manager
        ));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn finds_one_visible_version_in_a_chain() {
        let mut manager = TransactionRegistry::create();
        let writer = manager.begin();
        manager.commit(writer);
        let updater = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::create(&manager);
        let chain = RowVersionChain {
            versions: vec![
                create_version(writer, Some(updater)),
                create_version(updater, None),
            ],
        };

        assert_eq!(
            find_visible_version(&chain, &snapshot, reader, &manager),
            Some(&chain.versions[0])
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn selects_highest_xid_in_wait_for_cycle() {
        let mut graph = WaitForGraph::create();

        assert_eq!(graph.register_wait_dependencies(Xid(1), &[Xid(2)]), None);
        assert_eq!(graph.register_wait_dependencies(Xid(2), &[Xid(3)]), None);
        assert_eq!(
            graph.register_wait_dependencies(Xid(3), &[Xid(1)]),
            Some(Xid(3))
        );
        assert!(graph.take_victim(Xid(3)));
        assert!(!graph.take_victim(Xid(1)));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn breaks_cycle_when_removing_wait_edge() {
        let mut graph = WaitForGraph::create();
        graph.register_wait_dependencies(Xid(4), &[Xid(7)]);
        assert_eq!(
            graph.register_wait_dependencies(Xid(7), &[Xid(4)]),
            Some(Xid(7))
        );

        graph.clear_wait(Xid(7));
        assert_eq!(graph.register_wait_dependencies(Xid(8), &[Xid(4)]), None);
        graph.remove_transaction(Xid(4));
        assert_eq!(graph.register_wait_dependencies(Xid(7), &[Xid(8)]), None);
    }
}
