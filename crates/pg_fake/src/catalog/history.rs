use super::{
    Catalog, CatalogRelations, DEFAULT_SCHEMA, FunctionId, FunctionSchema, Schema, SchemaId,
    SequenceId, SequenceSchema, TEMP_SCHEMA, TableId, TableSchema, ViewId, ViewSchema,
};
use crate::txn::{CommandId, CommitSeq, Snapshot, TransactionRegistry, TransactionStatus, Xid};
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaIdentity {
    id: SchemaId,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogVersion<T> {
    xmin: Option<Xid>,
    xmin_command_id: CommandId,
    xmax: Option<Xid>,
    xmax_command_id: Option<CommandId>,
    value: T,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CatalogVisibility {
    Current(u64),
    Snapshot {
        generation: u64,
        pruning_generation: u64,
        xid: Option<Xid>,
        snapshot: Snapshot,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogHistory {
    generation: u64,
    pruning_generation: u64,
    pending_transactions: BTreeMap<Xid, CommandId>,
    latest_commit: CommitSeq,
    schemas: BTreeMap<SchemaId, Vec<CatalogVersion<SchemaIdentity>>>,
    tables: BTreeMap<TableId, Vec<CatalogVersion<Arc<TableSchema>>>>,
    sequences: BTreeMap<SequenceId, Vec<CatalogVersion<Arc<SequenceSchema>>>>,
    views: BTreeMap<ViewId, Vec<CatalogVersion<Arc<ViewSchema>>>>,
    functions: BTreeMap<FunctionId, Vec<CatalogVersion<Arc<FunctionSchema>>>>,
    next_schema_id: u64,
    next_table_id: u64,
    next_sequence_id: u64,
    next_constraint_id: u64,
    next_index_id: u64,
    next_view_id: u64,
    next_trigger_id: u64,
    next_function_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimedCatalogObjects {
    pub(crate) tables: Vec<TableId>,
    pub(crate) sequences: Vec<SequenceId>,
}

impl CatalogHistory {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        let schema = SchemaIdentity {
            id: SchemaId(1),
            name: DEFAULT_SCHEMA.into(),
        };
        CatalogHistory {
            generation: 0,
            pruning_generation: 0,
            pending_transactions: BTreeMap::new(),
            latest_commit: CommitSeq(0),
            schemas: BTreeMap::from([(
                schema.id,
                vec![CatalogVersion {
                    xmin: None,
                    xmin_command_id: CommandId(0),
                    xmax: None,
                    xmax_command_id: None,
                    value: schema,
                }],
            )]),
            tables: BTreeMap::new(),
            sequences: BTreeMap::new(),
            views: BTreeMap::new(),
            next_schema_id: 2,
            next_table_id: 1,
            next_sequence_id: 1,
            next_constraint_id: 1,
            next_index_id: 1,
            next_view_id: 1,
            next_trigger_id: 1,
            next_function_id: 1,
            functions: BTreeMap::new(),
        }
    }

    pub(crate) fn create_temporary_schema_id(&mut self) -> SchemaId {
        self.generation += 1;
        let id = SchemaId(self.next_schema_id);
        self.next_schema_id += 1;
        id
    }

    pub(crate) fn resolve_visibility(
        &mut self,
        xid: Option<Xid>,
        snapshot: Snapshot,
        transactions: &TransactionRegistry,
    ) -> CatalogVisibility {
        self.pending_transactions
            .retain(|pending, _| match transactions.get_status(*pending) {
                Some(TransactionStatus::InFlight) => true,
                Some(TransactionStatus::Committed(commit)) => {
                    self.latest_commit = self.latest_commit.max(commit);
                    self.generation += 1;
                    false
                }
                Some(TransactionStatus::Aborted) | None => {
                    self.generation += 1;
                    false
                }
            });
        if self.pending_transactions.is_empty()
            && snapshot.commit_seq >= self.latest_commit
            && xid
                .is_none_or(|xid| transactions.get_status(xid) == Some(TransactionStatus::InFlight))
        {
            return CatalogVisibility::Current(self.generation);
        }
        let mut snapshot = snapshot;
        match xid {
            None => snapshot.command_id = CommandId(0),
            Some(xid) if transactions.get_status(xid) == Some(TransactionStatus::InFlight) => {
                snapshot.command_id =
                    self.pending_transactions
                        .get(&xid)
                        .map_or(CommandId(0), |command| {
                            snapshot
                                .command_id
                                .min(CommandId(command.0.saturating_add(1)))
                        });
            }
            Some(_) => {}
        }
        CatalogVisibility::Snapshot {
            generation: self.generation,
            pruning_generation: self.pruning_generation,
            xid,
            snapshot,
        }
    }

    pub(crate) fn can_reuse_after_commit(&self, xid: Xid, snapshot: Snapshot) -> bool {
        self.pending_transactions.len() == 1
            && self.pending_transactions.contains_key(&xid)
            && snapshot.commit_seq >= self.latest_commit
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn materialize(
        &self,
        xid: Option<Xid>,
        snapshot: Snapshot,
        transactions: &TransactionRegistry,
    ) -> Catalog {
        self.materialize_for_session(xid, snapshot, transactions, None)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn materialize_for_session(
        &self,
        xid: Option<Xid>,
        snapshot: Snapshot,
        transactions: &TransactionRegistry,
        temporary_schema_id: Option<SchemaId>,
    ) -> Catalog {
        let mut schemas = self
            .schemas
            .values()
            .filter_map(|versions| {
                find_visible_catalog_version(versions, xid, snapshot, transactions)
            })
            .map(|schema| {
                (
                    schema.name.clone(),
                    Schema {
                        id: schema.id,
                        name: schema.name.clone(),
                        tables: BTreeMap::new(),
                        views: BTreeMap::new(),
                        sequences: BTreeMap::new(),
                        functions: BTreeMap::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(id) = temporary_schema_id {
            let previous = schemas.insert(
                TEMP_SCHEMA.into(),
                Schema {
                    id,
                    name: TEMP_SCHEMA.into(),
                    tables: BTreeMap::new(),
                    views: BTreeMap::new(),
                    sequences: BTreeMap::new(),
                    functions: BTreeMap::new(),
                },
            );
            assert!(previous.is_none(), "temporary schema name must be reserved");
        }
        assert!(
            schemas.contains_key(DEFAULT_SCHEMA),
            "the public schema must remain visible"
        );
        let mut catalog = Catalog {
            relations: Arc::new(CatalogRelations {
                schemas,
                deferrable_foreign_keys: Vec::new(),
                referencing_foreign_keys: BTreeMap::new(),
            }),
            next_schema_id: self.next_schema_id,
            next_table_id: self.next_table_id,
            next_sequence_id: self.next_sequence_id,
            next_constraint_id: self.next_constraint_id,
            next_index_id: self.next_index_id,
            next_view_id: self.next_view_id,
            next_trigger_id: self.next_trigger_id,
            next_function_id: self.next_function_id,
        };
        for versions in self.tables.values() {
            let Some(table) = find_visible_catalog_version(versions, xid, snapshot, transactions)
            else {
                continue;
            };
            let Some(schema) = Arc::make_mut(&mut catalog.relations)
                .schemas
                .values_mut()
                .find(|schema| schema.id == table.schema_id)
            else {
                continue;
            };
            let previous = schema.tables.insert(table.name.clone(), table.clone());
            assert!(
                previous.is_none(),
                "visible catalog relation names must be unique"
            );
        }
        for versions in self.sequences.values() {
            let Some(sequence) =
                find_visible_catalog_version(versions, xid, snapshot, transactions)
            else {
                continue;
            };
            let Some(schema) = Arc::make_mut(&mut catalog.relations)
                .schemas
                .values_mut()
                .find(|schema| schema.id == sequence.schema_id)
            else {
                continue;
            };
            let previous = schema
                .sequences
                .insert(sequence.name.clone(), sequence.clone());
            assert!(
                previous.is_none(),
                "visible catalog relation names must be unique"
            );
        }
        for versions in self.views.values() {
            let Some(view) = find_visible_catalog_version(versions, xid, snapshot, transactions)
            else {
                continue;
            };
            let Some(schema) = Arc::make_mut(&mut catalog.relations)
                .schemas
                .values_mut()
                .find(|schema| schema.id == view.schema_id)
            else {
                continue;
            };
            let previous = schema.views.insert(view.name.clone(), view.clone());
            assert!(
                previous.is_none(),
                "visible catalog relation names must be unique"
            );
        }
        for versions in self.functions.values() {
            let Some(function) =
                find_visible_catalog_version(versions, xid, snapshot, transactions)
            else {
                continue;
            };
            let Some(schema) = Arc::make_mut(&mut catalog.relations)
                .schemas
                .values_mut()
                .find(|schema| schema.id == function.schema_id)
            else {
                continue;
            };
            let previous = schema
                .functions
                .insert(function.name.clone(), function.clone());
            assert!(
                previous.is_none(),
                "visible catalog function names must be unique"
            );
        }
        catalog.rebuild_foreign_key_metadata();
        catalog
    }

    pub(crate) fn find_trigger_dependencies(
        &self,
        function_id: FunctionId,
        xid: Xid,
        snapshot: Snapshot,
        transactions: &TransactionRegistry,
    ) -> Vec<TableId> {
        self.tables
            .iter()
            .filter_map(|(id, versions)| {
                find_visible_catalog_version(versions, Some(xid), snapshot, transactions)
                    .is_some_and(|table| {
                        table
                            .triggers
                            .iter()
                            .any(|trigger| trigger.function_id == function_id)
                    })
                    .then_some(*id)
            })
            .collect()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn record_changes(
        &mut self,
        previous: &Catalog,
        current: &Catalog,
        xid: Xid,
        command_id: CommandId,
    ) {
        self.generation += 1;
        self.pending_transactions
            .entry(xid)
            .and_modify(|last_command| *last_command = (*last_command).max(command_id))
            .or_insert(command_id);
        record_catalog_changes(
            &mut self.schemas,
            previous
                .relations
                .schemas
                .values()
                .map(|schema| {
                    (
                        schema.id,
                        Cow::<SchemaIdentity>::Owned(SchemaIdentity {
                            id: schema.id,
                            name: schema.name.clone(),
                        }),
                    )
                })
                .filter(|(_, schema)| schema.name != TEMP_SCHEMA),
            current
                .relations
                .schemas
                .values()
                .map(|schema| {
                    (
                        schema.id,
                        Cow::<SchemaIdentity>::Owned(SchemaIdentity {
                            id: schema.id,
                            name: schema.name.clone(),
                        }),
                    )
                })
                .filter(|(_, schema)| schema.name != TEMP_SCHEMA),
            xid,
            command_id,
        );
        record_catalog_changes(
            &mut self.tables,
            previous
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.tables.values())
                .map(|table| (table.id, Cow::Borrowed(table))),
            current
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.tables.values())
                .map(|table| (table.id, Cow::Borrowed(table))),
            xid,
            command_id,
        );
        record_catalog_changes(
            &mut self.views,
            previous
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.views.values())
                .map(|view| (view.id, Cow::Borrowed(view))),
            current
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.views.values())
                .map(|view| (view.id, Cow::Borrowed(view))),
            xid,
            command_id,
        );
        record_catalog_changes(
            &mut self.sequences,
            previous
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.sequences.values())
                .map(|sequence| (sequence.id, Cow::Borrowed(sequence))),
            current
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.sequences.values())
                .map(|sequence| (sequence.id, Cow::Borrowed(sequence))),
            xid,
            command_id,
        );
        record_catalog_changes(
            &mut self.functions,
            previous
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.functions.values())
                .map(|function| (function.id, Cow::Borrowed(function))),
            current
                .relations
                .schemas
                .values()
                .flat_map(|schema| schema.functions.values())
                .map(|function| (function.id, Cow::Borrowed(function))),
            xid,
            command_id,
        );
        self.next_schema_id = self.next_schema_id.max(current.next_schema_id);
        self.next_table_id = self.next_table_id.max(current.next_table_id);
        self.next_sequence_id = self.next_sequence_id.max(current.next_sequence_id);
        self.next_constraint_id = self.next_constraint_id.max(current.next_constraint_id);
        self.next_index_id = self.next_index_id.max(current.next_index_id);
        self.next_view_id = self.next_view_id.max(current.next_view_id);
        self.next_trigger_id = self.next_trigger_id.max(current.next_trigger_id);
        self.next_function_id = self.next_function_id.max(current.next_function_id);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn discard_transaction(&mut self, xid: Xid) -> ReclaimedCatalogObjects {
        self.generation += 1;
        self.pending_transactions.remove(&xid);
        discard_catalog_transaction(&mut self.schemas, xid);
        discard_catalog_transaction(&mut self.views, xid);
        discard_catalog_transaction(&mut self.functions, xid);
        ReclaimedCatalogObjects {
            tables: discard_catalog_transaction(&mut self.tables, xid),
            sequences: discard_catalog_transaction(&mut self.sequences, xid),
        }
    }

    pub(crate) fn drop_temporary_schema(
        &mut self,
        temporary_schema_id: SchemaId,
    ) -> ReclaimedCatalogObjects {
        self.generation += 1;
        let tables = self
            .tables
            .iter()
            .filter_map(|(id, versions)| {
                versions
                    .iter()
                    .any(|version| version.value.schema_id == temporary_schema_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        let sequences = self
            .sequences
            .iter()
            .filter_map(|(id, versions)| {
                versions
                    .iter()
                    .any(|version| version.value.schema_id == temporary_schema_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        let views = self
            .views
            .iter()
            .filter_map(|(id, versions)| {
                versions
                    .iter()
                    .any(|version| version.value.schema_id == temporary_schema_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        let functions = self
            .functions
            .iter()
            .filter_map(|(id, versions)| {
                versions
                    .iter()
                    .any(|version| version.value.schema_id == temporary_schema_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in &tables {
            self.tables.remove(id);
        }
        for id in &sequences {
            self.sequences.remove(id);
        }
        for id in views {
            self.views.remove(&id);
        }
        for id in functions {
            self.functions.remove(&id);
        }
        ReclaimedCatalogObjects { tables, sequences }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn prune(
        &mut self,
        horizon: CommitSeq,
        transactions: &TransactionRegistry,
        protected_tables: &std::collections::BTreeSet<TableId>,
    ) -> ReclaimedCatalogObjects {
        self.pruning_generation += 1;
        prune_catalog_versions(
            &mut self.schemas,
            horizon,
            transactions,
            &std::collections::BTreeSet::new(),
        );
        prune_catalog_versions(
            &mut self.views,
            horizon,
            transactions,
            &std::collections::BTreeSet::new(),
        );
        prune_catalog_versions(
            &mut self.functions,
            horizon,
            transactions,
            &std::collections::BTreeSet::new(),
        );
        ReclaimedCatalogObjects {
            tables: prune_catalog_versions(
                &mut self.tables,
                horizon,
                transactions,
                protected_tables,
            ),
            sequences: prune_catalog_versions(
                &mut self.sequences,
                horizon,
                transactions,
                &std::collections::BTreeSet::new(),
            ),
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn record_catalog_changes<'a, Id, T>(
    histories: &mut BTreeMap<Id, Vec<CatalogVersion<T>>>,
    previous: impl Iterator<Item = (Id, Cow<'a, T>)>,
    current: impl Iterator<Item = (Id, Cow<'a, T>)>,
    xid: Xid,
    command_id: CommandId,
) where
    Id: Copy + Ord,
    T: Clone + PartialEq + 'a,
{
    let previous = previous.collect::<BTreeMap<_, _>>();
    let current = current.collect::<BTreeMap<_, _>>();
    for (id, old) in &previous {
        if current.get(id).is_some_and(|new| new == old) {
            continue;
        }
        let version = histories
            .get_mut(id)
            .and_then(|versions| {
                versions
                    .iter_mut()
                    .rev()
                    .find(|version| version.xmax.is_none() && &version.value == old.as_ref())
            })
            .expect("materialized catalog object must have a live version");
        version.xmax = Some(xid);
        version.xmax_command_id = Some(command_id);
    }
    for (id, new) in current {
        if previous.get(&id).is_some_and(|old| old == &new) {
            continue;
        }
        histories.entry(id).or_default().push(CatalogVersion {
            xmin: Some(xid),
            xmin_command_id: command_id,
            xmax: None,
            xmax_command_id: None,
            value: new.into_owned(),
        });
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn discard_catalog_transaction<Id, T>(
    histories: &mut BTreeMap<Id, Vec<CatalogVersion<T>>>,
    xid: Xid,
) -> Vec<Id>
where
    Id: Copy + Ord,
{
    for versions in histories.values_mut() {
        versions.retain(|version| version.xmin != Some(xid));
        for version in versions {
            if version.xmax == Some(xid) {
                version.xmax = None;
                version.xmax_command_id = None;
            }
        }
    }
    let removed = histories
        .iter()
        .filter_map(|(id, versions)| versions.is_empty().then_some(*id))
        .collect::<Vec<_>>();
    histories.retain(|_, versions| !versions.is_empty());
    removed
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn prune_catalog_versions<Id, T>(
    histories: &mut BTreeMap<Id, Vec<CatalogVersion<T>>>,
    horizon: CommitSeq,
    transactions: &TransactionRegistry,
    protected: &std::collections::BTreeSet<Id>,
) -> Vec<Id>
where
    Id: Copy + Ord,
{
    for (id, versions) in histories.iter_mut() {
        let retained = versions
            .iter()
            .filter(|version| {
                !matches!(
                    version.xmax.and_then(|xmax| transactions.get_status(xmax)),
                    Some(TransactionStatus::Committed(commit_seq)) if commit_seq <= horizon
                )
            })
            .count();
        if retained != 0 || !protected.contains(id) {
            versions.retain(|version| {
                !matches!(
                    version.xmax.and_then(|xmax| transactions.get_status(xmax)),
                    Some(TransactionStatus::Committed(commit_seq)) if commit_seq <= horizon
                )
            });
        }
    }
    let removed = histories
        .iter()
        .filter_map(|(id, versions)| versions.is_empty().then_some(*id))
        .collect::<Vec<_>>();
    histories.retain(|_, versions| !versions.is_empty());
    removed
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn find_visible_catalog_version<'a, T>(
    versions: &'a [CatalogVersion<T>],
    xid: Option<Xid>,
    snapshot: Snapshot,
    transactions: &TransactionRegistry,
) -> Option<&'a T> {
    versions
        .iter()
        .rev()
        .find(|version| is_catalog_version_visible(version, xid, snapshot, transactions))
        .map(|version| &version.value)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn is_catalog_version_visible<T>(
    version: &CatalogVersion<T>,
    xid: Option<Xid>,
    snapshot: Snapshot,
    transactions: &TransactionRegistry,
) -> bool {
    let xmin_visible = match version.xmin {
        None => true,
        Some(xmin) if Some(xmin) == xid => version.xmin_command_id < snapshot.command_id,
        Some(xmin) => matches!(
            transactions.get_status(xmin),
            Some(TransactionStatus::Committed(commit_seq)) if commit_seq <= snapshot.commit_seq
        ),
    };
    let xmax_visible = match version.xmax {
        None => false,
        Some(xmax) if Some(xmax) == xid => version
            .xmax_command_id
            .is_some_and(|command_id| command_id < snapshot.command_id),
        Some(xmax) => matches!(
            transactions.get_status(xmax),
            Some(TransactionStatus::Committed(commit_seq)) if commit_seq <= snapshot.commit_seq
        ),
    };
    xmin_visible && !xmax_visible
}
