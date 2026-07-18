use std::collections::BTreeMap;

use crate::{catalog::TableSchema, txn::Xid, value::Value};

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

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub schema: TableSchema,
    rows: RowStore,
    next_rowid: u64,
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
        Table {
            schema,
            rows: RowStore {
                chains: BTreeMap::new(),
            },
            next_rowid: 1,
        }
    }

    pub fn insert(&mut self, xmin: Xid, row: Row) -> RowId {
        let row_id = RowId(self.next_rowid);
        self.next_rowid += 1;
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
        row_id
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
