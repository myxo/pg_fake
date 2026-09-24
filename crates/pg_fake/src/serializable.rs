use std::collections::{BTreeMap, BTreeSet};

use crate::{
    catalog::TableId,
    storage::{RowId, UniqueIndexKey},
    txn::{CommandId, CommitSeq, Xid},
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Access {
    Relation(TableId),
    Row(TableId, RowId),
    Unique(TableId, Vec<usize>, UniqueIndexKey),
}

impl Access {
    fn table_id(&self) -> TableId {
        match self {
            Self::Relation(table) | Self::Row(table, _) | Self::Unique(table, _, _) => *table,
        }
    }

    fn conflicts_with(&self, write: &Self) -> bool {
        match (self, write) {
            (Self::Relation(left), _) => *left == write.table_id(),
            (Self::Row(left_table, left_row), Self::Row(right_table, right_row)) => {
                left_table == right_table && left_row == right_row
            }
            (
                Self::Unique(left_table, left_columns, left_key),
                Self::Unique(right_table, right_columns, right_key),
            ) => {
                left_table == right_table && left_columns == right_columns && left_key == right_key
            }
            (_, Self::Relation(right)) => self.table_id() == *right,
            _ => false,
        }
    }
}

#[derive(Clone)]
struct Transaction {
    started_at: u64,
    snapshot: Option<CommitSeq>,
    end: Option<CommitSeq>,
    ended_at: Option<u64>,
    serializable: bool,
    reads: BTreeSet<Access>,
    writes: BTreeMap<TableId, BTreeSet<(CommandId, Access)>>,
}

#[derive(Clone, Default)]
pub(crate) struct DependencyGraph {
    event: u64,
    transactions: BTreeMap<Xid, Transaction>,
    edges: BTreeSet<(Xid, Xid)>,
    dismissed_edges: BTreeSet<(Xid, Xid)>,
}

impl DependencyGraph {
    pub(crate) fn begin(&mut self, xid: Xid) {
        self.event += 1;
        assert!(
            self.transactions
                .insert(
                    xid,
                    Transaction {
                        started_at: self.event,
                        snapshot: None,
                        end: None,
                        ended_at: None,
                        serializable: false,
                        reads: BTreeSet::new(),
                        writes: BTreeMap::new(),
                    }
                )
                .is_none()
        );
    }

    pub(crate) fn set_snapshot(
        &mut self,
        xid: Xid,
        snapshot: CommitSeq,
        serializable: bool,
    ) -> bool {
        let transaction = self
            .transactions
            .get_mut(&xid)
            .expect("transaction is registered");
        let newly_serializable = serializable && !transaction.serializable;
        if serializable {
            assert!(
                transaction
                    .snapshot
                    .is_none_or(|previous| previous == snapshot)
            );
            transaction.snapshot = Some(snapshot);
            transaction.serializable = true;
        }
        if newly_serializable {
            self.rebuild_edges();
        }
        newly_serializable
    }

    pub(crate) fn needs_write_tracking(&self) -> bool {
        self.transactions
            .values()
            .any(|transaction| transaction.serializable)
    }

    pub(crate) fn is_serializable(&self, xid: Xid) -> bool {
        self.transactions
            .get(&xid)
            .is_some_and(|transaction| transaction.serializable)
    }

    pub(crate) fn read(&mut self, xid: Xid, access: Access) {
        let transaction = self
            .transactions
            .get_mut(&xid)
            .expect("transaction is registered");
        if transaction.serializable && transaction.reads.insert(access) {
            self.rebuild_edges();
        }
    }

    pub(crate) fn replace_table_writes(
        &mut self,
        xid: Xid,
        table: TableId,
        writes: BTreeSet<(CommandId, Access)>,
    ) {
        let transaction = self
            .transactions
            .get_mut(&xid)
            .expect("transaction is registered");
        if writes.is_empty() {
            transaction.writes.remove(&table);
        } else {
            transaction.writes.insert(table, writes);
        }
        self.rebuild_edges();
    }

    pub(crate) fn commit(&mut self, xid: Xid, commit_seq: CommitSeq) {
        self.event += 1;
        let transaction = self
            .transactions
            .get_mut(&xid)
            .expect("transaction is registered");
        transaction.end = Some(commit_seq);
        transaction.ended_at = Some(self.event);
        self.reclaim();
    }

    pub(crate) fn abort(&mut self, xid: Xid) {
        self.transactions.remove(&xid);
        self.reclaim();
    }

    pub(crate) fn collect_transaction_edges(&self, xid: Xid) -> BTreeSet<(Xid, Xid)> {
        self.edges
            .iter()
            .filter(|(reader, writer)| *reader == xid || *writer == xid)
            .copied()
            .collect()
    }

    pub(crate) fn dismiss_transaction_conflicts(
        &mut self,
        xid: Xid,
        preexisting: &BTreeSet<(Xid, Xid)>,
    ) {
        self.dismissed_edges.extend(
            self.edges
                .iter()
                .filter(|(reader, writer)| {
                    (*reader == xid || *writer == xid) && !preexisting.contains(&(*reader, *writer))
                })
                .copied(),
        );
    }

    pub(crate) fn would_fail_on_commit(&self, xid: Xid) -> bool {
        if !self.is_serializable(xid) {
            return false;
        }
        self.edges.iter().any(|&(reader, pivot)| {
            if self.dismissed_edges.contains(&(reader, pivot)) {
                return false;
            }
            let incoming = &self.transactions[&reader];
            let middle = &self.transactions[&pivot];
            (pivot == xid || reader == xid && middle.ended_at.is_some())
                && self.edges.iter().any(|&(source, writer)| {
                    if source != pivot {
                        return false;
                    }
                    if self.dismissed_edges.contains(&(source, writer)) {
                        return false;
                    }
                    let outgoing = &self.transactions[&writer];
                    if !outgoing.serializable {
                        return false;
                    }
                    let Some(outgoing_end) = outgoing.ended_at else {
                        return false;
                    };
                    if middle.ended_at.is_some_and(|end| end <= outgoing_end) {
                        return false;
                    }
                    if reader != writer && incoming.ended_at.is_some_and(|end| end <= outgoing_end)
                    {
                        return false;
                    }
                    if incoming.writes.is_empty()
                        && outgoing.end.is_some_and(|end| {
                            incoming.snapshot.is_some_and(|snapshot| snapshot < end)
                        })
                    {
                        return false;
                    }
                    true
                })
        })
    }

    fn reclaim(&mut self) {
        let oldest_active = self
            .transactions
            .values()
            .filter(|transaction| transaction.end.is_none())
            .map(|transaction| transaction.started_at)
            .min();
        let mut retained = self
            .transactions
            .iter()
            .filter_map(|(&xid, transaction)| {
                (transaction.end.is_none()
                    || oldest_active
                        .is_some_and(|start| transaction.ended_at.is_some_and(|end| end > start)))
                .then_some(xid)
            })
            .collect::<BTreeSet<_>>();
        loop {
            let previous = retained.len();
            for &(reader, writer) in &self.edges {
                if retained.contains(&reader) && self.transactions.contains_key(&writer) {
                    retained.insert(writer);
                }
                if retained.contains(&writer) && self.transactions.contains_key(&reader) {
                    retained.insert(reader);
                }
            }
            if retained.len() == previous {
                break;
            }
        }
        self.transactions.retain(|xid, _| retained.contains(xid));
        self.dismissed_edges
            .retain(|(reader, writer)| retained.contains(reader) && retained.contains(writer));
        self.rebuild_edges();
    }

    fn rebuild_edges(&mut self) {
        self.edges.clear();
        for (&reader_xid, reader) in &self.transactions {
            let Some(snapshot) = reader.snapshot.filter(|_| reader.serializable) else {
                continue;
            };
            for (&writer_xid, writer) in &self.transactions {
                if reader_xid == writer_xid
                    || writer.end.is_some_and(|end| end <= snapshot)
                    || reader.ended_at.is_some_and(|end| end <= writer.started_at)
                {
                    continue;
                }
                if reader.reads.iter().any(|read| {
                    writer
                        .writes
                        .values()
                        .flatten()
                        .any(|(_, write)| read.conflicts_with(write))
                }) {
                    self.edges.insert((reader_xid, writer_xid));
                }
            }
        }
        self.dismissed_edges
            .retain(|edge| self.edges.contains(edge));
    }

    #[cfg(test)]
    pub(crate) fn has_edge(&self, reader: Xid, writer: Xid) -> bool {
        self.edges.contains(&(reader, writer))
    }
}

#[cfg(test)]
mod tests {
    use super::{Access, DependencyGraph};
    use crate::{
        catalog::TableId,
        storage::RowId,
        txn::{CommandId, CommitSeq, Xid},
    };

    #[test]
    fn tracks_overlapping_row_and_relation_antidependencies() {
        let mut graph = DependencyGraph::default();
        let (reader, writer, table) = (Xid(1), Xid(2), TableId(1));
        graph.begin(reader);
        graph.set_snapshot(reader, CommitSeq(0), true);
        graph.read(reader, Access::Relation(table));
        graph.begin(writer);
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        assert!(graph.has_edge(reader, writer));
        graph.commit(writer, CommitSeq(1));
        assert!(graph.has_edge(reader, writer));
        graph.commit(reader, CommitSeq(1));
        assert!(graph.transactions.is_empty());
    }

    #[test]
    fn excludes_aborted_and_nonoverlapping_writers() {
        let mut graph = DependencyGraph::default();
        let (reader, writer, table) = (Xid(1), Xid(2), TableId(1));
        graph.begin(writer);
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        graph.commit(writer, CommitSeq(1));
        graph.begin(reader);
        graph.set_snapshot(reader, CommitSeq(1), true);
        graph.read(reader, Access::Row(table, RowId(1)));
        assert!(!graph.has_edge(reader, writer));
        graph.begin(Xid(3));
        graph.replace_table_writes(
            Xid(3),
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        assert!(graph.has_edge(reader, Xid(3)));
        graph.abort(Xid(3));
        assert!(!graph.has_edge(reader, Xid(3)));
    }

    #[test]
    fn keeps_reads_but_removes_rolled_back_savepoint_writes() {
        let mut graph = DependencyGraph::default();
        let (reader, writer, table) = (Xid(1), Xid(2), TableId(1));
        graph.begin(reader);
        graph.set_snapshot(reader, CommitSeq(0), true);
        graph.read(reader, Access::Row(table, RowId(1)));
        graph.begin(writer);
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(3), Access::Row(table, RowId(1)))].into(),
        );
        assert!(graph.has_edge(reader, writer));
        graph.replace_table_writes(writer, table, Default::default());
        assert!(!graph.has_edge(reader, writer));
        assert!(
            graph.transactions[&reader]
                .reads
                .contains(&Access::Row(table, RowId(1)))
        );
    }

    #[test]
    fn does_not_record_nonserializable_reads() {
        let mut graph = DependencyGraph::default();
        let (reader, writer, table) = (Xid(1), Xid(2), TableId(1));
        graph.begin(reader);
        graph.set_snapshot(reader, CommitSeq(0), false);
        graph.read(reader, Access::Relation(table));
        graph.begin(writer);
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        assert!(graph.transactions[&reader].reads.is_empty());
        assert!(!graph.has_edge(reader, writer));
    }

    #[test]
    fn retains_committed_reader_while_an_older_writer_is_active() {
        let mut graph = DependencyGraph::default();
        let (reader, writer, table) = (Xid(1), Xid(2), TableId(1));
        graph.begin(writer);
        graph.begin(reader);
        graph.set_snapshot(reader, CommitSeq(0), true);
        graph.read(reader, Access::Row(table, RowId(1)));
        graph.commit(reader, CommitSeq(0));
        assert!(graph.transactions.contains_key(&reader));
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        assert!(graph.has_edge(reader, writer));
        graph.commit(writer, CommitSeq(1));
        assert!(graph.transactions.is_empty());
    }

    #[test]
    fn distinguishes_row_reads_from_scans_across_small_schedules() {
        for read_row in 0..8 {
            for written_row in 0..8 {
                let mut graph = DependencyGraph::default();
                let (reader, writer, table) = (Xid(1), Xid(2), TableId(1));
                graph.begin(reader);
                graph.set_snapshot(reader, CommitSeq(0), true);
                graph.read(reader, Access::Row(table, RowId(read_row)));
                graph.begin(writer);
                graph.replace_table_writes(
                    writer,
                    table,
                    [(CommandId(0), Access::Row(table, RowId(written_row)))].into(),
                );
                assert_eq!(graph.has_edge(reader, writer), read_row == written_row);
                graph.read(reader, Access::Relation(table));
                assert!(graph.has_edge(reader, writer));
            }
        }
    }

    #[test]
    fn rejects_a_pivot_only_after_its_outgoing_writer_commits() {
        let mut graph = DependencyGraph::default();
        let (first, pivot, last, table) = (Xid(1), Xid(2), Xid(3), TableId(1));
        for xid in [first, pivot, last] {
            graph.begin(xid);
            graph.set_snapshot(xid, CommitSeq(0), true);
        }
        graph.read(first, Access::Row(table, RowId(1)));
        graph.read(pivot, Access::Row(table, RowId(2)));
        graph.replace_table_writes(
            first,
            table,
            [(CommandId(0), Access::Row(table, RowId(3)))].into(),
        );
        graph.replace_table_writes(
            pivot,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        graph.replace_table_writes(
            last,
            table,
            [(CommandId(0), Access::Row(table, RowId(2)))].into(),
        );
        assert!(!graph.would_fail_on_commit(pivot));
        graph.commit(last, CommitSeq(1));
        assert!(graph.would_fail_on_commit(pivot));
    }

    #[test]
    fn catches_an_incoming_read_discovered_after_the_pivot_commits() {
        let mut graph = DependencyGraph::default();
        let (reader, pivot, writer, table) = (Xid(1), Xid(2), Xid(3), TableId(1));
        for xid in [reader, pivot, writer] {
            graph.begin(xid);
            graph.set_snapshot(xid, CommitSeq(0), true);
        }
        graph.read(pivot, Access::Row(table, RowId(2)));
        graph.replace_table_writes(
            pivot,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(0), Access::Row(table, RowId(2)))].into(),
        );
        graph.commit(writer, CommitSeq(1));
        graph.commit(pivot, CommitSeq(2));
        graph.read(reader, Access::Row(table, RowId(1)));
        graph.replace_table_writes(
            reader,
            table,
            [(CommandId(0), Access::Row(table, RowId(3)))].into(),
        );
        assert!(graph.would_fail_on_commit(reader));
    }

    #[test]
    fn excludes_nonserializable_outgoing_writers_from_validation() {
        let mut graph = DependencyGraph::default();
        let (reader, pivot, writer, table) = (Xid(1), Xid(2), Xid(3), TableId(1));
        for xid in [reader, pivot, writer] {
            graph.begin(xid);
            graph.set_snapshot(xid, CommitSeq(0), xid != writer);
        }
        graph.read(reader, Access::Row(table, RowId(1)));
        graph.read(pivot, Access::Row(table, RowId(2)));
        graph.replace_table_writes(
            pivot,
            table,
            [(CommandId(0), Access::Row(table, RowId(1)))].into(),
        );
        graph.replace_table_writes(
            writer,
            table,
            [(CommandId(0), Access::Row(table, RowId(2)))].into(),
        );
        graph.commit(writer, CommitSeq(1));
        assert!(!graph.would_fail_on_commit(pivot));
    }
}
