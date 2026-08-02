use std::collections::{BTreeMap, BTreeSet, VecDeque};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitForGraph {
    edges: BTreeMap<Xid, BTreeSet<Xid>>,
    victims: BTreeSet<Xid>,
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

impl Default for WaitForGraph {
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
        let mut blockers = holder_conflicts.into_iter().collect::<BTreeSet<_>>();
        if let Some(waiter) = first_waiter.filter(|waiter| *waiter != xid) {
            blockers.insert(waiter);
        }
        LockAttempt::Blocked(blockers.into_iter().collect())
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

impl WaitForGraph {
    pub fn new() -> Self {
        WaitForGraph {
            edges: BTreeMap::new(),
            victims: BTreeSet::new(),
        }
    }

    pub fn wait_for(&mut self, waiter: Xid, blockers: &[Xid]) -> Option<Xid> {
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
        let cycle = self.cycle_containing(waiter)?;
        let victim = *cycle
            .iter()
            .next_back()
            .expect("a cycle must contain a transaction");
        self.victims.insert(victim);
        Some(victim)
    }

    pub fn clear_wait(&mut self, waiter: Xid) {
        self.edges.remove(&waiter);
    }

    pub fn remove_transaction(&mut self, xid: Xid) {
        self.edges.remove(&xid);
        for blockers in self.edges.values_mut() {
            blockers.remove(&xid);
        }
        self.edges.retain(|_, blockers| !blockers.is_empty());
        self.victims.remove(&xid);
    }

    pub fn take_victim(&mut self, xid: Xid) -> bool {
        self.victims.remove(&xid)
    }

    fn cycle_containing(&self, xid: Xid) -> Option<BTreeSet<Xid>> {
        let reachable = self.reachable(xid, false);
        let reaches_xid = self.reachable(xid, true);
        let cycle = reachable
            .intersection(&reaches_xid)
            .copied()
            .collect::<BTreeSet<_>>();
        (cycle.len() > 1).then_some(cycle)
    }

    fn reachable(&self, start: Xid, reverse: bool) -> BTreeSet<Xid> {
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

    #[test]
    fn wait_for_graph_selects_highest_xid_in_cycle() {
        let mut graph = WaitForGraph::new();

        assert_eq!(graph.wait_for(Xid(1), &[Xid(2)]), None);
        assert_eq!(graph.wait_for(Xid(2), &[Xid(3)]), None);
        assert_eq!(graph.wait_for(Xid(3), &[Xid(1)]), Some(Xid(3)));
        assert!(graph.take_victim(Xid(3)));
        assert!(!graph.take_victim(Xid(1)));
    }

    #[test]
    fn removing_wait_edge_breaks_cycle() {
        let mut graph = WaitForGraph::new();
        graph.wait_for(Xid(4), &[Xid(7)]);
        assert_eq!(graph.wait_for(Xid(7), &[Xid(4)]), Some(Xid(7)));

        graph.clear_wait(Xid(7));
        assert_eq!(graph.wait_for(Xid(8), &[Xid(4)]), None);
        graph.remove_transaction(Xid(4));
        assert_eq!(graph.wait_for(Xid(7), &[Xid(8)]), None);
    }
}
