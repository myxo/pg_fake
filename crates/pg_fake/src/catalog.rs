use std::collections::BTreeMap;

use sqlparser::ast;

use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType},
};

pub(crate) const DEFAULT_SCHEMA: &str = "public";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TableId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SequenceId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceSchema {
    pub(crate) id: SequenceId,
    pub(crate) name: String,
    pub(crate) data_type: BaseType,
    pub(crate) increment: i64,
    pub(crate) min_value: i64,
    pub(crate) max_value: i64,
    pub(crate) start_value: i64,
    pub(crate) cycle: bool,
    pub(crate) cache: i64,
    pub(crate) owned_by: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityKind {
    Always,
    ByDefault,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnDef {
    pub(crate) name: String,
    pub(crate) data_type: PgType,
    pub(crate) nullable: bool,
    pub(crate) default: Option<ast::Expr>,
    pub(crate) default_sequence: Option<String>,
    pub(crate) identity: Option<IdentityKind>,
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
    sequences: BTreeMap<String, SequenceSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    public: Schema,
    next_table_id: u64,
    next_sequence_id: u64,
    deferrable_foreign_keys: Vec<(String, bool)>,
    referencing_foreign_keys: BTreeMap<String, Vec<(String, usize)>>,
}

impl Default for Catalog {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}

impl Catalog {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        Catalog {
            public: Schema {
                name: DEFAULT_SCHEMA.into(),
                tables: BTreeMap::new(),
                sequences: BTreeMap::new(),
            },
            next_table_id: 1,
            next_sequence_id: 1,
            deferrable_foreign_keys: Vec::new(),
            referencing_foreign_keys: BTreeMap::new(),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_table(
        &mut self,
        name: String,
        columns: Vec<ColumnDef>,
        constraints: Vec<Constraint>,
    ) -> Result<TableId> {
        if self.has_relation(&name) {
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
        self.rebuild_foreign_key_metadata();
        Ok(id)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn require_table(&self, name: &str) -> Result<&TableSchema> {
        if self.public.sequences.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a table"),
            ));
        }
        self.public.tables.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_tables(&self) -> impl Iterator<Item = &TableSchema> {
        self.public.tables.values()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_table(&mut self, name: &str) -> Result<TableSchema> {
        if self.public.sequences.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a table"),
            ));
        }
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
        let table = self.public.tables.remove(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("table {name:?} does not exist"),
            )
        })?;
        self.rebuild_foreign_key_metadata();
        Ok(table)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn restore_table(&mut self, table: TableSchema) {
        let previous = self.public.tables.insert(table.name.clone(), table);
        assert!(previous.is_none(), "restored table must not already exist");
        self.rebuild_foreign_key_metadata();
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn contains_deferred_foreign_keys(
        &self,
        deferred_constraints: &std::collections::BTreeSet<String>,
        defer_all: bool,
    ) -> bool {
        self.deferrable_foreign_keys
            .iter()
            .any(|(name, initially_deferred)| {
                defer_all || *initially_deferred || deferred_constraints.contains(name)
            })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn referencing_foreign_keys(&self, parent: &str) -> Vec<(TableSchema, ForeignKey)> {
        self.referencing_foreign_keys
            .get(parent)
            .into_iter()
            .flatten()
            .map(|(table, constraint)| {
                let schema = self
                    .public
                    .tables
                    .get(table)
                    .expect("foreign key metadata references an existing table");
                let Constraint::ForeignKey(foreign_key) = &schema.constraints[*constraint] else {
                    unreachable!("foreign key metadata references a foreign key")
                };
                (schema.clone(), foreign_key.clone())
            })
            .collect()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rebuild_foreign_key_metadata(&mut self) {
        let mut deferrable_foreign_keys = Vec::new();
        let mut referencing_foreign_keys: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        for schema in self.public.tables.values() {
            for (index, constraint) in schema.constraints.iter().enumerate() {
                let Constraint::ForeignKey(foreign_key) = constraint else {
                    continue;
                };
                if foreign_key.deferrable {
                    deferrable_foreign_keys
                        .push((foreign_key.name.clone(), foreign_key.initially_deferred));
                }
                referencing_foreign_keys
                    .entry(foreign_key.foreign_table.clone())
                    .or_default()
                    .push((schema.name.clone(), index));
            }
        }
        self.deferrable_foreign_keys = deferrable_foreign_keys;
        self.referencing_foreign_keys = referencing_foreign_keys;
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_relation(&self, name: &str) -> bool {
        self.public.tables.contains_key(name) || self.public.sequences.contains_key(name)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_sequence(&mut self, mut sequence: SequenceSchema) -> Result<SequenceId> {
        if self.has_relation(&sequence.name) {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {:?} already exists", sequence.name),
            ));
        }
        let id = SequenceId(self.next_sequence_id);
        self.next_sequence_id += 1;
        sequence.id = id;
        self.public
            .sequences
            .insert(sequence.name.clone(), sequence);
        Ok(id)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn require_sequence(&self, name: &str) -> Result<&SequenceSchema> {
        if self.public.tables.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a sequence"),
            ));
        }
        self.public.sequences.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_sequences(&self) -> impl Iterator<Item = &SequenceSchema> {
        self.public.sequences.values()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_sequence(&mut self, name: &str) -> Result<SequenceSchema> {
        if self.public.tables.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a sequence"),
            ));
        }
        if let Some((table, column)) = self
            .public
            .sequences
            .get(name)
            .and_then(|sequence| sequence.owned_by.as_ref())
        {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop sequence {name:?} because column {column:?} of table {table:?} requires it"
                ),
            ));
        }
        self.public.sequences.remove(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("sequence {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_owned_sequences(&mut self, table_name: &str) -> Vec<SequenceSchema> {
        let names = self
            .public
            .sequences
            .iter()
            .filter_map(|(name, sequence)| {
                (sequence.owned_by.as_ref().map(|(table, _)| table.as_str()) == Some(table_name))
                    .then_some(name.clone())
            })
            .collect::<Vec<_>>();
        names
            .into_iter()
            .map(|name| {
                self.public
                    .sequences
                    .remove(&name)
                    .expect("owned sequence must exist")
            })
            .collect()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn restore_sequence(&mut self, sequence: SequenceSchema) {
        let previous = self
            .public
            .sequences
            .insert(sequence.name.clone(), sequence);
        assert!(
            previous.is_none(),
            "restored sequence must not already exist"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::BaseType;

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn create_column(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: PgType::create(BaseType::Int4),
            nullable,
            default: None,
            default_sequence: None,
            identity: None,
        }
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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
