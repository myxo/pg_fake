use crate::{
    catalog::{Catalog, CatalogHistory, CatalogVisibility, TableId},
    executor::{SequenceStorage, SequenceValueState},
    storage::Table,
    txn::{
        CommandId, CommitSeq, RelationLockManager, RowLockManager, Snapshot, TransactionRegistry,
        WaitForGraph, Xid,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(crate) struct DatabaseState {
    loaded_catalog: Option<(CatalogVisibility, Option<crate::catalog::SchemaId>)>,
    inactive_catalogs: BTreeMap<Option<crate::catalog::SchemaId>, Catalog>,
    pub(crate) catalog: Catalog,
    pub(crate) catalog_history: CatalogHistory,
    pub(crate) tables: BTreeMap<TableId, Table>,
    pub(crate) transactions: TransactionRegistry,
    pub(crate) serializable: Arc<Mutex<crate::serializable::DependencyGraph>>,
    recent_read_tracker: Option<(Xid, bool)>,
    pub(crate) row_locks: RowLockManager,
    pub(crate) advisory_locks: Arc<Mutex<crate::advisory::AdvisoryLockManager>>,
    pub(crate) relation_locks: RelationLockManager,
    pub(crate) wait_for: WaitForGraph,
    pub(crate) sequence_values: SequenceStorage,
    sequence_resets:
        BTreeMap<Xid, Vec<(CommandId, crate::catalog::SequenceId, SequenceValueState)>>,
    touched_tables: BTreeMap<Xid, Vec<TableId>>,
    reclaimable_tables: Vec<TableId>,
}

#[derive(Clone)]
pub(crate) struct QuerySourceTable {
    pub(crate) table: Arc<Table>,
    pub(crate) transactions: Arc<TransactionRegistry>,
}

impl DatabaseState {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        let catalog_history = CatalogHistory::create();
        let transactions = TransactionRegistry::create();
        let catalog =
            catalog_history.materialize(None, Snapshot::create(&transactions), &transactions);
        DatabaseState {
            loaded_catalog: None,
            inactive_catalogs: BTreeMap::new(),
            catalog,
            catalog_history,
            tables: BTreeMap::new(),
            transactions,
            serializable: Default::default(),
            recent_read_tracker: None,
            row_locks: RowLockManager::create(),
            advisory_locks: Default::default(),
            relation_locks: RelationLockManager::create(),
            wait_for: WaitForGraph::create(),
            sequence_values: Arc::new(Mutex::new(BTreeMap::new())),
            sequence_resets: BTreeMap::new(),
            touched_tables: BTreeMap::new(),
            reclaimable_tables: Vec::new(),
        }
    }

    pub(crate) fn capture_query_source(&self, table_id: TableId) -> Option<QuerySourceTable> {
        let table = self.tables.get(&table_id)?;
        let xids = table
            .iterate_version_chains()
            .flat_map(|(_, chain)| {
                chain
                    .versions
                    .iter()
                    .flat_map(|version| std::iter::once(version.xmin).chain(version.xmax))
            })
            .collect();
        Some(QuerySourceTable {
            table: Arc::new(table.clone()),
            transactions: Arc::new(self.transactions.snapshot_statuses(&xids)),
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn load_catalog(
        &mut self,
        xid: Option<Xid>,
        snapshot: Snapshot,
        temporary_schema_id: Option<crate::catalog::SchemaId>,
    ) {
        let visibility = self
            .catalog_history
            .resolve_visibility(xid, snapshot, &self.transactions);
        let catalog_key = (visibility, temporary_schema_id);
        if self.loaded_catalog == Some(catalog_key) {
            return;
        }
        let previous_key = self.loaded_catalog.take();
        let same_visibility = previous_key.is_some_and(|(previous, _)| previous == visibility);
        if !same_visibility {
            self.inactive_catalogs.clear();
        }
        let cached = self.inactive_catalogs.remove(&temporary_schema_id);
        let cache_hit = cached.is_some();
        let search_path = self.catalog.search_path.clone();
        let mut catalog = cached.unwrap_or_else(|| {
            self.catalog_history.materialize_for_session(
                xid,
                snapshot,
                &self.transactions,
                temporary_schema_id,
            )
        });
        catalog.set_search_path(&search_path);
        let previous = std::mem::replace(&mut self.catalog, catalog);
        if same_visibility {
            let (_, previous_schema_id) =
                previous_key.expect("same visibility has a loaded catalog");
            self.inactive_catalogs.insert(previous_schema_id, previous);
        }
        if !cache_hit {
            for schema in self.catalog.iterate_shared_tables() {
                if let Some(table) = self.tables.get_mut(&schema.id) {
                    table.replace_schema(Arc::clone(schema));
                }
            }
        }
        self.loaded_catalog = Some(catalog_key);
    }

    pub(crate) fn commit_loaded_catalog_transaction(
        &mut self,
        xid: Xid,
        snapshot: Snapshot,
        temporary_schema_id: Option<crate::catalog::SchemaId>,
    ) -> CommitSeq {
        // The commit path has loaded and synchronized the final catalog under this lock.
        let reusable = self.catalog_history.can_reuse_after_commit(xid, snapshot);
        let commit_seq = self.transactions.commit(xid);
        if reusable {
            let visibility = self.catalog_history.resolve_visibility(
                None,
                Snapshot::create(&self.transactions),
                &self.transactions,
            );
            assert!(matches!(visibility, CatalogVisibility::Current(_)));
            self.inactive_catalogs.clear();
            self.loaded_catalog = Some((visibility, temporary_schema_id));
        }
        commit_seq
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn record_catalog_changes(
        &mut self,
        previous: &Catalog,
        xid: Xid,
        command_id: CommandId,
    ) {
        self.catalog_history
            .record_changes(previous, &self.catalog, xid, command_id);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn mark_table_touched(&mut self, xid: Xid, table_id: TableId) {
        let tables = self.touched_tables.entry(xid).or_default();
        if !tables.contains(&table_id) {
            tables.push(table_id);
        }
        if !self
            .serializable
            .lock()
            .expect("dependency graph is poisoned")
            .needs_write_tracking()
        {
            return;
        }
        let accesses = self
            .tables
            .get(&table_id)
            .map_or_else(Default::default, |table| {
                table.collect_transaction_accesses(xid)
            });
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .replace_table_writes(xid, table_id, accesses);
    }

    pub(crate) fn begin_statement_tracking(
        &mut self,
        xid: Xid,
        snapshot: Snapshot,
        serializable: bool,
    ) {
        let first_serializable_statement = self
            .serializable
            .lock()
            .expect("dependency graph is poisoned")
            .set_snapshot(xid, snapshot.commit_seq, serializable);
        self.recent_read_tracker = Some((xid, serializable));
        if first_serializable_statement {
            let touched = self
                .touched_tables
                .iter()
                .flat_map(|(&writer, tables)| tables.iter().map(move |&table| (writer, table)))
                .collect::<Vec<_>>();
            for (writer, table_id) in touched {
                let accesses = self
                    .tables
                    .get(&table_id)
                    .map_or_else(Default::default, |table| {
                        table.collect_transaction_accesses(writer)
                    });
                self.serializable
                    .lock()
                    .expect("dependency graph is poisoned")
                    .replace_table_writes(writer, table_id, accesses);
            }
        }
    }

    pub(crate) fn record_read(&self, xid: Xid, access: crate::serializable::Access) {
        if self.recent_read_tracker == Some((xid, false)) {
            return;
        }
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .read(xid, access);
    }

    pub(crate) fn tracks_serializable_reads(&self, xid: Xid) -> bool {
        if let Some((tracked, serializable)) = self.recent_read_tracker
            && tracked == xid
        {
            return serializable;
        }
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .is_serializable(xid)
    }

    pub(crate) fn uses_serializable_snapshot(&self, xid: Xid) -> bool {
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .is_serializable(xid)
    }

    pub(crate) fn has_serialization_failure(&self, xid: Xid) -> bool {
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .would_fail_on_commit(xid)
    }

    pub(crate) fn collect_serialization_edges(&self, xid: Xid) -> BTreeSet<(Xid, Xid)> {
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .collect_transaction_edges(xid)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_touched_tables(&self, xid: Xid) -> bool {
        self.touched_tables
            .get(&xid)
            .is_some_and(|tables| !tables.is_empty())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn take_touched_tables(&mut self, xid: Xid) -> Vec<TableId> {
        self.touched_tables.remove(&xid).unwrap_or_default()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn collect_touched_tables(&self) -> BTreeSet<TableId> {
        self.touched_tables.values().flatten().copied().collect()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn mark_table_reclaimable(&mut self, table_id: TableId) {
        if !self.reclaimable_tables.contains(&table_id) {
            self.reclaimable_tables.push(table_id);
        }
    }

    pub(crate) fn reset_sequence_transactionally(
        &mut self,
        xid: Xid,
        sequence: &crate::catalog::SequenceSchema,
        command_id: CommandId,
    ) {
        let mut values = self
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");
        let value = values
            .get_mut(&sequence.id)
            .expect("catalog sequence must have storage");
        self.sequence_resets
            .entry(xid)
            .or_default()
            .push((command_id, sequence.id, *value));
        *value = SequenceValueState {
            last_value: sequence.start_value,
            is_called: false,
        };
    }

    pub(crate) fn commit_sequence_resets(&mut self, xid: Xid) {
        self.sequence_resets.remove(&xid);
    }

    pub(crate) fn abort_sequence_resets(&mut self, xid: Xid) {
        self.rollback_sequence_resets_since(xid, CommandId(0));
    }

    pub(crate) fn rollback_sequence_resets_since(&mut self, xid: Xid, boundary: CommandId) {
        let Some(resets) = self.sequence_resets.get_mut(&xid) else {
            return;
        };
        let mut values = self
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");
        while resets
            .last()
            .is_some_and(|(command, _, _)| *command >= boundary)
        {
            let (_, sequence, value) = resets.pop().expect("selected reset exists");
            values.insert(sequence, value);
        }
        if resets.is_empty() {
            self.sequence_resets.remove(&xid);
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn collect_reclaimable_table_ids(&self) -> Vec<TableId> {
        self.reclaimable_tables.clone()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn clear_table_reclaimable(&mut self, table_id: TableId) {
        self.reclaimable_tables
            .retain(|candidate| *candidate != table_id);
    }
}

impl DatabaseState {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn abort_transaction(&mut self, xid: Xid) {
        self.serializable
            .lock()
            .expect("dependency graph is poisoned")
            .abort(xid);
        self.abort_sequence_resets(xid);
        let reclaimed = self.catalog_history.discard_transaction(xid);
        for table_id in reclaimed.tables {
            self.tables.remove(&table_id);
        }
        let mut sequence_values = self
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");
        for sequence_id in reclaimed.sequences {
            sequence_values.remove(&sequence_id);
        }
        drop(sequence_values);
        self.transactions.abort(xid);
        for table_id in self.take_touched_tables(xid) {
            if let Some(table) = self.tables.get_mut(&table_id) {
                table.discard_transaction_versions(xid);
            }
        }
        self.prune_versions();
        self.row_locks.release_transaction_locks(xid);
        self.advisory_locks
            .lock()
            .expect("advisory lock mutex is poisoned")
            .release_transaction_locks(xid);
        self.relation_locks.release_transaction_locks(xid);
        self.wait_for.remove_transaction(xid);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn prune_versions(&mut self) {
        let horizon = self.transactions.find_reclamation_horizon();
        for table_id in self.collect_reclaimable_table_ids() {
            let Some(table) = self.tables.get_mut(&table_id) else {
                self.clear_table_reclaimable(table_id);
                continue;
            };
            table.prune_versions(horizon, &self.transactions);
            if !table.has_reclaimable_versions() {
                self.clear_table_reclaimable(table_id);
            }
        }
        let protected_tables = self.collect_touched_tables();
        let reclaimed = self
            .catalog_history
            .prune(horizon, &self.transactions, &protected_tables);
        for table_id in reclaimed.tables {
            self.tables.remove(&table_id);
        }
        let mut sequence_values = self
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned");
        for sequence_id in reclaimed.sequences {
            sequence_values.remove(&sequence_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DatabaseState;
    use crate::{
        catalog::TableId,
        serializable::Access,
        storage::RowId,
        txn::{CommandId, Snapshot},
    };

    #[test]
    fn preserves_serializable_reads_after_tracking_hint_changes() {
        let mut state = DatabaseState::create();
        let reader = state.transactions.begin();
        let writer = state.transactions.begin();
        {
            let mut graph = state.serializable.lock().unwrap();
            graph.begin(reader);
            graph.begin(writer);
        }
        let snapshot = Snapshot::create(&state.transactions);
        state.begin_statement_tracking(reader, snapshot, true);
        state.begin_statement_tracking(writer, snapshot, false);

        let table = TableId(1);
        assert!(state.tracks_serializable_reads(reader));
        assert!(!state.tracks_serializable_reads(writer));
        state.record_read(reader, Access::Relation(table));
        state.serializable.lock().unwrap().replace_table_writes(
            writer,
            table,
            std::collections::BTreeSet::from([(CommandId(0), Access::Row(table, RowId(1)))]),
        );
        assert!(state.serializable.lock().unwrap().has_edge(reader, writer));
    }
}
