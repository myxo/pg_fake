use std::collections::{BTreeMap, BTreeSet};

use crate::storage::{Version, VersionChain};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub commit_seq: CommitSeq,
    pub in_flight: BTreeSet<Xid>,
    statuses: BTreeMap<Xid, TransactionStatus>,
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
            in_flight: manager
                .statuses
                .iter()
                .filter_map(|(xid, status)| {
                    if matches!(status, TransactionStatus::InFlight) {
                        Some(*xid)
                    } else {
                        None
                    }
                })
                .collect(),
            statuses: manager.statuses.clone(),
        }
    }
}

pub fn is_visible(version: &Version, snapshot: &Snapshot, current_xid: Xid) -> bool {
    let xmin_visible = version.xmin == current_xid
        || matches!(
            snapshot.statuses.get(&version.xmin),
            Some(TransactionStatus::Committed(commit_seq))
                if *commit_seq <= snapshot.commit_seq && !snapshot.in_flight.contains(&version.xmin)
        );
    let xmax_invisible = matches!(
        version.xmax,
        Some(xmax) if xmax == current_xid
            || matches!(
                snapshot.statuses.get(&xmax),
                Some(TransactionStatus::Committed(commit_seq))
                    if *commit_seq <= snapshot.commit_seq && !snapshot.in_flight.contains(&xmax)
            )
    );

    xmin_visible && !xmax_invisible
}

pub fn visible_version<'a>(
    chain: &'a VersionChain,
    snapshot: &Snapshot,
    current_xid: Xid,
) -> Option<&'a Version> {
    chain
        .versions
        .iter()
        .find(|version| is_visible(version, snapshot, current_xid))
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

        assert!(is_visible(&inserted, &snapshot, writer));
        assert!(!is_visible(&inserted, &snapshot, reader));
    }

    #[test]
    fn committed_before_snapshot_is_visible() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        manager.commit(writer);
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);

        assert!(is_visible(&version(writer, None), &snapshot, reader));
    }

    #[test]
    fn committed_after_snapshot_is_invisible() {
        let mut manager = TransactionManager::new();
        let writer = manager.begin();
        let reader = manager.begin();
        let snapshot = Snapshot::new(&manager);
        manager.commit(writer);

        assert!(!is_visible(&version(writer, None), &snapshot, reader));
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
            reader
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
            reader
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
            visible_version(&chain, &snapshot, reader),
            Some(&chain.versions[0])
        );
    }
}
