use std::collections::BTreeMap;

use sqlparser::ast::Expr;

use crate::{
    error::{PgError, Result, SqlState},
    value::PgType,
};

pub const DEFAULT_SCHEMA: &str = "public";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TableId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: PgType,
    pub nullable: bool,
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    PrimaryKey(Vec<String>),
    Unique(Vec<String>),
    Check(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub id: TableId,
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub name: String,
    tables: BTreeMap<String, TableSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    public: Schema,
    next_table_id: u64,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalog {
    pub fn new() -> Self {
        Catalog {
            public: Schema {
                name: DEFAULT_SCHEMA.into(),
                tables: BTreeMap::new(),
            },
            next_table_id: 1,
        }
    }

    pub fn public_schema(&self) -> &Schema {
        &self.public
    }

    pub fn create_table(
        &mut self,
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<Constraint>,
    ) -> Result<TableId> {
        if self.public.tables.contains_key(&name) {
            return Err(PgError::new(
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

    pub fn table(&self, name: &str) -> Result<&TableSchema> {
        self.public.tables.get(name).ok_or_else(|| {
            PgError::new(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    pub fn drop_table(&mut self, name: &str) -> Result<TableSchema> {
        self.public.tables.remove(name).ok_or_else(|| {
            PgError::new(
                SqlState::UndefinedTable,
                format!("table {name:?} does not exist"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::BaseType;

    fn column(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: PgType::new(BaseType::Int4),
            nullable,
            default: None,
        }
    }

    #[test]
    fn creates_looks_up_and_drops_tables() {
        let mut catalog = Catalog::new();
        let users = catalog
            .create_table(
                "users".into(),
                vec![column("id", false), column("age", true)],
                vec![],
            )
            .unwrap();
        let posts = catalog
            .create_table("posts".into(), vec![column("id", false)], vec![])
            .unwrap();

        assert_eq!(catalog.public_schema().name, DEFAULT_SCHEMA);
        assert_eq!(users, TableId(1));
        assert_eq!(posts, TableId(2));
        assert_eq!(catalog.table("users").unwrap().id, users);
        assert_eq!(
            catalog.table("users").unwrap().columns,
            vec![column("id", false), column("age", true)]
        );

        let dropped = catalog.drop_table("users").unwrap();
        assert_eq!(dropped.id, users);
        assert_eq!(
            catalog.table("users").unwrap_err().sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(catalog.table("posts").unwrap().id, posts);
    }

    #[test]
    fn duplicate_table_is_42p07() {
        let mut catalog = Catalog::new();
        catalog
            .create_table("users".into(), vec![column("id", false)], vec![])
            .unwrap();

        let error = catalog
            .create_table("users".into(), vec![column("id", false)], vec![])
            .unwrap_err();

        assert_eq!(error.sqlstate, SqlState::DuplicateTable);
    }

    #[test]
    fn missing_table_is_42p01() {
        let mut catalog = Catalog::new();

        assert_eq!(
            catalog.table("missing").unwrap_err().sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            catalog.drop_table("missing").unwrap_err().sqlstate,
            SqlState::UndefinedTable
        );
    }
}
