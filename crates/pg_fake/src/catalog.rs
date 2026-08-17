use std::collections::BTreeMap;

use sqlparser::ast;

use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    value::PgType,
};

pub(crate) const DEFAULT_SCHEMA: &str = "public";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TableId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnDef {
    pub(crate) name: String,
    pub(crate) data_type: PgType,
    pub(crate) nullable: bool,
    pub(crate) default: Option<ast::Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Constraint {
    PrimaryKey(Vec<String>),
    Unique(Vec<String>),
    Check(Box<ast::Expr>),
    ForeignKey(ForeignKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForeignKeyAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForeignKey {
    pub(crate) name: String,
    pub(crate) columns: Vec<String>,
    pub(crate) foreign_table: String,
    pub(crate) referred_columns: Vec<String>,
    pub(crate) on_delete: ForeignKeyAction,
    pub(crate) on_update: ForeignKeyAction,
    pub(crate) deferrable: bool,
    pub(crate) initially_deferred: bool,
    pub(crate) match_kind: Option<ast::ConstraintReferenceMatchKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableSchema {
    pub(crate) id: TableId,
    pub(crate) name: String,
    pub(crate) columns: Vec<ColumnDef>,
    pub(crate) constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Schema {
    pub(crate) name: String,
    tables: BTreeMap<String, TableSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    public: Schema,
    next_table_id: u64,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::create()
    }
}

impl Catalog {
    pub(crate) fn create() -> Self {
        Catalog {
            public: Schema {
                name: DEFAULT_SCHEMA.into(),
                tables: BTreeMap::new(),
            },
            next_table_id: 1,
        }
    }

    pub(crate) fn create_table(
        &mut self,
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<Constraint>,
    ) -> Result<TableId> {
        if self.public.tables.contains_key(&name) {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {name:?} already exists"),
            ));
        }

        let id = TableId(self.next_table_id);
        self.next_table_id += 1;
        self.public.tables.insert(
            name.clone(),
            TableSchema {
                id,
                name,
                columns,
                constraints,
            },
        );
        Ok(id)
    }

    pub(crate) fn require_table(&self, name: &str) -> Result<&TableSchema> {
        self.public.tables.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    pub(crate) fn iterate_tables(&self) -> impl Iterator<Item = &TableSchema> {
        self.public.tables.values()
    }

    pub(crate) fn drop_table(&mut self, name: &str) -> Result<TableSchema> {
        if let Some((table, constraint)) = self.public.tables.values().find_map(|table| {
            table
                .constraints
                .iter()
                .find_map(|constraint| match constraint {
                    Constraint::ForeignKey(foreign_key) if foreign_key.foreign_table == name => {
                        Some((table.name.as_str(), foreign_key.name.as_str()))
                    }
                    _ => None,
                })
        }) {
            return reject_unsupported(format!(
                "cannot drop table {name:?} because constraint {constraint:?} on table {table:?} depends on it"
            ));
        }
        self.public.tables.remove(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("table {name:?} does not exist"),
            )
        })
    }

    pub(crate) fn restore_table(&mut self, table: TableSchema) {
        let previous = self.public.tables.insert(table.name.clone(), table);
        assert!(previous.is_none(), "restored table must not already exist");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::BaseType;

    fn create_column(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: PgType::create(BaseType::Int4),
            nullable,
            default: None,
        }
    }

    #[test]
    fn creates_looks_up_and_drops_tables() {
        let mut catalog = Catalog::create();
        let users = catalog
            .create_table(
                "users".into(),
                vec![create_column("id", false), create_column("age", true)],
                vec![],
            )
            .unwrap();
        let posts = catalog
            .create_table("posts".into(), vec![create_column("id", false)], vec![])
            .unwrap();

        assert_eq!(catalog.public.name, DEFAULT_SCHEMA);
        assert_eq!(users, TableId(1));
        assert_eq!(posts, TableId(2));
        assert_eq!(catalog.require_table("users").unwrap().id, users);
        assert_eq!(
            catalog.require_table("users").unwrap().columns,
            vec![create_column("id", false), create_column("age", true)]
        );

        let dropped = catalog.drop_table("users").unwrap();
        assert_eq!(dropped.id, users);
        assert_eq!(
            catalog.require_table("users").unwrap_err().sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(catalog.require_table("posts").unwrap().id, posts);
    }

    #[test]
    fn reports_42p07_for_duplicate_table() {
        let mut catalog = Catalog::create();
        catalog
            .create_table("users".into(), vec![create_column("id", false)], vec![])
            .unwrap();

        let error = catalog
            .create_table("users".into(), vec![create_column("id", false)], vec![])
            .unwrap_err();

        assert_eq!(error.sqlstate, SqlState::DuplicateTable);
    }

    #[test]
    fn reports_42p01_for_missing_table() {
        let mut catalog = Catalog::create();

        assert_eq!(
            catalog.require_table("missing").unwrap_err().sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            catalog.drop_table("missing").unwrap_err().sqlstate,
            SqlState::UndefinedTable
        );
    }
}
