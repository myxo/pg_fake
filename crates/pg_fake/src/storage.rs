use std::collections::{BTreeMap, BTreeSet};

use crate::{
    catalog::{Constraint, IndexSchema, TableSchema},
    executor::StatementExecutionContext,
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
    pending: BTreeMap<Xid, BTreeSet<RowId>>,
    committed: BTreeMap<CommitSeq, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NormalizedIndexValue {
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
struct UniqueIndexKey(Vec<NormalizedIndexValue>);

#[derive(Debug, Clone, PartialEq)]
struct UniqueIndex {
    columns: Vec<usize>,
    predicate: Option<ast::Expr>,
    entries: BTreeMap<UniqueIndexKey, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub(crate) schema: TableSchema,
    version_chains: VersionChainStore,
    indexes: Vec<UniqueIndex>,
    reclamation: Box<VersionReclamation>,
    next_rowid: u64,
}

impl Table {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create(schema: TableSchema) -> Self {
        let indexes = schema
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => {
                    Some(UniqueIndex {
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
                    })
                }
                Constraint::Check { .. } | Constraint::ForeignKey(_) => None,
            })
            .chain(
                schema
                    .indexes
                    .iter()
                    .filter_map(|index| index.unique.then(|| create_unique_index(&schema, index))),
            )
            .collect();
        Table {
            schema,
            version_chains: VersionChainStore {
                chains: BTreeMap::new(),
            },
            indexes,
            reclamation: Box::new(VersionReclamation {
                pending: BTreeMap::new(),
                committed: BTreeMap::new(),
            }),
            next_rowid: 1,
        }
    }

    pub(crate) fn replace_schema(&mut self, schema: TableSchema) {
        assert_eq!(self.schema.id, schema.id);
        self.schema = schema;
        self.indexes = self
            .schema
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } => {
                    Some(UniqueIndex {
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
                    })
                }
                Constraint::Check { .. } | Constraint::ForeignKey(_) => None,
            })
            .chain(self.schema.indexes.iter().filter_map(|index| {
                index
                    .unique
                    .then(|| create_unique_index(&self.schema, index))
            }))
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

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn insert(&mut self, xmin: Xid, command_id: CommandId, row: Row) -> RowId {
        let row_id = RowId(self.next_rowid);
        self.next_rowid += 1;
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
        self.reclamation.pending.remove(&xid);
        self.version_chains.chains.retain(|_, chain| {
            chain.versions.retain(|version| version.xmin != xid);
            for version in &mut chain.versions {
                if version.xmax == Some(xid) {
                    version.xmax = None;
                    version.xmax_command_id = None;
                }
            }
            !chain.versions.is_empty()
        });
        self.rebuild_indexes();
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn commit_transaction_versions(&mut self, xid: Xid, commit_seq: CommitSeq) -> bool {
        if let Some(row_ids) = self.reclamation.pending.remove(&xid) {
            self.reclamation
                .committed
                .entry(commit_seq)
                .or_default()
                .extend(row_ids);
            true
        } else {
            false
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

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_version_chains(&self) -> impl Iterator<Item = (RowId, &RowVersionChain)> {
        self.version_chains
            .chains
            .iter()
            .map(|(row_id, chain)| (*row_id, chain))
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_visible_unique_conflict(
        &self,
        row: &Row,
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        excluded_row: Option<RowId>,
        changed_columns: Option<&BTreeSet<usize>>,
        arbiter_columns: Option<&[usize]>,
        arbiter_predicate: Option<&ast::Expr>,
        context: &StatementExecutionContext,
    ) -> bool {
        let snapshot = snapshot.include_current_command();
        self.indexes
            .iter()
            .filter(|index| {
                let _ = changed_columns;
                arbiter_columns.is_none_or(|columns| {
                    index.columns == columns && index.predicate.as_ref() == arbiter_predicate
                })
            })
            .any(|index| {
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
    }

    pub(crate) fn rows_have_unique_conflict(
        &self,
        left: &Row,
        right: &Row,
        context: &StatementExecutionContext,
    ) -> bool {
        self.indexes.iter().any(|index| {
            matches_index_predicate(&self.schema, index, left, context)
                && matches_index_predicate(&self.schema, index, right, context)
                && build_row_index_key(&self.schema, index, left)
                    .is_some_and(|key| build_row_index_key(&self.schema, index, right) == Some(key))
        })
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
        context: &StatementExecutionContext,
    ) -> Option<(RowId, &RowVersion)> {
        let snapshot = snapshot.include_current_command();
        let index = self.indexes.iter().find(|index| {
            index.columns == arbiter_columns
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
        context: &StatementExecutionContext,
    ) -> Option<RowId> {
        self.indexes
            .iter()
            .filter(|index| {
                arbiter_columns.is_none_or(|columns| {
                    index.columns == columns && index.predicate.as_ref() == arbiter_predicate
                }) && matches_index_predicate(&self.schema, index, row, context)
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
        context: &StatementExecutionContext,
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
                            arbiter_columns.is_none_or(|columns| {
                                index.columns == columns
                                    && index.predicate.as_ref() == arbiter_predicate
                            }) && matches_index_predicate(
                                &self.schema,
                                index,
                                &version.row,
                                context,
                            ) && build_row_index_key(&self.schema, index, &version.row).is_some()
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
        let index = self
            .indexes
            .iter()
            .find(|index| index.columns == columns && index.predicate.is_none())?;
        let key = build_index_key(&self.schema, columns, values)?;
        index.entries.get(&key)?.iter().find_map(|row_id| {
            let version = self.version_chains.chains.get(row_id).and_then(|chain| {
                find_visible_version(chain, snapshot, current_xid, transactions)
            })?;
            (build_row_index_key(&self.schema, index, &version.row).as_ref() == Some(&key))
                .then_some(*row_id)
        })
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
            .find(|index| index.columns == columns && index.predicate.is_none())?;
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
            .any(|index| index.columns == columns && index.predicate.is_none())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn find_unique_visible_row(
        &self,
        columns: &[usize],
        values: &[Value],
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Option<&Row> {
        self.find_unique_visible_version(columns, values, snapshot, current_xid, transactions)
            .map(|(_, version)| &version.row)
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
        let row_id = self.find_unique_row(columns, values, snapshot, current_xid, transactions)?;
        self.version_chains
            .chains
            .get(&row_id)
            .and_then(|chain| find_visible_version(chain, snapshot, current_xid, transactions))
            .map(|version| (row_id, version))
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

fn create_unique_index(schema: &TableSchema, index: &IndexSchema) -> UniqueIndex {
    UniqueIndex {
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
    index: &UniqueIndex,
    row: &Row,
    context: &StatementExecutionContext,
) -> bool {
    index.predicate.as_ref().is_none_or(|predicate| {
        crate::executor::evaluate_index_predicate(predicate, schema, row, context)
            .expect("validated index predicate must evaluate")
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn build_row_index_key(
    schema: &TableSchema,
    index: &UniqueIndex,
    row: &Row,
) -> Option<UniqueIndexKey> {
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
fn build_index_key(
    schema: &TableSchema,
    columns: &[usize],
    values: &[Value],
) -> Option<UniqueIndexKey> {
    assert_eq!(columns.len(), values.len());
    columns
        .iter()
        .zip(values)
        .map(|(column, value)| normalize_index_value(value, schema.columns[*column].data_type.base))
        .collect::<Option<Vec<_>>>()
        .map(UniqueIndexKey)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn normalize_index_value(value: &Value, base: BaseType) -> Option<NormalizedIndexValue> {
    match (value, base) {
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
mod tests {
    use crate::{
        catalog::{Catalog, ColumnDef},
        value::{BaseType, PgType},
    };

    use super::*;

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn create_table() -> Table {
        let mut catalog = Catalog::create();
        let table_id = catalog
            .create_table(
                "items".into(),
                vec![ColumnDef {
                    name: "value".into(),
                    data_type: PgType::create(BaseType::Int4),
                    nullable: false,
                    default: None,
                    default_sequence: None,
                    identity: None,
                }],
                vec![],
            )
            .unwrap();
        assert_eq!(table_id.0, 1);
        Table::create(catalog.require_table("items").unwrap().clone())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn create_indexed_table() -> Table {
        let mut catalog = Catalog::create();
        catalog
            .create_table(
                "items".into(),
                vec![ColumnDef {
                    name: "value".into(),
                    data_type: PgType::create(BaseType::Int4),
                    nullable: false,
                    default: None,
                    default_sequence: None,
                    identity: None,
                }],
                vec![Constraint::PrimaryKey {
                    id: crate::catalog::ConstraintId(0),
                    name: "values_pkey".into(),
                    columns: vec!["value".into()],
                }],
            )
            .unwrap();
        Table::create(catalog.require_table("items").unwrap().clone())
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn creates_new_version_chain_for_insert() {
        let mut table = create_table();
        let row_id = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);

        assert_eq!(row_id, RowId(1));
        assert_eq!(
            table.version_chains.chains.get(&row_id).unwrap().versions,
            vec![RowVersion {
                xmin: Xid(10),
                xmin_command_id: CommandId(0),
                xmax: None,
                xmax_command_id: None,
                row: vec![Value::Int4(1)],
            }]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn finds_visible_rows_through_a_unique_index() {
        let mut table = create_indexed_table();
        let mut transactions = TransactionRegistry::create();
        let xid = transactions.begin();
        let snapshot = Snapshot::create(&transactions);
        let row_id = table.insert(xid, CommandId(0), vec![Value::Int4(1)]);

        assert_eq!(
            table.find_unique_row(&[0], &[Value::Int4(1)], &snapshot, xid, &transactions,),
            Some(row_id)
        );
        assert_eq!(
            table.find_unique_visible_row(&[0], &[Value::Int4(1)], &snapshot, xid, &transactions,),
            Some(&vec![Value::Int4(1)])
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn retires_old_version_and_appends_new_version_for_update() {
        let mut table = create_table();
        let row_id = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);

        assert_eq!(
            table.append_updated_version(
                row_id,
                Xid(10),
                Xid(11),
                CommandId(1),
                vec![Value::Int4(2)],
                None,
            ),
            row_id
        );
        assert_eq!(
            table.version_chains.chains.get(&row_id).unwrap().versions,
            vec![
                RowVersion {
                    xmin: Xid(10),
                    xmin_command_id: CommandId(0),
                    xmax: Some(Xid(11)),
                    xmax_command_id: Some(CommandId(1)),
                    row: vec![Value::Int4(1)],
                },
                RowVersion {
                    xmin: Xid(11),
                    xmin_command_id: CommandId(1),
                    xmax: None,
                    xmax_command_id: None,
                    row: vec![Value::Int4(2)],
                },
            ]
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn abort_removes_created_versions_and_restores_retired_versions() {
        let mut table = create_table();
        let existing = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);
        let inserted = table.insert(Xid(11), CommandId(1), vec![Value::Int4(2)]);
        table.append_updated_version(
            existing,
            Xid(10),
            Xid(11),
            CommandId(1),
            vec![Value::Int4(3)],
            None,
        );

        table.discard_transaction_versions(Xid(11));

        assert_eq!(
            table.version_chains.chains.get(&existing).unwrap().versions,
            vec![RowVersion {
                xmin: Xid(10),
                xmin_command_id: CommandId(0),
                xmax: None,
                xmax_command_id: None,
                row: vec![Value::Int4(1)],
            }]
        );
        assert!(!table.version_chains.chains.contains_key(&inserted));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn marks_current_version_deleted() {
        let mut table = create_table();
        let row_id = table.insert(Xid(10), CommandId(0), vec![Value::Int4(1)]);
        table.append_updated_version(
            row_id,
            Xid(10),
            Xid(11),
            CommandId(1),
            vec![Value::Int4(2)],
            None,
        );

        assert_eq!(
            table.mark_version_deleted(row_id, Xid(11), Xid(12), CommandId(2)),
            row_id
        );
        assert_eq!(
            table.version_chains.chains.get(&row_id).unwrap().versions,
            vec![
                RowVersion {
                    xmin: Xid(10),
                    xmin_command_id: CommandId(0),
                    xmax: Some(Xid(11)),
                    xmax_command_id: Some(CommandId(1)),
                    row: vec![Value::Int4(1)],
                },
                RowVersion {
                    xmin: Xid(11),
                    xmin_command_id: CommandId(1),
                    xmax: Some(Xid(12)),
                    xmax_command_id: Some(CommandId(2)),
                    row: vec![Value::Int4(2)],
                },
            ]
        );
    }
}
