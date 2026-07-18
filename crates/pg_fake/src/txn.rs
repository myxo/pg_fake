use std::collections::BTreeMap;

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
}
