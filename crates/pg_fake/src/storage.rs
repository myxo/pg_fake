use std::collections::{BTreeMap, BTreeSet};

use crate::{
    catalog::{Constraint, TableSchema},
    txn::{Snapshot, TransactionRegistry, Xid, find_visible_version},
    value::{BaseType, Value},
};

pub(crate) type Row = Vec<Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RowId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RowVersion {
    pub(crate) xmin: Xid,
    pub(crate) xmax: Option<Xid>,
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
    entries: BTreeMap<UniqueIndexKey, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub(crate) schema: TableSchema,
    version_chains: VersionChainStore,
    indexes: Vec<UniqueIndex>,
    next_rowid: u64,
}

impl Table {
    pub(crate) fn create(schema: TableSchema) -> Self {
        let indexes = schema
            .constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::PrimaryKey(columns) | Constraint::Unique(columns) => {
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
                        entries: BTreeMap::new(),
                    })
                }
                Constraint::Check(_) | Constraint::ForeignKey(_) => None,
            })
            .collect();
        Table {
            schema,
            version_chains: VersionChainStore {
                chains: BTreeMap::new(),
            },
            indexes,
            next_rowid: 1,
        }
    }

    pub(crate) fn insert(&mut self, xmin: Xid, row: Row) -> RowId {
        let row_id = RowId(self.next_rowid);
        self.next_rowid += 1;
        let index_row = row.clone();
        let previous = self.version_chains.chains.insert(
            row_id,
            RowVersionChain {
                versions: vec![RowVersion {
                    xmin,
                    xmax: None,
                    row,
                }],
            },
        );
        assert!(previous.is_none());
        self.add_index_entries(row_id, &index_row);
        row_id
    }

    pub(crate) fn mark_version_deleted(
        &mut self,
        row_id: RowId,
        version_xmin: Xid,
        xmax: Xid,
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
        row_id
    }

    pub(crate) fn append_updated_version(
        &mut self,
        row_id: RowId,
        version_xmin: Xid,
        xmin: Xid,
        row: Row,
    ) -> RowId {
        self.mark_version_deleted(row_id, version_xmin, xmin);
        let index_row = row.clone();
        self.version_chains
            .chains
            .get_mut(&row_id)
            .expect("row must exist")
            .versions
            .push(RowVersion {
                xmin,
                xmax: None,
                row,
            });
        self.add_index_entries(row_id, &index_row);
        row_id
    }

    pub(crate) fn discard_transaction_versions(&mut self, xid: Xid) {
        self.version_chains.chains.retain(|_, chain| {
            chain.versions.retain(|version| version.xmin != xid);
            for version in &mut chain.versions {
                if version.xmax == Some(xid) {
                    version.xmax = None;
                }
            }
            !chain.versions.is_empty()
        });
        self.rebuild_indexes();
    }

    pub(crate) fn iterate_version_chains(&self) -> impl Iterator<Item = (RowId, &RowVersionChain)> {
        self.version_chains
            .chains
            .iter()
            .map(|(row_id, chain)| (*row_id, chain))
    }

    pub(crate) fn has_visible_unique_conflict(
        &self,
        row: &Row,
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
        excluded_row: Option<RowId>,
    ) -> bool {
        self.indexes.iter().any(|index| {
            let Some(key) = build_row_index_key(&self.schema, index, row) else {
                return false;
            };
            index.entries.get(&key).is_some_and(|row_ids| {
                row_ids.iter().any(|row_id| {
                    if Some(*row_id) == excluded_row {
                        return false;
                    }
                    let Some(version) = self.version_chains.chains.get(row_id).and_then(|chain| {
                        find_visible_version(chain, snapshot, current_xid, transactions)
                    }) else {
                        return false;
                    };
                    build_row_index_key(&self.schema, index, &version.row).as_ref() == Some(&key)
                })
            })
        })
    }

    pub(crate) fn find_unique_row(
        &self,
        columns: &[usize],
        values: &[Value],
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionRegistry,
    ) -> Option<RowId> {
        let index = self.indexes.iter().find(|index| index.columns == columns)?;
        let key = build_index_key(&self.schema, columns, values)?;
        index.entries.get(&key)?.iter().find_map(|row_id| {
            let version = self.version_chains.chains.get(row_id).and_then(|chain| {
                find_visible_version(chain, snapshot, current_xid, transactions)
            })?;
            (build_row_index_key(&self.schema, index, &version.row).as_ref() == Some(&key))
                .then_some(*row_id)
        })
    }

    fn add_index_entries(&mut self, row_id: RowId, row: &Row) {
        let entries = self
            .indexes
            .iter()
            .map(|index| build_row_index_key(&self.schema, index, row))
            .collect::<Vec<_>>();
        for (index, key) in self.indexes.iter_mut().zip(entries) {
            if let Some(key) = key {
                index.entries.entry(key).or_default().insert(row_id);
            }
        }
    }

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

fn build_row_index_key(
    schema: &TableSchema,
    index: &UniqueIndex,
    row: &Row,
) -> Option<UniqueIndexKey> {
    let values = index
        .columns
        .iter()
        .map(|column| row[*column].clone())
        .collect::<Vec<_>>();
    build_index_key(schema, &index.columns, &values)
}

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
        _ => unreachable!("row values must match declared column types"),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        catalog::{Catalog, ColumnDef},
        value::{BaseType, PgType},
    };

    use super::*;

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
                }],
                vec![],
            )
            .unwrap();
        assert_eq!(table_id.0, 1);
        Table::create(catalog.require_table("items").unwrap().clone())
    }

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
                }],
                vec![Constraint::PrimaryKey(vec!["value".into()])],
            )
            .unwrap();
        Table::create(catalog.require_table("items").unwrap().clone())
    }

    #[test]
    fn creates_new_version_chain_for_insert() {
        let mut table = create_table();
        let row_id = table.insert(Xid(10), vec![Value::Int4(1)]);

        assert_eq!(row_id, RowId(1));
        assert_eq!(
            table.version_chains.chains.get(&row_id).unwrap().versions,
            vec![RowVersion {
                xmin: Xid(10),
                xmax: None,
                row: vec![Value::Int4(1)],
            }]
        );
    }

    #[test]
    fn finds_visible_rows_through_a_unique_index() {
        let mut table = create_indexed_table();
        let mut transactions = TransactionRegistry::create();
        let xid = transactions.begin();
        let snapshot = Snapshot::create(&transactions);
        let row_id = table.insert(xid, vec![Value::Int4(1)]);

        assert_eq!(
            table.find_unique_row(&[0], &[Value::Int4(1)], &snapshot, xid, &transactions,),
            Some(row_id)
        );
    }

    #[test]
    fn retires_old_version_and_appends_new_version_for_update() {
        let mut table = create_table();
        let row_id = table.insert(Xid(10), vec![Value::Int4(1)]);

        assert_eq!(
            table.append_updated_version(row_id, Xid(10), Xid(11), vec![Value::Int4(2)]),
            row_id
        );
        assert_eq!(
            table.version_chains.chains.get(&row_id).unwrap().versions,
            vec![
                RowVersion {
                    xmin: Xid(10),
                    xmax: Some(Xid(11)),
                    row: vec![Value::Int4(1)],
                },
                RowVersion {
                    xmin: Xid(11),
                    xmax: None,
                    row: vec![Value::Int4(2)],
                },
            ]
        );
    }

    #[test]
    fn abort_removes_created_versions_and_restores_retired_versions() {
        let mut table = create_table();
        let existing = table.insert(Xid(10), vec![Value::Int4(1)]);
        let inserted = table.insert(Xid(11), vec![Value::Int4(2)]);
        table.append_updated_version(existing, Xid(10), Xid(11), vec![Value::Int4(3)]);

        table.discard_transaction_versions(Xid(11));

        assert_eq!(
            table.version_chains.chains.get(&existing).unwrap().versions,
            vec![RowVersion {
                xmin: Xid(10),
                xmax: None,
                row: vec![Value::Int4(1)],
            }]
        );
        assert!(!table.version_chains.chains.contains_key(&inserted));
    }

    #[test]
    fn marks_current_version_deleted() {
        let mut table = create_table();
        let row_id = table.insert(Xid(10), vec![Value::Int4(1)]);
        table.append_updated_version(row_id, Xid(10), Xid(11), vec![Value::Int4(2)]);

        assert_eq!(table.mark_version_deleted(row_id, Xid(11), Xid(12)), row_id);
        assert_eq!(
            table.version_chains.chains.get(&row_id).unwrap().versions,
            vec![
                RowVersion {
                    xmin: Xid(10),
                    xmax: Some(Xid(11)),
                    row: vec![Value::Int4(1)],
                },
                RowVersion {
                    xmin: Xid(11),
                    xmax: Some(Xid(12)),
                    row: vec![Value::Int4(2)],
                },
            ]
        );
    }
}
