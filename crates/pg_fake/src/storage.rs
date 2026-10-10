use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    catalog::{Constraint, IndexSchema, TableSchema},
    executor::StatementContext,
    txn::{
        CommandId, CommitSeq, Snapshot, TransactionRegistry, TransactionStatus, Xid,
        find_visible_version,
    },
    value::{BaseType, Value},
};
use sqlparser::ast;

pub(crate) type Row = Vec<Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RowId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowVersion {
    pub(crate) xmin: Xid,
    pub(crate) xmin_command_id: CommandId,
    pub(crate) xmax: Option<Xid>,
    pub(crate) xmax_command_id: Option<CommandId>,
    pub(crate) row: Row,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowVersionChain {
    pub(crate) versions: Vec<RowVersion>,
}

#[derive(Debug, Clone, PartialEq)]
struct VersionChainStore {
    chains: BTreeMap<RowId, RowVersionChain>,
}

#[derive(Debug, Clone, PartialEq)]
struct VersionReclamation {
    touched: BTreeMap<Xid, BTreeSet<RowId>>,
    pending: BTreeMap<Xid, BTreeSet<RowId>>,
    committed: BTreeMap<CommitSeq, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq)]
struct TruncatedStorage {
    command_id: CommandId,
    version_chains: VersionChainStore,
    reclamation: VersionReclamation,
}

fn discard_transaction_versions(
    store: &mut VersionChainStore,
    reclamation: &mut VersionReclamation,
    touched: &BTreeSet<RowId>,
    xid: Xid,
    boundary: CommandId,
) {
    for row_id in touched {
        let chain = store
            .chains
            .get_mut(row_id)
            .expect("touched row must have a version chain");
        chain
            .versions
            .retain(|version| version.xmin != xid || version.xmin_command_id < boundary);
        for version in &mut chain.versions {
            if version.xmax == Some(xid)
                && version
                    .xmax_command_id
                    .is_some_and(|command| command >= boundary)
            {
                version.xmax = None;
                version.xmax_command_id = None;
            }
        }
        if chain.versions.is_empty() {
            store.chains.remove(row_id);
        }
    }
    if let Some(rows) = reclamation.pending.get_mut(&xid) {
        rows.retain(|row| {
            store.chains.get(row).is_some_and(|chain| {
                chain
                    .versions
                    .iter()
                    .any(|version| version.xmin == xid || version.xmax == Some(xid))
            })
        });
        if rows.is_empty() {
            reclamation.pending.remove(&xid);
        }
    }
    if let Some(rows) = reclamation.touched.get_mut(&xid) {
        rows.retain(|row| {
            store.chains.get(row).is_some_and(|chain| {
                chain
                    .versions
                    .iter()
                    .any(|version| version.xmin == xid || version.xmax == Some(xid))
            })
        });
        if rows.is_empty() {
            reclamation.touched.remove(&xid);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NormalizedIndexValue {
    Jsonb(crate::jsonb::Jsonb),
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Float4(u32),
    Float8(u64),
    Numeric(bigdecimal::BigDecimal),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(uuid::Uuid),
    Date(crate::value::PgDate),
    Time(crate::value::PgTime),
    Timestamp(crate::value::PgTimestamp),
    TimestampTz(crate::value::PgTimestampTz),
    Interval(crate::value::PgInterval),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct IndexKey(Vec<NormalizedIndexValue>);

#[derive(Debug, Clone, PartialEq)]
struct TableIndex {
    name: String,
    unique: bool,
    columns: Vec<usize>,
    predicate: Option<ast::Expr>,
    entries: BTreeMap<IndexKey, BTreeSet<RowId>>,
}

#[derive(Default)]
pub(crate) struct PendingUniqueChanges {
    entries: Vec<BTreeMap<IndexKey, BTreeSet<RowId>>>,
    row_keys: BTreeMap<RowId, Vec<Option<IndexKey>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub(crate) schema: Arc<TableSchema>,
    version_chains: VersionChainStore,
    indexes: Vec<TableIndex>,
    reclamation: Box<VersionReclamation>,
    truncated: BTreeMap<Xid, Vec<TruncatedStorage>>,
    next_rowid: u64,
}

impl Table {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create(schema: TableSchema) -> Self {
        let indexes = schema
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::PrimaryKey { name, columns, .. }
                | Constraint::Unique { name, columns, .. } => Some(TableIndex {
                    name: name.clone(),
                    unique: true,
                    columns: columns
                        .iter()
                        .map(|name| {
                            schema
                                .columns
                                .iter()
                                .position(|column| &column.name == name)
                                .expect("constraint columns must exist")
                        })
                        .collect(),
                    predicate: None,
                    entries: BTreeMap::new(),
                }),
                Constraint::Check { .. } | Constraint::ForeignKey(_) => None,
            })
            .chain(
                schema
                    .indexes
                    .iter()
                    .map(|index| create_index(&schema, index)),
            )
            .collect();
        Table {
            schema: Arc::new(schema),
            version_chains: VersionChainStore {
                chains: BTreeMap::new(),
            },
            indexes,
            reclamation: Box::new(VersionReclamation {
                touched: BTreeMap::new(),
                pending: BTreeMap::new(),
                committed: BTreeMap::new(),
            }),
            truncated: BTreeMap::new(),
            next_rowid: 1,
        }
    }

    pub(crate) fn replace_schema(&mut self, schema: Arc<TableSchema>) {
        assert_eq!(self.schema.id, schema.id);
        if Arc::ptr_eq(&self.schema, &schema) {
            return;
        }
        let unchanged = self.schema == schema;
        self.schema = schema;
        if unchanged {
            return;
        }
        self.indexes = self
            .schema
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::PrimaryKey { name, columns, .. }
                | Constraint::Unique { name, columns, .. } => Some(TableIndex {
                    name: name.clone(),
                    unique: true,
                    columns: columns
                        .iter()
                        .map(|name| {
                            self.schema
                                .columns
                                .iter()
                                .position(|column| &column.name == name)
                                .expect("constraint columns must exist")
                        })
                        .collect(),
                    predicate: None,
                    entries: BTreeMap::new(),
                }),
                Constraint::Check { .. } | Constraint::ForeignKey(_) => None,
            })
            .chain(
                self.schema
                    .indexes
                    .iter()
                    .map(|index| create_index(&self.schema, index)),
            )
            .collect();
        self.rebuild_indexes();
    }

    pub(crate) fn collect_visible_versions(
        &self,
        snapshot: &Snapshot,
        xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Vec<(RowId, RowVersion)> {
        self.version_chains
            .chains
            .iter()
            .filter_map(|(row_id, chain)| {
                find_visible_version(chain, snapshot, xid, transactions)
                    .cloned()
                    .map(|version| (*row_id, version))
            })
            .collect()
    }

    pub(crate) fn create_unique_read_key(
        &self,
        columns: &[usize],
        values: &[Value],
    ) -> Option<IndexKey> {
        self.has_unique_index(columns)
            .then(|| build_index_key(&self.schema, columns, values))
            .flatten()
    }

    pub(crate) fn find_nonunique_visible_versions(
        &self,
        columns: &[usize],
        values: &[Value],
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Vec<(RowId, &RowVersion)> {
        let index = self
            .indexes
            .iter()
            .find(|index| !index.unique && index.columns == columns && index.predicate.is_none())
            .expect("nonunique lookup requires a matching index");
        let Some(key) = build_index_key(&self.schema, columns, values) else {
            return Vec::new();
        };
        index.entries.get(&key).map_or_else(Vec::new, |row_ids| {
            row_ids
                .iter()
                .filter_map(|row_id| {
                    let version = self.version_chains.chains.get(row_id).and_then(|chain| {
                        find_visible_version(chain, snapshot, current_xid, transactions)
                    })?;
                    (build_row_index_key(&self.schema, index, &version.row).as_ref() == Some(&key))
                        .then_some((*row_id, version))
                })
                .collect()
        })
    }

    pub(crate) fn has_nonunique_index(&self, columns: &[usize]) -> bool {
        self.indexes
            .iter()
            .any(|index| !index.unique && index.columns == columns && index.predicate.is_none())
    }

    pub(crate) fn collect_transaction_accesses(
        &self,
        xid: Xid,
    ) -> BTreeSet<(CommandId, crate::serializable::Access)> {
        use crate::serializable::Access;

        let mut accesses = BTreeSet::new();
        for (row_id, chain) in &self.version_chains.chains {
            for version in &chain.versions {
                let command = if version.xmin == xid {
                    Some(version.xmin_command_id)
                } else if version.xmax == Some(xid) {
                    version.xmax_command_id
                } else {
                    None
                };
                let Some(command) = command else { continue };
                accesses.insert((command, Access::Row(self.schema.id, *row_id)));
                for index in self.indexes.iter().filter(|index| index.unique) {
                    if let Some(key) = build_row_index_key(&self.schema, index, &version.row) {
                        accesses.insert((
                            command,
                            Access::Unique(self.schema.id, index.columns.clone(), key),
                        ));
                    }
                }
            }
        }
        for (transaction, truncated) in &self.truncated {
            if *transaction == xid {
                for storage in truncated {
                    accesses.insert((storage.command_id, Access::Relation(self.schema.id)));
                }
            }
        }
        accesses
    }

    pub(crate) fn truncate_all(&mut self, xid: Xid, command_id: CommandId) {
        let storage = TruncatedStorage {
            command_id,
            version_chains: std::mem::replace(
                &mut self.version_chains,
                VersionChainStore {
                    chains: BTreeMap::new(),
                },
            ),
            reclamation: std::mem::replace(
                self.reclamation.as_mut(),
                VersionReclamation {
                    touched: BTreeMap::new(),
                    pending: BTreeMap::new(),
                    committed: BTreeMap::new(),
                },
            ),
        };
        self.truncated.entry(xid).or_default().push(storage);
        self.rebuild_indexes();
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn insert(&mut self, xmin: Xid, command_id: CommandId, row: Row) -> RowId {
        let row_id = RowId(self.next_rowid);
        self.next_rowid += 1;
        self.reclamation
            .touched
            .entry(xmin)
            .or_default()
            .insert(row_id);
        self.add_index_entries(row_id, &row, None);
        let previous = self.version_chains.chains.insert(
            row_id,
            RowVersionChain {
                versions: vec![RowVersion {
                    xmin,
                    xmin_command_id: command_id,
                    xmax: None,
                    xmax_command_id: None,
                    row,
                }],
            },
        );
        assert!(previous.is_none());
        row_id
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn mark_version_deleted(
        &mut self,
        row_id: RowId,
        version_xmin: Xid,
        xmax: Xid,
        command_id: CommandId,
    ) -> RowId {
        let chain = self
            .version_chains
            .chains
            .get_mut(&row_id)
            .expect("row must exist");
        let version = chain
            .versions
            .iter_mut()
            .rev()
            .find(|version| version.xmin == version_xmin && version.xmax.is_none())
            .expect("live version with xmin must exist");
        version.xmax = Some(xmax);
        version.xmax_command_id = Some(command_id);
        self.reclamation
            .pending
            .entry(xmax)
            .or_default()
            .insert(row_id);
        self.reclamation
            .touched
            .entry(xmax)
            .or_default()
            .insert(row_id);
        row_id
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn append_updated_version(
        &mut self,
        row_id: RowId,
        version_xmin: Xid,
        xmin: Xid,
        command_id: CommandId,
        row: Row,
        changed_columns: Option<&BTreeSet<usize>>,
    ) -> RowId {
        self.mark_version_deleted(row_id, version_xmin, xmin, command_id);
        self.add_index_entries(row_id, &row, changed_columns);
        self.version_chains
            .chains
            .get_mut(&row_id)
            .expect("row must exist")
            .versions
            .push(RowVersion {
                xmin,
                xmin_command_id: command_id,
                xmax: None,
                xmax_command_id: None,
                row,
            });
        row_id
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn discard_transaction_versions(&mut self, xid: Xid) {
        self.discard_versions_since(xid, CommandId(0));
    }

    pub(crate) fn discard_versions_since(&mut self, xid: Xid, boundary: CommandId) {
        let mut restored_truncate = false;
        if let Some(storages) = self.truncated.get_mut(&xid) {
            while storages
                .last()
                .is_some_and(|storage| storage.command_id >= boundary)
            {
                let storage = storages.pop().expect("selected truncate exists");
                self.version_chains = storage.version_chains;
                *self.reclamation = storage.reclamation;
                restored_truncate = true;
            }
            if storages.is_empty() {
                self.truncated.remove(&xid);
            }
        }
        let touched = self
            .reclamation
            .touched
            .get(&xid)
            .cloned()
            .unwrap_or_default();
        let previous_keys = if restored_truncate {
            Vec::new()
        } else {
            touched
                .iter()
                .map(|row_id| {
                    let chain = self
                        .version_chains
                        .chains
                        .get(row_id)
                        .expect("touched row must have a version chain");
                    let keys = self
                        .indexes
                        .iter()
                        .map(|index| {
                            chain
                                .versions
                                .iter()
                                .filter_map(|version| {
                                    build_row_index_key(&self.schema, index, &version.row)
                                })
                                .collect::<BTreeSet<_>>()
                        })
                        .collect::<Vec<_>>();
                    (*row_id, keys)
                })
                .collect::<Vec<_>>()
        };
        discard_transaction_versions(
            &mut self.version_chains,
            &mut self.reclamation,
            &touched,
            xid,
            boundary,
        );
        if restored_truncate {
            self.rebuild_indexes();
            return;
        }
        for (row_id, keys) in previous_keys {
            let chain = self.version_chains.chains.get(&row_id);
            for (index, previous) in self.indexes.iter_mut().zip(keys) {
                let retained = chain
                    .into_iter()
                    .flat_map(|chain| chain.versions.iter())
                    .filter_map(|version| build_row_index_key(&self.schema, index, &version.row))
                    .collect::<BTreeSet<_>>();
                for key in previous.difference(&retained) {
                    let row_ids = index
                        .entries
                        .get_mut(key)
                        .expect("rolled-back index entry must exist");
                    assert!(row_ids.remove(&row_id));
                    if row_ids.is_empty() {
                        index.entries.remove(key);
                    }
                }
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn commit_transaction_versions(&mut self, xid: Xid, commit_seq: CommitSeq) -> bool {
        let truncated = self.truncated.remove(&xid).is_some();
        self.reclamation.touched.remove(&xid);
        if let Some(row_ids) = self.reclamation.pending.remove(&xid) {
            self.reclamation
                .committed
                .entry(commit_seq)
                .or_default()
                .extend(row_ids);
            true
        } else {
            truncated
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_reclaimable_versions(&self) -> bool {
        !self.reclamation.committed.is_empty()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn prune_versions(
        &mut self,
        horizon: CommitSeq,
        transactions: &TransactionRegistry,
    ) {
        let commit_seqs = self
            .reclamation
            .committed
            .range(..=horizon)
            .map(|(commit_seq, _)| *commit_seq)
            .collect::<Vec<_>>();
        if commit_seqs.is_empty() {
            return;
        }
        let mut row_ids = BTreeSet::new();
        for commit_seq in commit_seqs {
            row_ids.extend(
                self.reclamation
                    .committed
                    .remove(&commit_seq)
                    .expect("selected reclamation batch must exist"),
            );
        }
        for row_id in row_ids {
            let mut chain = self
                .version_chains
                .chains
                .remove(&row_id)
                .expect("reclamation candidate row must exist");
            let reclaim = chain
                .versions
                .iter()
                .map(|version| {
                    matches!(
                        version.xmax.and_then(|xmax| transactions.get_status(xmax)),
                        Some(TransactionStatus::Committed(commit_seq)) if commit_seq <= horizon
                    )
                })
                .collect::<Vec<_>>();
            for index in &mut self.indexes {
                let retained_keys = chain
                    .versions
                    .iter()
                    .zip(&reclaim)
                    .filter_map(|(version, &reclaim)| {
                        (!reclaim)
                            .then(|| build_row_index_key(&self.schema, index, &version.row))?
                    })
                    .collect::<BTreeSet<_>>();
                let removed_keys = chain
                    .versions
                    .iter()
                    .zip(&reclaim)
                    .filter_map(|(version, &reclaim)| {
                        reclaim.then(|| build_row_index_key(&self.schema, index, &version.row))?
                    })
                    .collect::<BTreeSet<_>>();
                for key in removed_keys.difference(&retained_keys) {
                    let remove_key = {
                        let row_ids = index
                            .entries
                            .get_mut(key)
                            .expect("reclaimed index entry must exist");
                        assert!(row_ids.remove(&row_id));
                        row_ids.is_empty()
                    };
                    if remove_key {
                        index.entries.remove(key);
                    }
                }
            }
            chain.versions = chain
                .versions
                .into_iter()
                .zip(reclaim)
                .filter_map(|(version, reclaim)| (!reclaim).then_some(version))
                .collect();
            if !chain.versions.is_empty() {
                let previous = self.version_chains.chains.insert(row_id, chain);
                assert!(previous.is_none());
            }
        }
    }

    pub(crate) fn get_version_chain(&self, row_id: RowId) -> Option<&RowVersionChain> {
        self.version_chains.chains.get(&row_id)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_version_chains(&self) -> impl Iterator<Item = (RowId, &RowVersionChain)> {
        self.version_chains
            .chains
            .iter()
            .map(|(row_id, chain)| (*row_id, chain))
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_unique_conflict_name(
        &self,
        row: &Row,
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        excluded_row: Option<RowId>,
        changed_columns: Option<&BTreeSet<usize>>,
        arbiter_columns: Option<&[usize]>,
        arbiter_predicate: Option<&ast::Expr>,
        context: &StatementContext,
    ) -> Option<&str> {
        let snapshot = snapshot.include_current_command();
        self.indexes
            .iter()
            .filter(|index| {
                let _ = changed_columns;
                if !index.unique {
                    return false;
                }
                arbiter_columns.is_none_or(|columns| {
                    index.columns == columns && index.predicate.as_ref() == arbiter_predicate
                })
            })
            .find(|index| {
                if !matches_index_predicate(&self.schema, index, row, context) {
                    return false;
                }
                let Some(key) = build_row_index_key(&self.schema, index, row) else {
                    return false;
                };
                index.entries.get(&key).is_some_and(|row_ids| {
                    row_ids.iter().any(|row_id| {
                        if Some(*row_id) == excluded_row {
                            return false;
                        }
                        let Some(version) =
                            self.version_chains.chains.get(row_id).and_then(|chain| {
                                find_visible_version(chain, &snapshot, current_xid, transactions)
                            })
                        else {
                            return false;
                        };
                        matches_index_predicate(&self.schema, index, &version.row, context)
                            && build_row_index_key(&self.schema, index, &version.row).as_ref()
                                == Some(&key)
                    })
                })
            })
            .map(|index| index.name.as_str())
    }

    pub(crate) fn pending_unique_changes(&self) -> PendingUniqueChanges {
        PendingUniqueChanges {
            entries: std::iter::repeat_with(BTreeMap::new)
                .take(self.indexes.len())
                .collect(),
            row_keys: BTreeMap::new(),
        }
    }

    pub(crate) fn find_unique_conflict_name_with_pending(
        &self,
        row: &Row,
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        excluded_row: Option<RowId>,
        context: &StatementContext,
        pending: &PendingUniqueChanges,
    ) -> Option<&str> {
        let snapshot = snapshot.include_current_command();
        self.indexes
            .iter()
            .enumerate()
            .find(|(index_number, index)| {
                if !index.unique {
                    return false;
                }
                if !matches_index_predicate(&self.schema, index, row, context) {
                    return false;
                }
                let Some(key) = build_row_index_key(&self.schema, index, row) else {
                    return false;
                };
                if pending.entries[*index_number]
                    .get(&key)
                    .is_some_and(|row_ids| {
                        row_ids.iter().any(|row_id| Some(*row_id) != excluded_row)
                    })
                {
                    return true;
                }
                index.entries.get(&key).is_some_and(|row_ids| {
                    row_ids.iter().any(|row_id| {
                        if Some(*row_id) == excluded_row || pending.row_keys.contains_key(row_id) {
                            return false;
                        }
                        let Some(version) =
                            self.version_chains.chains.get(row_id).and_then(|chain| {
                                find_visible_version(chain, &snapshot, current_xid, transactions)
                            })
                        else {
                            return false;
                        };
                        matches_index_predicate(&self.schema, index, &version.row, context)
                            && build_row_index_key(&self.schema, index, &version.row).as_ref()
                                == Some(&key)
                    })
                })
            })
            .map(|(_, index)| index.name.as_str())
    }

    pub(crate) fn record_pending_unique_change(
        &self,
        row_id: RowId,
        row: &Row,
        pending: &mut PendingUniqueChanges,
        context: &StatementContext,
    ) {
        if let Some(previous_keys) = pending.row_keys.remove(&row_id) {
            for (entries, key) in pending.entries.iter_mut().zip(previous_keys) {
                if let Some(key) = key
                    && let Some(row_ids) = entries.get_mut(&key)
                {
                    row_ids.remove(&row_id);
                    if row_ids.is_empty() {
                        entries.remove(&key);
                    }
                }
            }
        }

        let keys = self
            .indexes
            .iter()
            .map(|index| {
                if !index.unique {
                    return None;
                }
                if !matches_index_predicate(&self.schema, index, row, context) {
                    return None;
                }
                build_row_index_key(&self.schema, index, row)
            })
            .collect::<Vec<_>>();
        for (entries, key) in pending.entries.iter_mut().zip(&keys) {
            if let Some(key) = key {
                entries.entry(key.clone()).or_default().insert(row_id);
            }
        }
        pending.row_keys.insert(row_id, keys);
    }

    pub(crate) fn find_row_unique_conflict_name(
        &self,
        left: &Row,
        right: &Row,
        context: &StatementContext,
    ) -> Option<&str> {
        self.indexes
            .iter()
            .find(|index| {
                index.unique
                    && matches_index_predicate(&self.schema, index, left, context)
                    && matches_index_predicate(&self.schema, index, right, context)
                    && build_row_index_key(&self.schema, index, left).is_some_and(|key| {
                        build_row_index_key(&self.schema, index, right) == Some(key)
                    })
            })
            .map(|index| index.name.as_str())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_visible_unique_conflict(
        &self,
        row: &Row,
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        arbiter_columns: &[usize],
        arbiter_predicate: Option<&ast::Expr>,
        context: &StatementContext,
    ) -> Option<(RowId, &RowVersion)> {
        let snapshot = snapshot.include_current_command();
        let index = self.indexes.iter().find(|index| {
            index.unique
                && index.columns == arbiter_columns
                && index.predicate.as_ref() == arbiter_predicate
                && matches_index_predicate(&self.schema, index, row, context)
        })?;
        let key = build_row_index_key(&self.schema, index, row)?;
        index.entries.get(&key)?.iter().find_map(|row_id| {
            let version = self.version_chains.chains.get(row_id).and_then(|chain| {
                find_visible_version(chain, &snapshot, current_xid, transactions)
            })?;
            (matches_index_predicate(&self.schema, index, &version.row, context)
                && build_row_index_key(&self.schema, index, &version.row).as_ref() == Some(&key))
            .then_some((*row_id, version))
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_conflicting_row(
        &self,
        row: &Row,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        arbiter_columns: Option<&[usize]>,
        arbiter_predicate: Option<&ast::Expr>,
        context: &StatementContext,
    ) -> Option<RowId> {
        self.indexes
            .iter()
            .filter(|index| {
                index.unique
                    && arbiter_columns.is_none_or(|columns| {
                        index.columns == columns && index.predicate.as_ref() == arbiter_predicate
                    })
                    && matches_index_predicate(&self.schema, index, row, context)
            })
            .find_map(|index| {
                let key = build_row_index_key(&self.schema, index, row)?;
                index.entries.get(&key)?.iter().find_map(|row_id| {
                    self.version_chains
                        .chains
                        .get(row_id)?
                        .versions
                        .iter()
                        .rev()
                        .any(|version| {
                            version.xmin != current_xid
                                && !matches!(
                                    transactions.get_status(version.xmin),
                                    Some(TransactionStatus::Aborted)
                                )
                                && version.xmax.is_none_or(|xmax| {
                                    !matches!(
                                        transactions.get_status(xmax),
                                        Some(TransactionStatus::Committed(_))
                                    )
                                })
                                && matches_index_predicate(
                                    &self.schema,
                                    index,
                                    &version.row,
                                    context,
                                )
                                && build_row_index_key(&self.schema, index, &version.row).as_ref()
                                    == Some(&key)
                        })
                        .then_some(*row_id)
                })
            })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_unique_candidate_rows(
        &self,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        arbiter_columns: Option<&[usize]>,
        arbiter_predicate: Option<&ast::Expr>,
        context: &StatementContext,
    ) -> Vec<RowId> {
        self.version_chains
            .chains
            .iter()
            .filter_map(|(row_id, chain)| {
                chain
                    .versions
                    .iter()
                    .rev()
                    .find(|version| {
                        version.xmin != current_xid
                            && !matches!(
                                transactions.get_status(version.xmin),
                                Some(TransactionStatus::Aborted)
                            )
                            && version.xmax.is_none_or(|xmax| {
                                !matches!(
                                    transactions.get_status(xmax),
                                    Some(TransactionStatus::Committed(_))
                                )
                            })
                    })
                    .is_some_and(|version| {
                        self.indexes.iter().any(|index| {
                            index.unique
                                && arbiter_columns.is_none_or(|columns| {
                                    index.columns == columns
                                        && index.predicate.as_ref() == arbiter_predicate
                                })
                                && matches_index_predicate(
                                    &self.schema,
                                    index,
                                    &version.row,
                                    context,
                                )
                                && build_row_index_key(&self.schema, index, &version.row).is_some()
                        })
                    })
                    .then_some(*row_id)
            })
            .collect()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_unique_row(
        &self,
        columns: &[usize],
        values: &[Value],
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Option<RowId> {
        self.find_unique_visible_version(columns, values, snapshot, current_xid, transactions)
            .map(|(row_id, _)| row_id)
    }

    pub(crate) fn find_unique_candidate_row(
        &self,
        columns: &[usize],
        values: &[Value],
        current_xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Option<RowId> {
        let index = self
            .indexes
            .iter()
            .find(|index| index.unique && index.columns == columns && index.predicate.is_none())?;
        let key = build_index_key(&self.schema, columns, values)?;
        index.entries.get(&key)?.iter().find_map(|row_id| {
            self.version_chains
                .chains
                .get(row_id)?
                .versions
                .iter()
                .rev()
                .any(|version| {
                    version.xmin != current_xid
                        && !matches!(
                            transactions.get_status(version.xmin),
                            Some(TransactionStatus::Aborted)
                        )
                        && version.xmax.is_none_or(|xmax| {
                            !matches!(
                                transactions.get_status(xmax),
                                Some(TransactionStatus::Committed(_))
                            )
                        })
                        && build_row_index_key(&self.schema, index, &version.row).as_ref()
                            == Some(&key)
                })
                .then_some(*row_id)
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_unique_index(&self, columns: &[usize]) -> bool {
        self.indexes
            .iter()
            .any(|index| index.unique && index.columns == columns && index.predicate.is_none())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_unique_visible_version(
        &self,
        columns: &[usize],
        values: &[Value],
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Option<(RowId, &RowVersion)> {
        let index = self
            .indexes
            .iter()
            .find(|index| index.unique && index.columns == columns && index.predicate.is_none())?;
        let key = build_index_key(&self.schema, columns, values)?;
        index.entries.get(&key)?.iter().find_map(|row_id| {
            let version = self.version_chains.chains.get(row_id).and_then(|chain| {
                find_visible_version(chain, snapshot, current_xid, transactions)
            })?;
            (build_row_index_key(&self.schema, index, &version.row).as_ref() == Some(&key))
                .then_some((*row_id, version))
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn add_index_entries(
        &mut self,
        row_id: RowId,
        row: &Row,
        changed_columns: Option<&BTreeSet<usize>>,
    ) {
        let entries = self
            .indexes
            .iter()
            .enumerate()
            .filter(|(_, index)| {
                changed_columns.is_none_or(|columns| {
                    index.columns.iter().any(|column| columns.contains(column))
                })
            })
            .map(|(index, unique)| (index, build_row_index_key(&self.schema, unique, row)))
            .collect::<Vec<_>>();
        for (index, key) in entries {
            if let Some(key) = key {
                self.indexes[index]
                    .entries
                    .entry(key)
                    .or_default()
                    .insert(row_id);
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rebuild_indexes(&mut self) {
        for index in &mut self.indexes {
            index.entries.clear();
        }
        let entries = self
            .version_chains
            .chains
            .iter()
            .flat_map(|(row_id, chain)| {
                chain.versions.iter().flat_map(|version| {
                    self.indexes
                        .iter()
                        .enumerate()
                        .filter_map(|(index, unique)| {
                            build_row_index_key(&self.schema, unique, &version.row)
                                .map(|key| (index, key, *row_id))
                        })
                })
            })
            .collect::<Vec<_>>();
        for (index, key, row_id) in entries {
            self.indexes[index]
                .entries
                .entry(key)
                .or_default()
                .insert(row_id);
        }
    }
}

fn create_index(schema: &TableSchema, index: &IndexSchema) -> TableIndex {
    TableIndex {
        name: index.name.clone(),
        unique: index.unique,
        columns: index
            .columns
            .iter()
            .map(|definition| {
                schema
                    .columns
                    .iter()
                    .position(|column| column.name == definition.name)
                    .expect("index columns must exist")
            })
            .collect(),
        predicate: index.predicate.clone(),
        entries: BTreeMap::new(),
    }
}

fn matches_index_predicate(
    schema: &TableSchema,
    index: &TableIndex,
    row: &Row,
    context: &StatementContext,
) -> bool {
    index.predicate.as_ref().is_none_or(|predicate| {
        crate::executor::evaluate_index_predicate(predicate, schema, row, context)
            .expect("validated index predicate must evaluate")
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn build_row_index_key(schema: &TableSchema, index: &TableIndex, row: &Row) -> Option<IndexKey> {
    if index.columns.iter().any(|column| *column >= row.len()) {
        return None;
    }
    let values = index
        .columns
        .iter()
        .map(|column| row[*column].clone())
        .collect::<Vec<_>>();
    build_index_key(schema, &index.columns, &values)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn build_index_key(schema: &TableSchema, columns: &[usize], values: &[Value]) -> Option<IndexKey> {
    assert_eq!(columns.len(), values.len());
    columns
        .iter()
        .zip(values)
        .map(|(column, value)| normalize_index_value(value, schema.columns[*column].data_type.base))
        .collect::<Option<Vec<_>>>()
        .map(IndexKey)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn normalize_index_value(value: &Value, base: BaseType) -> Option<NormalizedIndexValue> {
    match (value, base) {
        (Value::Jsonb(value), BaseType::Jsonb) => Some(NormalizedIndexValue::Jsonb(value.clone())),
        (Value::Null, _) => None,
        (Value::Bool(value), BaseType::Bool) => Some(NormalizedIndexValue::Bool(*value)),
        (Value::Int2(value), BaseType::Int2) => Some(NormalizedIndexValue::Int2(*value)),
        (Value::Int4(value), BaseType::Int4) => Some(NormalizedIndexValue::Int4(*value)),
        (Value::Int8(value), BaseType::Int8) => Some(NormalizedIndexValue::Int8(*value)),
        (Value::Float4(value), BaseType::Float4) => {
            Some(NormalizedIndexValue::Float4(if value.is_nan() {
                f32::NAN.to_bits()
            } else if *value == 0.0 {
                0
            } else {
                value.to_bits()
            }))
        }
        (Value::Float8(value), BaseType::Float8) => {
            Some(NormalizedIndexValue::Float8(if value.is_nan() {
                f64::NAN.to_bits()
            } else if *value == 0.0 {
                0
            } else {
                value.to_bits()
            }))
        }
        (Value::Numeric(value), BaseType::Numeric) => {
            Some(NormalizedIndexValue::Numeric(value.normalized()))
        }
        (Value::Text(value), BaseType::Bpchar) => Some(NormalizedIndexValue::Text(
            value.trim_end_matches(' ').into(),
        )),
        (Value::Text(value), BaseType::Text | BaseType::Varchar) => {
            Some(NormalizedIndexValue::Text(value.clone()))
        }
        (Value::Bytea(value), BaseType::Bytea) => Some(NormalizedIndexValue::Bytea(value.clone())),
        (Value::Uuid(value), BaseType::Uuid) => Some(NormalizedIndexValue::Uuid(*value)),
        (Value::Date(value), BaseType::Date) => Some(NormalizedIndexValue::Date(*value)),
        (Value::Time(value), BaseType::Time) => Some(NormalizedIndexValue::Time(*value)),
        (Value::Timestamp(value), BaseType::Timestamp) => {
            Some(NormalizedIndexValue::Timestamp(*value))
        }
        (Value::TimestampTz(value), BaseType::TimestampTz) => {
            Some(NormalizedIndexValue::TimestampTz(*value))
        }
        (Value::Interval(value), BaseType::Interval) => {
            Some(NormalizedIndexValue::Interval(*value))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "storage_test.rs"]
mod tests;
