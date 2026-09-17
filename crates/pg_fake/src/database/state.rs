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
    pub(crate) row_locks: RowLockManager,
    pub(crate) advisory_locks: Arc<Mutex<crate::advisory::AdvisoryLockManager>>,
    pub(crate) relation_locks: RelationLockManager,
    pub(crate) wait_for: WaitForGraph,
    pub(crate) sequence_values: SequenceStorage,
    sequence_resets: BTreeMap<Xid, BTreeMap<crate::catalog::SequenceId, SequenceValueState>>,
    touched_tables: BTreeMap<Xid, Vec<TableId>>,
    reclaimable_tables: Vec<TableId>,
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
            .entry(sequence.id)
            .or_insert(*value);
        *value = SequenceValueState {
            last_value: sequence.start_value,
            is_called: false,
        };
    }

    pub(crate) fn commit_sequence_resets(&mut self, xid: Xid) {
        self.sequence_resets.remove(&xid);
    }

    pub(crate) fn abort_sequence_resets(&mut self, xid: Xid) {
        let Some(restored) = self.sequence_resets.remove(&xid) else {
            return;
        };
        self.sequence_values
            .lock()
            .expect("sequence storage is poisoned")
            .extend(restored);
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
