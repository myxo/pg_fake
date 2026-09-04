use std::collections::{BTreeMap, BTreeSet};

use sqlparser::ast;

use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    txn::{CommandId, CommitSeq, Snapshot, TransactionRegistry, TransactionStatus, Xid},
    value::{BaseType, PgType},
};

pub(crate) const DEFAULT_SCHEMA: &str = "public";
pub(crate) const TEMP_SCHEMA: &str = "pg_temp";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RelationName {
    pub(crate) schema: Option<String>,
    pub(crate) name: String,
}

impl RelationName {
    pub(crate) fn create(schema: Option<String>, name: String) -> Self {
        RelationName { schema, name }
    }

    pub(crate) fn create_unqualified(name: impl Into<String>) -> Self {
        RelationName {
            schema: None,
            name: name.into(),
        }
    }
}

impl From<&str> for RelationName {
    fn from(name: &str) -> Self {
        RelationName::create_unqualified(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ResolvedRelationName {
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
}

impl ResolvedRelationName {
    pub(crate) fn get_lock_name(&self) -> String {
        format!("{}:{}", self.schema_id.0, self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SchemaId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TableId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SequenceId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ConstraintId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SequenceSchema {
    pub(crate) id: SequenceId,
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
    pub(crate) data_type: BaseType,
    pub(crate) increment: i64,
    pub(crate) min_value: i64,
    pub(crate) max_value: i64,
    pub(crate) start_value: i64,
    pub(crate) cycle: bool,
    pub(crate) cache: i64,
    pub(crate) owned_by: Option<(TableId, String)>,
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
    pub(crate) default_sequence: Option<ResolvedRelationName>,
    pub(crate) identity: Option<IdentityKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Constraint {
    PrimaryKey {
        id: ConstraintId,
        name: String,
        columns: Vec<String>,
    },
    Unique {
        id: ConstraintId,
        name: String,
        columns: Vec<String>,
    },
    Check {
        id: ConstraintId,
        name: String,
        expression: Box<ast::Expr>,
        validated: bool,
    },
    ForeignKey(ForeignKey),
}

impl Constraint {
    pub(crate) fn get_id(&self) -> ConstraintId {
        match self {
            Self::PrimaryKey { id, .. } | Self::Unique { id, .. } | Self::Check { id, .. } => *id,
            Self::ForeignKey(foreign_key) => foreign_key.id,
        }
    }

    pub(crate) fn get_name(&self) -> Option<&str> {
        match self {
            Self::PrimaryKey { name, .. } | Self::Unique { name, .. } => Some(name),
            Self::Check { name, .. } => Some(name),
            Self::ForeignKey(foreign_key) => Some(&foreign_key.name),
        }
    }
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
    pub(crate) id: ConstraintId,
    pub(crate) name: String,
    pub(crate) columns: Vec<String>,
    pub(crate) foreign_table: RelationName,
    pub(crate) foreign_table_id: TableId,
    pub(crate) referred_columns: Vec<String>,
    pub(crate) on_delete: ForeignKeyAction,
    pub(crate) on_update: ForeignKeyAction,
    pub(crate) deferrable: bool,
    pub(crate) initially_deferred: bool,
    pub(crate) match_kind: Option<ast::ConstraintReferenceMatchKind>,
    pub(crate) validated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableSchema {
    pub(crate) id: TableId,
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
    pub(crate) columns: Vec<ColumnDef>,
    pub(crate) constraints: Vec<Constraint>,
    pub(crate) persistence: TablePersistence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TablePersistence {
    Permanent,
    Temporary { on_commit_drop: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Schema {
    pub(crate) id: SchemaId,
    pub(crate) name: String,
    tables: BTreeMap<String, TableSchema>,
    sequences: BTreeMap<String, SequenceSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    schemas: BTreeMap<String, Schema>,
    next_schema_id: u64,
    next_table_id: u64,
    next_sequence_id: u64,
    next_constraint_id: u64,
    deferrable_foreign_keys: Vec<(ConstraintId, bool)>,
    referencing_foreign_keys: BTreeMap<TableId, Vec<(TableId, usize)>>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogHistory {
    schemas: BTreeMap<SchemaId, Vec<CatalogVersion<SchemaIdentity>>>,
    tables: BTreeMap<TableId, Vec<CatalogVersion<TableSchema>>>,
    sequences: BTreeMap<SequenceId, Vec<CatalogVersion<SequenceSchema>>>,
    next_schema_id: u64,
    next_table_id: u64,
    next_sequence_id: u64,
    next_constraint_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimedCatalogObjects {
    pub(crate) tables: Vec<TableId>,
    pub(crate) sequences: Vec<SequenceId>,
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
        let public = Schema {
            id: SchemaId(1),
            name: DEFAULT_SCHEMA.into(),
            tables: BTreeMap::new(),
            sequences: BTreeMap::new(),
        };
        Catalog {
            schemas: BTreeMap::from([(public.name.clone(), public)]),
            next_schema_id: 2,
            next_table_id: 1,
            next_sequence_id: 1,
            next_constraint_id: 1,
            deferrable_foreign_keys: Vec::new(),
            referencing_foreign_keys: BTreeMap::new(),
        }
    }

    fn get_default_schema(&self) -> &Schema {
        self.require_schema(DEFAULT_SCHEMA)
            .expect("the public schema must exist")
    }

    fn get_default_schema_mut(&mut self) -> &mut Schema {
        self.schemas
            .get_mut(DEFAULT_SCHEMA)
            .expect("the public schema must exist")
    }

    fn get_schema_by_id_mut(&mut self, id: SchemaId) -> &mut Schema {
        self.schemas
            .values_mut()
            .find(|schema| schema.id == id)
            .expect("catalog object schema must exist")
    }

    fn get_schema_by_id(&self, id: SchemaId) -> &Schema {
        self.schemas
            .values()
            .find(|schema| schema.id == id)
            .expect("catalog object schema must exist")
    }

    pub(crate) fn get_schema_name(&self, id: SchemaId) -> &str {
        &self.get_schema_by_id(id).name
    }

    pub(crate) fn resolve_relation_name(
        &self,
        name: &RelationName,
    ) -> Result<ResolvedRelationName> {
        let schema = match &name.schema {
            Some(schema) => self.require_schema(schema)?,
            None => self
                .schemas
                .get(TEMP_SCHEMA)
                .filter(|schema| {
                    schema.tables.contains_key(&name.name)
                        || schema.sequences.contains_key(&name.name)
                })
                .unwrap_or_else(|| self.get_default_schema()),
        };
        Ok(ResolvedRelationName {
            schema_id: schema.id,
            name: name.name.clone(),
        })
    }

    pub(crate) fn resolve_creation_name(
        &self,
        name: &RelationName,
        temporary: bool,
    ) -> Result<ResolvedRelationName> {
        let schema = if temporary {
            match name.schema.as_deref() {
                None | Some(TEMP_SCHEMA) => self.require_schema(TEMP_SCHEMA)?,
                Some(schema) => {
                    return Err(PgError::create(
                        SqlState::InvalidTableDefinition,
                        format!("temporary relations cannot specify schema {schema:?}"),
                    ));
                }
            }
        } else {
            match name.schema.as_deref() {
                None => self.get_default_schema(),
                Some(schema) => self.require_schema(schema)?,
            }
        };
        Ok(ResolvedRelationName {
            schema_id: schema.id,
            name: name.name.clone(),
        })
    }

    pub(crate) fn has_resolved_relation(&self, name: &ResolvedRelationName) -> bool {
        let schema = self.get_schema_by_id(name.schema_id);
        schema.tables.contains_key(&name.name) || schema.sequences.contains_key(&name.name)
    }

    pub(crate) fn require_named_table(&self, name: &RelationName) -> Result<&TableSchema> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.sequences.contains_key(&name.name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a table", name.name),
            ));
        }
        schema.tables.get(&name.name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {:?} does not exist", name.name),
            )
        })
    }

    pub(crate) fn require_named_sequence(&self, name: &RelationName) -> Result<&SequenceSchema> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.tables.contains_key(&name.name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a sequence", name.name),
            ));
        }
        schema.sequences.get(&name.name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {:?} does not exist", name.name),
            )
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn create_schema(&mut self, name: String) -> Result<SchemaId> {
        if self.schemas.contains_key(&name) {
            return Err(PgError::create(
                SqlState::DuplicateSchema,
                format!("schema {name:?} already exists"),
            ));
        }
        let id = SchemaId(self.next_schema_id);
        self.next_schema_id += 1;
        let previous = self.schemas.insert(
            name.clone(),
            Schema {
                id,
                name,
                tables: BTreeMap::new(),
                sequences: BTreeMap::new(),
            },
        );
        assert!(previous.is_none(), "new schema must not replace a schema");
        Ok(id)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn drop_schema(&mut self, name: &str) -> Result<Schema> {
        if name == DEFAULT_SCHEMA {
            return reject_unsupported("dropping the public schema is not implemented");
        }
        let schema = self.schemas.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::InvalidSchemaName,
                format!("schema {name:?} does not exist"),
            )
        })?;
        if !schema.tables.is_empty() || !schema.sequences.is_empty() {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!("cannot drop schema {name:?} because other objects depend on it"),
            ));
        }
        Ok(self
            .schemas
            .remove(name)
            .expect("required schema must exist"))
    }

    pub(crate) fn require_schema(&self, name: &str) -> Result<&Schema> {
        self.schemas.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::InvalidSchemaName,
                format!("schema {name:?} does not exist"),
            )
        })
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_table(
        &mut self,
        name: String,
        columns: Vec<ColumnDef>,
        mut constraints: Vec<Constraint>,
    ) -> Result<TableId> {
        if self.has_relation(&name) {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {name:?} already exists"),
            ));
        }

        let id = TableId(self.next_table_id);
        self.next_table_id += 1;
        let schema_id = self.get_default_schema().id;
        for constraint in &mut constraints {
            let constraint_id = ConstraintId(self.next_constraint_id);
            self.next_constraint_id += 1;
            match constraint {
                Constraint::PrimaryKey { id, .. }
                | Constraint::Unique { id, .. }
                | Constraint::Check { id, .. } => *id = constraint_id,
                Constraint::ForeignKey(foreign_key) => {
                    foreign_key.id = constraint_id;
                    foreign_key.foreign_table_id = if foreign_key.foreign_table.schema.is_none()
                        && foreign_key.foreign_table.name == name
                    {
                        id
                    } else {
                        self.require_named_table(&foreign_key.foreign_table)?.id
                    };
                }
            }
        }
        self.get_default_schema_mut().tables.insert(
            name.clone(),
            TableSchema {
                id,
                schema_id,
                name,
                columns,
                constraints,
                persistence: TablePersistence::Permanent,
            },
        );
        self.rebuild_foreign_key_metadata();
        Ok(id)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_named_table(
        &mut self,
        name: ResolvedRelationName,
        columns: Vec<ColumnDef>,
        mut constraints: Vec<Constraint>,
        persistence: TablePersistence,
    ) -> Result<TableId> {
        if self.has_resolved_relation(&name) {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {:?} already exists", name.name),
            ));
        }

        let id = TableId(self.next_table_id);
        self.next_table_id += 1;
        for constraint in &mut constraints {
            let constraint_id = ConstraintId(self.next_constraint_id);
            self.next_constraint_id += 1;
            match constraint {
                Constraint::PrimaryKey { id, .. }
                | Constraint::Unique { id, .. }
                | Constraint::Check { id, .. } => *id = constraint_id,
                Constraint::ForeignKey(foreign_key) => {
                    foreign_key.id = constraint_id;
                    foreign_key.foreign_table_id = if foreign_key.foreign_table.name == name.name
                        && foreign_key
                            .foreign_table
                            .schema
                            .as_deref()
                            .is_none_or(|schema| {
                                self.require_schema(schema)
                                    .is_ok_and(|schema| schema.id == name.schema_id)
                            }) {
                        id
                    } else {
                        self.require_named_table(&foreign_key.foreign_table)?.id
                    };
                }
            }
        }
        let previous = self.get_schema_by_id_mut(name.schema_id).tables.insert(
            name.name.clone(),
            TableSchema {
                id,
                schema_id: name.schema_id,
                name: name.name,
                columns,
                constraints,
                persistence,
            },
        );
        assert!(previous.is_none(), "new table must not replace a relation");
        self.rebuild_foreign_key_metadata();
        Ok(id)
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn require_table(&self, name: &str) -> Result<&TableSchema> {
        let schema = self.get_default_schema();
        if schema.sequences.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a table"),
            ));
        }
        schema.tables.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn require_table_mut(&mut self, name: &str) -> Result<&mut TableSchema> {
        let schema = self.get_default_schema_mut();
        if schema.sequences.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a table"),
            ));
        }
        schema.tables.get_mut(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn require_table_by_id(&self, id: TableId) -> Result<&TableSchema> {
        self.schemas
            .values()
            .flat_map(|schema| schema.tables.values())
            .find(|table| table.id == id)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation with id {} does not exist", id.0),
                )
            })
    }

    pub(crate) fn replace_table(&mut self, table: TableSchema) -> Result<()> {
        let current = self.require_table_by_id(table.id)?.clone();
        let target = ResolvedRelationName {
            schema_id: table.schema_id,
            name: table.name.clone(),
        };
        if (current.schema_id != table.schema_id || current.name != table.name)
            && self.has_resolved_relation(&target)
        {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {:?} already exists", table.name),
            ));
        }
        self.get_schema_by_id_mut(current.schema_id)
            .tables
            .remove(&current.name)
            .expect("required table must exist");
        let previous = self
            .get_schema_by_id_mut(table.schema_id)
            .tables
            .insert(table.name.clone(), table);
        assert!(previous.is_none(), "replacement table name must be free");
        self.rebuild_foreign_key_metadata();
        Ok(())
    }

    pub(crate) fn allocate_constraint_id(&mut self) -> ConstraintId {
        let id = ConstraintId(self.next_constraint_id);
        self.next_constraint_id += 1;
        id
    }

    pub(crate) fn rename_column_dependencies(
        &mut self,
        table_id: TableId,
        old_name: &str,
        new_name: &str,
    ) {
        for table in self
            .schemas
            .values_mut()
            .flat_map(|schema| schema.tables.values_mut())
        {
            for constraint in &mut table.constraints {
                match constraint {
                    Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. }
                        if table.id == table_id =>
                    {
                        for column in columns {
                            if column == old_name {
                                *column = new_name.to_owned();
                            }
                        }
                    }
                    Constraint::ForeignKey(foreign_key) => {
                        if table.id == table_id {
                            for column in &mut foreign_key.columns {
                                if column == old_name {
                                    *column = new_name.to_owned();
                                }
                            }
                        }
                        if foreign_key.foreign_table_id == table_id {
                            for column in &mut foreign_key.referred_columns {
                                if column == old_name {
                                    *column = new_name.to_owned();
                                }
                            }
                        }
                    }
                    Constraint::Check { .. }
                    | Constraint::PrimaryKey { .. }
                    | Constraint::Unique { .. } => {}
                }
            }
        }
        for sequence in self
            .schemas
            .values_mut()
            .flat_map(|schema| schema.sequences.values_mut())
        {
            if let Some((owner, column)) = &mut sequence.owned_by
                && *owner == table_id
                && column == old_name
            {
                *column = new_name.to_owned();
            }
        }
        self.rebuild_foreign_key_metadata();
    }

    pub(crate) fn rename_table_dependencies(&mut self, table_id: TableId, new_name: &str) {
        for table in self
            .schemas
            .values_mut()
            .flat_map(|schema| schema.tables.values_mut())
        {
            for constraint in &mut table.constraints {
                if let Constraint::ForeignKey(foreign_key) = constraint
                    && foreign_key.foreign_table_id == table_id
                {
                    foreign_key.foreign_table.name = new_name.to_owned();
                }
            }
        }
    }

    pub(crate) fn has_constraint(&self, table_id: TableId, constraint_id: ConstraintId) -> bool {
        self.require_table_by_id(table_id).is_ok_and(|table| {
            table
                .constraints
                .iter()
                .any(|constraint| constraint.get_id() == constraint_id)
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_tables(&self) -> impl Iterator<Item = &TableSchema> {
        self.schemas
            .values()
            .flat_map(|schema| schema.tables.values())
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_tables(&mut self, names: &[String]) -> Result<Vec<TableSchema>> {
        let targets = names
            .iter()
            .map(|name| self.require_table(name).cloned())
            .collect::<Result<Vec<_>>>()?;
        let target_ids = targets
            .iter()
            .map(|table| table.id)
            .collect::<BTreeSet<_>>();
        if let Some((target, table, constraint)) = self.iterate_tables().find_map(|table| {
            if target_ids.contains(&table.id) {
                return None;
            }
            table.constraints.iter().find_map(|constraint| {
                let Constraint::ForeignKey(foreign_key) = constraint else {
                    return None;
                };
                target_ids
                    .contains(&foreign_key.foreign_table_id)
                    .then_some((
                        foreign_key.foreign_table.name.as_str(),
                        table.name.as_str(),
                        foreign_key.name.as_str(),
                    ))
            })
        }) {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop table {target:?} because constraint {constraint:?} on table {table:?} depends on it"
                ),
            ));
        }
        let dropped = names
            .iter()
            .map(|name| {
                self.get_default_schema_mut()
                    .tables
                    .remove(name)
                    .expect("required table must exist")
            })
            .collect();
        self.rebuild_foreign_key_metadata();
        Ok(dropped)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_named_tables(&mut self, names: &[RelationName]) -> Result<Vec<TableSchema>> {
        let targets = names
            .iter()
            .map(|name| self.require_named_table(name).cloned())
            .collect::<Result<Vec<_>>>()?;
        let target_ids = targets
            .iter()
            .map(|table| table.id)
            .collect::<BTreeSet<_>>();
        if let Some((target, table, constraint)) = self.iterate_tables().find_map(|table| {
            if target_ids.contains(&table.id) {
                return None;
            }
            table.constraints.iter().find_map(|constraint| {
                let Constraint::ForeignKey(foreign_key) = constraint else {
                    return None;
                };
                target_ids
                    .contains(&foreign_key.foreign_table_id)
                    .then_some((
                        foreign_key.foreign_table.name.as_str(),
                        table.name.as_str(),
                        foreign_key.name.as_str(),
                    ))
            })
        }) {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop table {target:?} because constraint {constraint:?} on table {table:?} depends on it"
                ),
            ));
        }
        let dropped = targets
            .into_iter()
            .map(|table| {
                self.get_schema_by_id_mut(table.schema_id)
                    .tables
                    .remove(&table.name)
                    .expect("required table must exist")
            })
            .collect();
        self.rebuild_foreign_key_metadata();
        Ok(dropped)
    }

    pub(crate) fn drop_table_by_id(&mut self, id: TableId) -> Result<TableSchema> {
        let table = self.require_table_by_id(id)?.clone();
        let dropped = self
            .get_schema_by_id_mut(table.schema_id)
            .tables
            .remove(&table.name)
            .expect("required table must exist");
        self.rebuild_foreign_key_metadata();
        Ok(dropped)
    }

    #[cfg(test)]
    pub(crate) fn drop_table(&mut self, name: &str) -> Result<TableSchema> {
        Ok(self.drop_tables(&[name.to_owned()])?.remove(0))
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn contains_deferred_foreign_keys(
        &self,
        deferred_constraints: &std::collections::BTreeSet<ConstraintId>,
        defer_all: bool,
    ) -> bool {
        self.deferrable_foreign_keys
            .iter()
            .any(|(id, initially_deferred)| {
                defer_all || *initially_deferred || deferred_constraints.contains(id)
            })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn referencing_foreign_keys(
        &self,
        parent: TableId,
    ) -> Vec<(TableSchema, ForeignKey)> {
        self.referencing_foreign_keys
            .get(&parent)
            .into_iter()
            .flatten()
            .map(|(table, constraint)| {
                let schema = self
                    .require_table_by_id(*table)
                    .expect("foreign key metadata references an existing table");
                let Constraint::ForeignKey(foreign_key) = &schema.constraints[*constraint] else {
                    unreachable!("foreign key metadata references a foreign key")
                };
                (schema.clone(), foreign_key.clone())
            })
            .collect()
    }

    pub(crate) fn has_referencing_foreign_keys(&self, parent: TableId) -> bool {
        self.referencing_foreign_keys.contains_key(&parent)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn rebuild_foreign_key_metadata(&mut self) {
        let mut deferrable_foreign_keys = Vec::new();
        let mut referencing_foreign_keys: BTreeMap<TableId, Vec<(TableId, usize)>> =
            BTreeMap::new();
        for table in self.iterate_tables() {
            for (index, constraint) in table.constraints.iter().enumerate() {
                let Constraint::ForeignKey(foreign_key) = constraint else {
                    continue;
                };
                if foreign_key.deferrable {
                    deferrable_foreign_keys.push((foreign_key.id, foreign_key.initially_deferred));
                }
                referencing_foreign_keys
                    .entry(foreign_key.foreign_table_id)
                    .or_default()
                    .push((table.id, index));
            }
        }
        self.deferrable_foreign_keys = deferrable_foreign_keys;
        self.referencing_foreign_keys = referencing_foreign_keys;
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_relation(&self, name: &str) -> bool {
        let schema = self.get_default_schema();
        schema.tables.contains_key(name) || schema.sequences.contains_key(name)
    }

    #[cfg(test)]
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
        sequence.schema_id = self.get_default_schema().id;
        self.get_default_schema_mut()
            .sequences
            .insert(sequence.name.clone(), sequence);
        Ok(id)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create_named_sequence(
        &mut self,
        name: ResolvedRelationName,
        mut sequence: SequenceSchema,
    ) -> Result<SequenceId> {
        if self.has_resolved_relation(&name) {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {:?} already exists", name.name),
            ));
        }
        let id = SequenceId(self.next_sequence_id);
        self.next_sequence_id += 1;
        sequence.id = id;
        sequence.schema_id = name.schema_id;
        sequence.name = name.name;
        let previous = self
            .get_schema_by_id_mut(sequence.schema_id)
            .sequences
            .insert(sequence.name.clone(), sequence);
        assert!(
            previous.is_none(),
            "new sequence must not replace a relation"
        );
        Ok(id)
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn require_sequence(&self, name: &str) -> Result<&SequenceSchema> {
        let schema = self.get_default_schema();
        if schema.tables.contains_key(name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{name:?} is not a sequence"),
            ));
        }
        schema.sequences.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_sequences(&self) -> impl Iterator<Item = &SequenceSchema> {
        self.schemas
            .values()
            .flat_map(|schema| schema.sequences.values())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_named_sequence(&mut self, name: &RelationName) -> Result<SequenceSchema> {
        let sequence = self.require_named_sequence(name)?.clone();
        if let Some((table, column)) = &sequence.owned_by {
            let table = self
                .require_table_by_id(*table)
                .expect("sequence owner must remain visible");
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop sequence {:?} because column {column:?} of table {:?} requires it",
                    sequence.name, table.name
                ),
            ));
        }
        let resolved_name = ResolvedRelationName {
            schema_id: sequence.schema_id,
            name: sequence.name.clone(),
        };
        if let Some((table, column)) = self.iterate_tables().find_map(|table| {
            table.columns.iter().find_map(|column| {
                (column.default_sequence.as_ref() == Some(&resolved_name))
                    .then_some((table.name.as_str(), column.name.as_str()))
            })
        }) {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!(
                    "cannot drop sequence {:?} because column {column:?} of table {table:?} requires it",
                    sequence.name
                ),
            ));
        }
        Ok(self
            .get_schema_by_id_mut(sequence.schema_id)
            .sequences
            .remove(&sequence.name)
            .expect("required sequence must exist"))
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_owned_sequences(&mut self, table_id: TableId) -> Vec<SequenceSchema> {
        let names = self
            .schemas
            .values()
            .flat_map(|schema| {
                schema.sequences.iter().filter_map(|(name, sequence)| {
                    (sequence.owned_by.as_ref().map(|(table, _)| *table) == Some(table_id))
                        .then_some((schema.id, name.clone()))
                })
            })
            .collect::<Vec<_>>();
        names
            .into_iter()
            .map(|(schema_id, name)| {
                self.get_schema_by_id_mut(schema_id)
                    .sequences
                    .remove(&name)
                    .expect("owned sequence must exist")
            })
            .collect()
    }

    pub(crate) fn drop_column_owned_sequences(
        &mut self,
        table_id: TableId,
        column_name: &str,
    ) -> Vec<SequenceSchema> {
        let names = self
            .schemas
            .values()
            .flat_map(|schema| {
                schema.sequences.iter().filter_map(|(name, sequence)| {
                    (sequence.owned_by.as_ref() == Some(&(table_id, column_name.to_owned())))
                        .then_some((schema.id, name.clone()))
                })
            })
            .collect::<Vec<_>>();
        names
            .into_iter()
            .map(|(schema_id, name)| {
                self.get_schema_by_id_mut(schema_id)
                    .sequences
                    .remove(&name)
                    .expect("owned sequence must exist")
            })
            .collect()
    }
}

impl CatalogHistory {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        let schema = SchemaIdentity {
            id: SchemaId(1),
            name: DEFAULT_SCHEMA.into(),
        };
        CatalogHistory {
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
            next_schema_id: 2,
            next_table_id: 1,
            next_sequence_id: 1,
            next_constraint_id: 1,
        }
    }

    pub(crate) fn create_temporary_schema_id(&mut self) -> SchemaId {
        let id = SchemaId(self.next_schema_id);
        self.next_schema_id += 1;
        id
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
                        sequences: BTreeMap::new(),
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
                    sequences: BTreeMap::new(),
                },
            );
            assert!(previous.is_none(), "temporary schema name must be reserved");
        }
        assert!(
            schemas.contains_key(DEFAULT_SCHEMA),
            "the public schema must remain visible"
        );
        let mut catalog = Catalog {
            schemas,
            next_schema_id: self.next_schema_id,
            next_table_id: self.next_table_id,
            next_sequence_id: self.next_sequence_id,
            next_constraint_id: self.next_constraint_id,
            deferrable_foreign_keys: Vec::new(),
            referencing_foreign_keys: BTreeMap::new(),
        };
        for versions in self.tables.values() {
            let Some(table) = find_visible_catalog_version(versions, xid, snapshot, transactions)
            else {
                continue;
            };
            let Some(schema) = catalog
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
            let Some(schema) = catalog
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
        catalog.rebuild_foreign_key_metadata();
        catalog
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn record_changes(
        &mut self,
        previous: &Catalog,
        current: &Catalog,
        xid: Xid,
        command_id: CommandId,
    ) {
        record_catalog_changes(
            &mut self.schemas,
            previous
                .schemas
                .values()
                .map(|schema| {
                    (
                        schema.id,
                        SchemaIdentity {
                            id: schema.id,
                            name: schema.name.clone(),
                        },
                    )
                })
                .filter(|(_, schema)| schema.name != TEMP_SCHEMA),
            current
                .schemas
                .values()
                .map(|schema| {
                    (
                        schema.id,
                        SchemaIdentity {
                            id: schema.id,
                            name: schema.name.clone(),
                        },
                    )
                })
                .filter(|(_, schema)| schema.name != TEMP_SCHEMA),
            xid,
            command_id,
        );
        record_catalog_changes(
            &mut self.tables,
            previous
                .schemas
                .values()
                .flat_map(|schema| schema.tables.values())
                .map(|table| (table.id, table.clone())),
            current
                .schemas
                .values()
                .flat_map(|schema| schema.tables.values())
                .map(|table| (table.id, table.clone())),
            xid,
            command_id,
        );
        record_catalog_changes(
            &mut self.sequences,
            previous
                .schemas
                .values()
                .flat_map(|schema| schema.sequences.values())
                .map(|sequence| (sequence.id, sequence.clone())),
            current
                .schemas
                .values()
                .flat_map(|schema| schema.sequences.values())
                .map(|sequence| (sequence.id, sequence.clone())),
            xid,
            command_id,
        );
        self.next_schema_id = self.next_schema_id.max(current.next_schema_id);
        self.next_table_id = self.next_table_id.max(current.next_table_id);
        self.next_sequence_id = self.next_sequence_id.max(current.next_sequence_id);
        self.next_constraint_id = self.next_constraint_id.max(current.next_constraint_id);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn discard_transaction(&mut self, xid: Xid) -> ReclaimedCatalogObjects {
        discard_catalog_transaction(&mut self.schemas, xid);
        ReclaimedCatalogObjects {
            tables: discard_catalog_transaction(&mut self.tables, xid),
            sequences: discard_catalog_transaction(&mut self.sequences, xid),
        }
    }

    pub(crate) fn drop_temporary_schema(
        &mut self,
        temporary_schema_id: SchemaId,
    ) -> ReclaimedCatalogObjects {
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
        for id in &tables {
            self.tables.remove(id);
        }
        for id in &sequences {
            self.sequences.remove(id);
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
        prune_catalog_versions(
            &mut self.schemas,
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
fn record_catalog_changes<Id, T>(
    histories: &mut BTreeMap<Id, Vec<CatalogVersion<T>>>,
    previous: impl Iterator<Item = (Id, T)>,
    current: impl Iterator<Item = (Id, T)>,
    xid: Xid,
    command_id: CommandId,
) where
    Id: Copy + Ord,
    T: Clone + PartialEq,
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
                    .find(|version| version.xmax.is_none() && &version.value == old)
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
            value: new,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::BaseType;
    use chaos_theory::check;

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

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn get_constraint_id(constraint: &Constraint) -> ConstraintId {
        match constraint {
            Constraint::PrimaryKey { id, .. }
            | Constraint::Unique { id, .. }
            | Constraint::Check { id, .. } => *id,
            Constraint::ForeignKey(foreign_key) => foreign_key.id,
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

        assert_eq!(
            catalog.require_schema(DEFAULT_SCHEMA).unwrap().name,
            DEFAULT_SCHEMA
        );
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

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn versions_catalog_visibility_and_preserves_relation_identity() {
        let mut transactions = TransactionRegistry::create();
        let mut history = CatalogHistory::create();
        let before_create = Snapshot::create(&transactions);
        let creator = transactions.begin();
        let mut catalog = history.materialize(
            Some(creator),
            before_create.use_command(CommandId(0)),
            &transactions,
        );
        let previous = catalog.clone();
        let table_id = catalog
            .create_table("items".into(), vec![create_column("id", false)], vec![])
            .unwrap();
        history.record_changes(&previous, &catalog, creator, CommandId(0));

        assert!(
            history
                .materialize(
                    Some(creator),
                    before_create.use_command(CommandId(1)),
                    &transactions,
                )
                .require_table("items")
                .is_ok()
        );
        assert!(
            history
                .materialize(None, before_create, &transactions)
                .require_table("items")
                .is_err()
        );

        transactions.commit(creator);
        let after_create = Snapshot::create(&transactions);
        assert_eq!(
            history
                .materialize(None, after_create, &transactions)
                .require_table("items")
                .unwrap()
                .id,
            table_id
        );
        assert!(
            history
                .materialize(None, before_create, &transactions)
                .require_table("items")
                .is_err()
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn discards_aborted_catalog_versions_and_reclaims_created_identity() {
        let mut transactions = TransactionRegistry::create();
        let mut history = CatalogHistory::create();
        let creator = transactions.begin();
        let snapshot = Snapshot::create(&transactions);
        let mut catalog = history.materialize(Some(creator), snapshot, &transactions);
        let previous = catalog.clone();
        let table_id = catalog
            .create_table("items".into(), vec![create_column("id", false)], vec![])
            .unwrap();
        history.record_changes(&previous, &catalog, creator, CommandId(0));

        let reclaimed = history.discard_transaction(creator);
        transactions.abort(creator);

        assert_eq!(reclaimed.tables, vec![table_id]);
        assert!(
            history
                .materialize(None, Snapshot::create(&transactions), &transactions)
                .require_table("items")
                .is_err()
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn assigns_new_relation_and_constraint_identities_after_name_reuse() {
        let mut catalog = Catalog::create();
        let first_table = catalog
            .create_table(
                "items".into(),
                vec![create_column("id", false)],
                vec![Constraint::PrimaryKey {
                    id: ConstraintId(0),
                    name: "items_pkey".into(),
                    columns: vec!["id".into()],
                }],
            )
            .unwrap();
        let first_constraint =
            get_constraint_id(&catalog.require_table("items").unwrap().constraints[0]);
        catalog.drop_table("items").unwrap();
        let second_table = catalog
            .create_table(
                "items".into(),
                vec![create_column("id", false)],
                vec![Constraint::PrimaryKey {
                    id: ConstraintId(0),
                    name: "items_pkey".into(),
                    columns: vec!["id".into()],
                }],
            )
            .unwrap();
        let second_constraint =
            get_constraint_id(&catalog.require_table("items").unwrap().constraints[0]);

        assert_ne!(first_table, second_table);
        assert_ne!(first_constraint, second_constraint);
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn versions_schema_visibility_and_identity() {
        let mut transactions = TransactionRegistry::create();
        let mut history = CatalogHistory::create();
        let before_create = Snapshot::create(&transactions);
        let creator = transactions.begin();
        let mut catalog = history.materialize(Some(creator), before_create, &transactions);
        let previous = catalog.clone();
        let first_id = catalog.create_schema("app".into()).unwrap();
        history.record_changes(&previous, &catalog, creator, CommandId(0));

        assert!(
            history
                .materialize(
                    Some(creator),
                    before_create.use_command(CommandId(1)),
                    &transactions,
                )
                .require_schema("app")
                .is_ok()
        );
        assert!(
            history
                .materialize(None, before_create, &transactions)
                .require_schema("app")
                .is_err()
        );

        transactions.commit(creator);
        let before_drop = Snapshot::create(&transactions);
        let changer = transactions.begin();
        let mut catalog = history.materialize(Some(changer), before_drop, &transactions);
        let previous = catalog.clone();
        catalog.drop_schema("app").unwrap();
        history.record_changes(&previous, &catalog, changer, CommandId(0));
        let mut catalog = history.materialize(
            Some(changer),
            before_drop.use_command(CommandId(1)),
            &transactions,
        );
        let previous = catalog.clone();
        let second_id = catalog.create_schema("app".into()).unwrap();
        history.record_changes(&previous, &catalog, changer, CommandId(1));

        assert_ne!(first_id, second_id);
        assert_eq!(
            history
                .materialize(
                    Some(changer),
                    before_drop.use_command(CommandId(2)),
                    &transactions,
                )
                .require_schema("app")
                .unwrap()
                .id,
            second_id
        );
        assert_eq!(
            history
                .materialize(None, before_drop, &transactions)
                .require_schema("app")
                .unwrap()
                .id,
            first_id
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn keeps_the_default_schema_materializable() {
        let mut catalog = Catalog::create();

        assert_eq!(
            catalog.drop_schema(DEFAULT_SCHEMA).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
        assert_eq!(
            catalog.require_schema(DEFAULT_SCHEMA).unwrap().id,
            SchemaId(1)
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_catalog_snapshot_model_across_generated_name_reuse() {
        check(|source| {
            let commit_create: bool = source.any("commit_create");
            let commit_change: bool = source.any("commit_change");
            let recreate: bool = source.any("recreate");
            let mut transactions = TransactionRegistry::create();
            let mut history = CatalogHistory::create();
            let creator = transactions.begin();
            let before_create = Snapshot::create(&transactions);
            let mut catalog = history.materialize(Some(creator), before_create, &transactions);
            let previous = catalog.clone();
            let first_id = catalog
                .create_table("items".into(), vec![create_column("id", false)], vec![])
                .unwrap();
            history.record_changes(&previous, &catalog, creator, CommandId(0));

            if !commit_create {
                history.discard_transaction(creator);
                transactions.abort(creator);
                assert!(
                    history
                        .materialize(None, Snapshot::create(&transactions), &transactions)
                        .require_table("items")
                        .is_err()
                );
                return;
            }

            transactions.commit(creator);
            let retained_snapshot = Snapshot::create(&transactions);
            let reader = transactions.begin();
            transactions.retain_snapshot(reader, retained_snapshot);
            let changer = transactions.begin();
            let mut catalog = history.materialize(Some(changer), retained_snapshot, &transactions);
            let previous = catalog.clone();
            catalog.drop_table("items").unwrap();
            history.record_changes(&previous, &catalog, changer, CommandId(0));
            let second_id = recreate.then(|| {
                let mut catalog = history.materialize(
                    Some(changer),
                    retained_snapshot.use_command(CommandId(1)),
                    &transactions,
                );
                let previous = catalog.clone();
                let id = catalog
                    .create_table("items".into(), vec![create_column("id", false)], vec![])
                    .unwrap();
                history.record_changes(&previous, &catalog, changer, CommandId(1));
                id
            });

            assert_eq!(
                history
                    .materialize(Some(reader), retained_snapshot, &transactions)
                    .require_table("items")
                    .unwrap()
                    .id,
                first_id
            );
            if commit_change {
                transactions.commit(changer);
            } else {
                history.discard_transaction(changer);
                transactions.abort(changer);
            }
            let latest = history.materialize(None, Snapshot::create(&transactions), &transactions);
            if commit_change {
                match second_id {
                    Some(id) => assert_eq!(latest.require_table("items").unwrap().id, id),
                    None => assert!(latest.require_table("items").is_err()),
                }
            } else {
                assert_eq!(latest.require_table("items").unwrap().id, first_id);
            }
            assert_eq!(
                history
                    .materialize(Some(reader), retained_snapshot, &transactions)
                    .require_table("items")
                    .unwrap()
                    .id,
                first_id
            );
        });
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn binds_foreign_keys_and_sequence_owners_to_table_identities() {
        let mut catalog = Catalog::create();
        let parent = catalog
            .create_table(
                "parents".into(),
                vec![create_column("id", false)],
                vec![Constraint::PrimaryKey {
                    id: ConstraintId(0),
                    name: "parents_pkey".into(),
                    columns: vec!["id".into()],
                }],
            )
            .unwrap();
        let child = catalog
            .create_table(
                "children".into(),
                vec![create_column("parent_id", false)],
                vec![Constraint::ForeignKey(ForeignKey {
                    id: ConstraintId(0),
                    name: "children_parent_id_fkey".into(),
                    columns: vec!["parent_id".into()],
                    foreign_table: "parents".into(),
                    foreign_table_id: TableId(0),
                    referred_columns: vec!["id".into()],
                    on_delete: ForeignKeyAction::NoAction,
                    on_update: ForeignKeyAction::NoAction,
                    deferrable: false,
                    initially_deferred: false,
                    match_kind: None,
                    validated: true,
                })],
            )
            .unwrap();
        let Constraint::ForeignKey(foreign_key) =
            &catalog.require_table("children").unwrap().constraints[0]
        else {
            unreachable!()
        };
        assert_eq!(foreign_key.foreign_table_id, parent);
        assert_eq!(catalog.referencing_foreign_keys(parent)[0].0.id, child);

        let mut sequence = SequenceSchema {
            id: SequenceId(0),
            schema_id: SchemaId(0),
            name: "parents_id_seq".into(),
            data_type: BaseType::Int8,
            increment: 1,
            min_value: 1,
            max_value: i64::MAX,
            start_value: 1,
            cycle: false,
            cache: 1,
            owned_by: Some((parent, "id".into())),
        };
        let sequence_id = catalog.create_sequence(sequence.clone()).unwrap();
        sequence.id = sequence_id;
        sequence.schema_id = SchemaId(1);
        assert_eq!(
            catalog.require_sequence("parents_id_seq").unwrap(),
            &sequence
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn preserves_old_identity_across_transactional_drop_and_recreate() {
        let mut transactions = TransactionRegistry::create();
        let mut history = CatalogHistory::create();
        let creator = transactions.begin();
        let mut catalog = history.materialize(
            Some(creator),
            Snapshot::create(&transactions).use_command(CommandId(0)),
            &transactions,
        );
        let previous = catalog.clone();
        let old_id = catalog
            .create_table("items".into(), vec![create_column("id", false)], vec![])
            .unwrap();
        history.record_changes(&previous, &catalog, creator, CommandId(0));
        transactions.commit(creator);

        let reader = transactions.begin();
        let old_snapshot = Snapshot::create(&transactions);
        transactions.retain_snapshot(reader, old_snapshot);
        let changer = transactions.begin();
        let mut catalog = history.materialize(Some(changer), old_snapshot, &transactions);
        let previous = catalog.clone();
        catalog.drop_table("items").unwrap();
        history.record_changes(&previous, &catalog, changer, CommandId(0));
        let mut catalog = history.materialize(
            Some(changer),
            old_snapshot.use_command(CommandId(1)),
            &transactions,
        );
        let previous = catalog.clone();
        let new_id = catalog
            .create_table("items".into(), vec![create_column("id", false)], vec![])
            .unwrap();
        history.record_changes(&previous, &catalog, changer, CommandId(1));

        assert_ne!(old_id, new_id);
        assert_eq!(
            history
                .materialize(
                    Some(changer),
                    old_snapshot.use_command(CommandId(2)),
                    &transactions,
                )
                .require_table("items")
                .unwrap()
                .id,
            new_id
        );
        assert_eq!(
            history
                .materialize(Some(reader), old_snapshot, &transactions)
                .require_table("items")
                .unwrap()
                .id,
            old_id
        );

        transactions.commit(changer);
        let reclaimed = history.prune(
            transactions.find_reclamation_horizon(),
            &transactions,
            &std::collections::BTreeSet::new(),
        );
        assert!(reclaimed.tables.is_empty());
        assert_eq!(
            history
                .materialize(Some(reader), old_snapshot, &transactions)
                .require_table("items")
                .unwrap()
                .id,
            old_id
        );
        transactions.finish_read_only(reader);
        let reclaimed = history.prune(
            transactions.find_reclamation_horizon(),
            &transactions,
            &std::collections::BTreeSet::new(),
        );
        assert_eq!(reclaimed.tables, vec![old_id]);
        assert_eq!(
            history
                .materialize(None, Snapshot::create(&transactions), &transactions)
                .require_table("items")
                .unwrap()
                .id,
            new_id
        );
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn does_not_apply_deferred_state_to_a_recreated_constraint() {
        let mut catalog = Catalog::create();
        catalog
            .create_table(
                "parents".into(),
                vec![create_column("id", false)],
                vec![Constraint::PrimaryKey {
                    id: ConstraintId(0),
                    name: "parents_pkey".into(),
                    columns: vec!["id".into()],
                }],
            )
            .unwrap();
        let create_child_constraint = || {
            Constraint::ForeignKey(ForeignKey {
                id: ConstraintId(0),
                name: "children_parent_id_fkey".into(),
                columns: vec!["parent_id".into()],
                foreign_table: "parents".into(),
                foreign_table_id: TableId(0),
                referred_columns: vec!["id".into()],
                on_delete: ForeignKeyAction::NoAction,
                on_update: ForeignKeyAction::NoAction,
                deferrable: true,
                initially_deferred: false,
                match_kind: None,
                validated: true,
            })
        };
        catalog
            .create_table(
                "children".into(),
                vec![create_column("parent_id", false)],
                vec![create_child_constraint()],
            )
            .unwrap();
        let old_id = get_constraint_id(&catalog.require_table("children").unwrap().constraints[0]);
        catalog.drop_table("children").unwrap();
        catalog
            .create_table(
                "children".into(),
                vec![create_column("parent_id", false)],
                vec![create_child_constraint()],
            )
            .unwrap();
        let new_id = get_constraint_id(&catalog.require_table("children").unwrap().constraints[0]);

        assert_ne!(old_id, new_id);
        assert!(!catalog.contains_deferred_foreign_keys(
            &std::collections::BTreeSet::from([old_id]),
            false,
        ));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn versions_sequence_creation_and_abort() {
        let mut transactions = TransactionRegistry::create();
        let mut history = CatalogHistory::create();
        let creator = transactions.begin();
        let before_create = Snapshot::create(&transactions);
        let mut catalog = history.materialize(Some(creator), before_create, &transactions);
        let previous = catalog.clone();
        let sequence_id = catalog
            .create_sequence(SequenceSchema {
                id: SequenceId(0),
                schema_id: SchemaId(0),
                name: "item_ids".into(),
                data_type: BaseType::Int8,
                increment: 1,
                min_value: 1,
                max_value: i64::MAX,
                start_value: 1,
                cycle: false,
                cache: 1,
                owned_by: None,
            })
            .unwrap();
        history.record_changes(&previous, &catalog, creator, CommandId(0));

        assert_eq!(
            history
                .materialize(
                    Some(creator),
                    before_create.use_command(CommandId(1)),
                    &transactions,
                )
                .require_sequence("item_ids")
                .unwrap()
                .id,
            sequence_id
        );
        assert!(
            history
                .materialize(None, before_create, &transactions)
                .require_sequence("item_ids")
                .is_err()
        );
        assert_eq!(
            history.discard_transaction(creator).sequences,
            vec![sequence_id]
        );
        transactions.abort(creator);
        assert!(
            history
                .materialize(None, Snapshot::create(&transactions), &transactions)
                .require_sequence("item_ids")
                .is_err()
        );
    }
}
