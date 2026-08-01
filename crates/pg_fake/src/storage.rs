use std::collections::{BTreeMap, BTreeSet};

use crate::{
    catalog::{Constraint, TableSchema},
    txn::{Snapshot, TransactionManager, Xid, visible_version},
    value::{BaseType, Value},
};

pub type Row = Vec<Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(pub u64);

#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub xmin: Xid,
    pub xmax: Option<Xid>,
    pub row: Row,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VersionChain {
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq)]
struct RowStore {
    chains: BTreeMap<RowId, VersionChain>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum IndexValue {
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Float4(u32),
    Float8(u64),
    Numeric(bigdecimal::BigDecimal),
    Text(String),
    Bytea(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IndexKey(Vec<IndexValue>);

#[derive(Debug, Clone, PartialEq)]
struct UniqueIndex {
    columns: Vec<usize>,
    entries: BTreeMap<IndexKey, BTreeSet<RowId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub schema: TableSchema,
    rows: RowStore,
    indexes: Vec<UniqueIndex>,
    next_rowid: u64,
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
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
                                    .position(|column| column.name == *name)
                                    .expect("constraint columns must exist")
                            })
                            .collect(),
                        entries: BTreeMap::new(),
                    })
                }
                Constraint::Check(_) => None,
            })
            .collect();
        Table {
            schema,
            rows: RowStore {
                chains: BTreeMap::new(),
            },
            indexes,
            next_rowid: 1,
        }
    }

    pub fn insert(&mut self, xmin: Xid, row: Row) -> RowId {
        let row_id = RowId(self.next_rowid);
        self.next_rowid += 1;
        let index_row = row.clone();
        let previous = self.rows.chains.insert(
            row_id,
            VersionChain {
                versions: vec![Version {
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

    pub fn tombstone(&mut self, row_id: RowId, version_xmin: Xid, xmax: Xid) -> RowId {
        let chain = self.rows.chains.get_mut(&row_id).expect("row must exist");
        let version = chain
            .versions
            .iter_mut()
            .rev()
            .find(|version| version.xmin == version_xmin && version.xmax.is_none())
            .expect("live version with xmin must exist");
        version.xmax = Some(xmax);
        row_id
    }

    pub fn update(&mut self, row_id: RowId, version_xmin: Xid, xmin: Xid, row: Row) -> RowId {
        self.tombstone(row_id, version_xmin, xmin);
        let index_row = row.clone();
        self.rows
            .chains
            .get_mut(&row_id)
            .expect("row must exist")
            .versions
            .push(Version {
                xmin,
                xmax: None,
                row,
            });
        self.add_index_entries(row_id, &index_row);
        row_id
    }

    pub fn abort(&mut self, xid: Xid) {
        self.rows.chains.retain(|_, chain| {
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

    pub fn versions(&self, row_id: RowId) -> Option<&VersionChain> {
        self.rows.chains.get(&row_id)
    }

    pub fn rows(&self) -> impl Iterator<Item = (RowId, &VersionChain)> {
        self.rows
            .chains
            .iter()
            .map(|(row_id, chain)| (*row_id, chain))
    }

    pub fn unique_conflict(
        &self,
        row: &Row,
        snapshot: &Snapshot,
        current_xid: Xid,
        transactions: &TransactionManager,
        excluded_row: Option<RowId>,
    ) -> bool {
        self.indexes.iter().any(|index| {
            let Some(key) = index_key(&self.schema, index, row) else {
                return false;
            };
            index.entries.get(&key).is_some_and(|row_ids| {
                row_ids.iter().any(|row_id| {
                    if Some(*row_id) == excluded_row {
                        return false;
                    }
                    let Some(version) = self.rows.chains.get(row_id).and_then(|chain| {
                        visible_version(chain, snapshot, current_xid, transactions)
                    }) else {
                        return false;
                    };
                    index_key(&self.schema, index, &version.row).as_ref() == Some(&key)
                })
            })
        })
    }

    fn add_index_entries(&mut self, row_id: RowId, row: &Row) {
        let entries = self
            .indexes
            .iter()
            .map(|index| index_key(&self.schema, index, row))
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
            .rows
            .chains
            .iter()
            .flat_map(|(row_id, chain)| {
                chain.versions.iter().flat_map(|version| {
                    self.indexes
                        .iter()
                        .enumerate()
                        .filter_map(|(index, unique)| {
                            index_key(&self.schema, unique, &version.row)
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

fn index_key(schema: &TableSchema, index: &UniqueIndex, row: &Row) -> Option<IndexKey> {
    index
        .columns
        .iter()
        .map(
            |column| match (&row[*column], schema.columns[*column].data_type.base) {
                (Value::Null, _) => None,
                (Value::Bool(value), BaseType::Bool) => Some(IndexValue::Bool(*value)),
                (Value::Int2(value), BaseType::Int2) => Some(IndexValue::Int2(*value)),
                (Value::Int4(value), BaseType::Int4) => Some(IndexValue::Int4(*value)),
                (Value::Int8(value), BaseType::Int8) => Some(IndexValue::Int8(*value)),
                (Value::Float4(value), BaseType::Float4) => {
                    Some(IndexValue::Float4(if value.is_nan() {
                        f32::NAN.to_bits()
                    } else if *value == 0.0 {
                        0
                    } else {
                        value.to_bits()
                    }))
                }
                (Value::Float8(value), BaseType::Float8) => {
                    Some(IndexValue::Float8(if value.is_nan() {
                        f64::NAN.to_bits()
                    } else if *value == 0.0 {
                        0
                    } else {
                        value.to_bits()
                    }))
                }
                (Value::Numeric(value), BaseType::Numeric) => {
                    Some(IndexValue::Numeric(value.normalized()))
                }
                (Value::Text(value), BaseType::Bpchar) => {
                    Some(IndexValue::Text(value.trim_end_matches(' ').into()))
                }
                (Value::Text(value), BaseType::Text | BaseType::Varchar) => {
                    Some(IndexValue::Text(value.clone()))
                }
                (Value::Bytea(value), BaseType::Bytea) => Some(IndexValue::Bytea(value.clone())),
                _ => unreachable!("row values must match declared column types"),
            },
        )
        .collect::<Option<Vec<_>>>()
        .map(IndexKey)
}

#[cfg(test)]
mod tests {
    use crate::{
        catalog::{Catalog, ColumnDef},
        value::{BaseType, PgType},
    };

    use super::*;

    fn table() -> Table {
        let mut catalog = Catalog::new();
        let table_id = catalog
            .create_table(
                "items".into(),
                vec![ColumnDef {
                    name: "value".into(),
                    data_type: PgType::new(BaseType::Int4),
                    nullable: false,
                    default: None,
                }],
                vec![],
            )
            .unwrap();
        assert_eq!(table_id.0, 1);
        Table::new(catalog.table("items").unwrap().clone())
    }

    #[test]
    fn insert_creates_a_new_version_chain() {
        let mut table = table();
        let row_id = table.insert(Xid(10), vec![Value::Int4(1)]);

        assert_eq!(row_id, RowId(1));
        assert_eq!(
            table.versions(row_id).unwrap().versions,
            vec![Version {
                xmin: Xid(10),
                xmax: None,
                row: vec![Value::Int4(1)],
            }]
        );
    }

    #[test]
    fn update_retires_the_old_version_and_appends_a_new_one() {
        let mut table = table();
        let row_id = table.insert(Xid(10), vec![Value::Int4(1)]);

        assert_eq!(
            table.update(row_id, Xid(10), Xid(11), vec![Value::Int4(2)]),
            row_id
        );
        assert_eq!(
            table.versions(row_id).unwrap().versions,
            vec![
                Version {
                    xmin: Xid(10),
                    xmax: Some(Xid(11)),
                    row: vec![Value::Int4(1)],
                },
                Version {
                    xmin: Xid(11),
                    xmax: None,
                    row: vec![Value::Int4(2)],
                },
            ]
        );
    }

    #[test]
    fn abort_removes_created_versions_and_restores_retired_versions() {
        let mut table = table();
        let existing = table.insert(Xid(10), vec![Value::Int4(1)]);
        let inserted = table.insert(Xid(11), vec![Value::Int4(2)]);
        table.update(existing, Xid(10), Xid(11), vec![Value::Int4(3)]);

        table.abort(Xid(11));

        assert_eq!(
            table.versions(existing).unwrap().versions,
            vec![Version {
                xmin: Xid(10),
                xmax: None,
                row: vec![Value::Int4(1)],
            }]
        );
        assert!(table.versions(inserted).is_none());
    }

    #[test]
    fn tombstone_retires_the_current_version() {
        let mut table = table();
        let row_id = table.insert(Xid(10), vec![Value::Int4(1)]);
        table.update(row_id, Xid(10), Xid(11), vec![Value::Int4(2)]);

        assert_eq!(table.tombstone(row_id, Xid(11), Xid(12)), row_id);
        assert_eq!(
            table.versions(row_id).unwrap().versions,
            vec![
                Version {
                    xmin: Xid(10),
                    xmax: Some(Xid(11)),
                    row: vec![Value::Int4(1)],
                },
                Version {
                    xmin: Xid(11),
                    xmax: Some(Xid(12)),
                    row: vec![Value::Int4(2)],
                },
            ]
        );
    }
}
