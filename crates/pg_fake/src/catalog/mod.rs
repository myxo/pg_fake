use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

use sqlparser::ast;

use crate::{
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType},
};

mod history;

pub(crate) use history::{CatalogHistory, CatalogVisibility};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct IndexId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ViewId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TriggerId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FunctionId(pub(crate) u64);

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
pub(crate) struct IndexColumnDefinition {
    pub(crate) name: String,
    pub(crate) descending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexSchema {
    pub(crate) id: IndexId,
    pub(crate) name: String,
    pub(crate) unique: bool,
    pub(crate) columns: Vec<IndexColumnDefinition>,
    pub(crate) include: Vec<String>,
    pub(crate) predicate: Option<ast::Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TriggerSchema {
    pub(crate) id: TriggerId,
    pub(crate) name: String,
    pub(crate) function_id: FunctionId,
    pub(crate) definition: ast::CreateTrigger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionSchema {
    pub(crate) id: FunctionId,
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
    pub(crate) definition: ast::CreateFunction,
    pub(crate) body: ast::PlPgSqlBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ViewDependency {
    Table(TableId),
    View(ViewId),
    Sequence(SequenceId),
    Constraint(ConstraintId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewColumn {
    pub(crate) name: String,
    pub(crate) data_type: PgType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewSchema {
    pub(crate) id: ViewId,
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
    pub(crate) columns: Vec<ViewColumn>,
    pub(crate) query: Box<ast::Query>,
    pub(crate) comment: Option<String>,
    pub(crate) dependencies: BTreeSet<ViewDependency>,
    pub(crate) column_dependencies: BTreeMap<TableId, BTreeSet<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableSchema {
    pub(crate) id: TableId,
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
    pub(crate) columns: Vec<ColumnDef>,
    pub(crate) constraints: Vec<Constraint>,
    pub(crate) indexes: Vec<IndexSchema>,
    pub(crate) triggers: Vec<TriggerSchema>,
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
    tables: BTreeMap<String, Arc<TableSchema>>,
    views: BTreeMap<String, Arc<ViewSchema>>,
    sequences: BTreeMap<String, Arc<SequenceSchema>>,
    functions: BTreeMap<String, Arc<FunctionSchema>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    relations: Arc<CatalogRelations>,
    next_schema_id: u64,
    next_table_id: u64,
    next_sequence_id: u64,
    next_constraint_id: u64,
    next_index_id: u64,
    next_view_id: u64,
    next_trigger_id: u64,
    next_function_id: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogIdentity(Weak<CatalogRelations>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogRelations {
    schemas: BTreeMap<String, Schema>,
    deferrable_foreign_keys: Vec<(ConstraintId, bool)>,
    referencing_foreign_keys: BTreeMap<TableId, Vec<(TableId, usize)>>,
}

impl Default for Catalog {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn default() -> Self {
        Self::create()
    }
}

impl Catalog {
    pub(crate) fn create_identity(&self) -> CatalogIdentity {
        CatalogIdentity(Arc::downgrade(&self.relations))
    }

    pub(crate) fn matches_identity(&self, identity: &CatalogIdentity) -> bool {
        std::ptr::eq(Arc::as_ptr(&self.relations), identity.0.as_ptr())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        let public = Schema {
            id: SchemaId(1),
            name: DEFAULT_SCHEMA.into(),
            tables: BTreeMap::new(),
            views: BTreeMap::new(),
            sequences: BTreeMap::new(),
            functions: BTreeMap::new(),
        };
        Catalog {
            relations: Arc::new(CatalogRelations {
                schemas: BTreeMap::from([(public.name.clone(), public)]),
                deferrable_foreign_keys: Vec::new(),
                referencing_foreign_keys: BTreeMap::new(),
            }),
            next_schema_id: 2,
            next_table_id: 1,
            next_sequence_id: 1,
            next_constraint_id: 1,
            next_index_id: 1,
            next_view_id: 1,
            next_trigger_id: 1,
            next_function_id: 1,
        }
    }

    fn get_default_schema(&self) -> &Schema {
        self.require_schema(DEFAULT_SCHEMA)
            .expect("the public schema must exist")
    }

    fn get_default_schema_mut(&mut self) -> &mut Schema {
        Arc::make_mut(&mut self.relations)
            .schemas
            .get_mut(DEFAULT_SCHEMA)
            .expect("the public schema must exist")
    }

    fn get_schema_by_id_mut(&mut self, id: SchemaId) -> &mut Schema {
        Arc::make_mut(&mut self.relations)
            .schemas
            .values_mut()
            .find(|schema| schema.id == id)
            .expect("catalog object schema must exist")
    }

    fn get_schema_by_id(&self, id: SchemaId) -> &Schema {
        self.relations
            .schemas
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
            None => {
                self.relations
                    .schemas
                    .get(TEMP_SCHEMA)
                    .filter(|schema| {
                        schema.tables.contains_key(&name.name)
                            || schema.views.contains_key(&name.name)
                            || schema.sequences.contains_key(&name.name)
                            || schema.tables.values().any(|table| {
                                table.indexes.iter().any(|index| index.name == name.name)
                            })
                    })
                    .unwrap_or_else(|| self.get_default_schema())
            }
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
        schema.tables.contains_key(&name.name)
            || schema.views.contains_key(&name.name)
            || schema.sequences.contains_key(&name.name)
            || schema
                .tables
                .values()
                .any(|table| table.indexes.iter().any(|index| index.name == name.name))
    }

    pub(crate) fn require_named_table(&self, name: &RelationName) -> Result<&TableSchema> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.sequences.contains_key(&name.name) || schema.views.contains_key(&name.name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a table", name.name),
            ));
        }
        if schema
            .tables
            .values()
            .any(|table| table.indexes.iter().any(|index| index.name == name.name))
        {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a table", name.name),
            ));
        }
        schema
            .tables
            .get(&name.name)
            .map(Arc::as_ref)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation {:?} does not exist", name.name),
                )
            })
    }

    pub(crate) fn require_named_view(&self, name: &RelationName) -> Result<&ViewSchema> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.tables.contains_key(&name.name)
            || schema.sequences.contains_key(&name.name)
            || schema
                .tables
                .values()
                .any(|table| table.indexes.iter().any(|index| index.name == name.name))
        {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a view", name.name),
            ));
        }
        schema
            .views
            .get(&name.name)
            .map(Arc::as_ref)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation {:?} does not exist", name.name),
                )
            })
    }

    pub(crate) fn require_named_sequence(&self, name: &RelationName) -> Result<&SequenceSchema> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.tables.contains_key(&name.name) || schema.views.contains_key(&name.name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a sequence", name.name),
            ));
        }
        if schema
            .tables
            .values()
            .any(|table| table.indexes.iter().any(|index| index.name == name.name))
        {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a sequence", name.name),
            ));
        }
        schema
            .sequences
            .get(&name.name)
            .map(Arc::as_ref)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation {:?} does not exist", name.name),
                )
            })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn create_schema(&mut self, name: String) -> Result<SchemaId> {
        if self.relations.schemas.contains_key(&name) {
            return Err(PgError::create(
                SqlState::DuplicateSchema,
                format!("schema {name:?} already exists"),
            ));
        }
        let id = SchemaId(self.next_schema_id);
        self.next_schema_id += 1;
        let previous = Arc::make_mut(&mut self.relations).schemas.insert(
            name.clone(),
            Schema {
                id,
                name,
                tables: BTreeMap::new(),
                views: BTreeMap::new(),
                sequences: BTreeMap::new(),
                functions: BTreeMap::new(),
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
        let schema = self.relations.schemas.get(name).ok_or_else(|| {
            PgError::create(
                SqlState::InvalidSchemaName,
                format!("schema {name:?} does not exist"),
            )
        })?;
        if !schema.tables.is_empty()
            || !schema.views.is_empty()
            || !schema.sequences.is_empty()
            || !schema.functions.is_empty()
        {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!("cannot drop schema {name:?} because other objects depend on it"),
            ));
        }
        Ok(Arc::make_mut(&mut self.relations)
            .schemas
            .remove(name)
            .expect("required schema must exist"))
    }

    pub(crate) fn require_schema(&self, name: &str) -> Result<&Schema> {
        self.relations.schemas.get(name).ok_or_else(|| {
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
            Arc::new(TableSchema {
                id,
                schema_id,
                name,
                columns,
                constraints,
                indexes: Vec::new(),
                triggers: Vec::new(),
                persistence: TablePersistence::Permanent,
            }),
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
            Arc::new(TableSchema {
                id,
                schema_id: name.schema_id,
                name: name.name,
                columns,
                constraints,
                indexes: Vec::new(),
                triggers: Vec::new(),
                persistence,
            }),
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
        schema.tables.get(name).map(Arc::as_ref).ok_or_else(|| {
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
        schema
            .tables
            .get_mut(name)
            .map(Arc::make_mut)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation {name:?} does not exist"),
                )
            })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn require_table_by_id(&self, id: TableId) -> Result<&TableSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.tables.values())
            .map(Arc::as_ref)
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
            .insert(table.name.clone(), Arc::new(table));
        assert!(previous.is_none(), "replacement table name must be free");
        self.rebuild_foreign_key_metadata();
        Ok(())
    }

    pub(crate) fn allocate_constraint_id(&mut self) -> ConstraintId {
        let id = ConstraintId(self.next_constraint_id);
        self.next_constraint_id += 1;
        id
    }

    pub(crate) fn allocate_index_id(&mut self) -> IndexId {
        let id = IndexId(self.next_index_id);
        self.next_index_id += 1;
        id
    }

    pub(crate) fn allocate_trigger_id(&mut self) -> TriggerId {
        let id = TriggerId(self.next_trigger_id);
        self.next_trigger_id += 1;
        id
    }

    pub(crate) fn resolve_function_name(
        &self,
        name: &RelationName,
    ) -> Result<ResolvedRelationName> {
        let schema = match name.schema.as_deref() {
            Some(schema) => self.require_schema(schema)?,
            None => self.get_default_schema(),
        };
        Ok(ResolvedRelationName {
            schema_id: schema.id,
            name: name.name.clone(),
        })
    }

    pub(crate) fn require_named_function(&self, name: &RelationName) -> Result<&FunctionSchema> {
        let name = self.resolve_function_name(name)?;
        self.get_schema_by_id(name.schema_id)
            .functions
            .get(&name.name)
            .map(Arc::as_ref)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function {}() does not exist", name.name),
                )
            })
    }

    pub(crate) fn require_function_by_id(&self, id: FunctionId) -> Result<&FunctionSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.functions.values())
            .map(Arc::as_ref)
            .find(|function| function.id == id)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedFunction,
                    format!("function with id {} does not exist", id.0),
                )
            })
    }

    pub(crate) fn create_or_replace_function(
        &mut self,
        name: ResolvedRelationName,
        definition: ast::CreateFunction,
        body: ast::PlPgSqlBlock,
        replace: bool,
    ) -> Result<FunctionId> {
        let existing = self
            .get_schema_by_id(name.schema_id)
            .functions
            .get(&name.name)
            .cloned();
        if existing.is_some() && !replace {
            return Err(PgError::create(
                SqlState::DuplicateFunction,
                format!("function {}() already exists", name.name),
            ));
        }
        let id = existing.map_or_else(
            || {
                let id = FunctionId(self.next_function_id);
                self.next_function_id += 1;
                id
            },
            |function| function.id,
        );
        self.get_schema_by_id_mut(name.schema_id).functions.insert(
            name.name.clone(),
            Arc::new(FunctionSchema {
                id,
                schema_id: name.schema_id,
                name: name.name,
                definition,
                body,
            }),
        );
        Ok(id)
    }

    pub(crate) fn drop_function(&mut self, function: &FunctionSchema) {
        self.get_schema_by_id_mut(function.schema_id)
            .functions
            .remove(&function.name)
            .expect("required function must exist");
    }

    pub(crate) fn require_named_index(
        &self,
        name: &RelationName,
    ) -> Result<(&TableSchema, &IndexSchema)> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.tables.contains_key(&name.name) || schema.sequences.contains_key(&name.name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not an index", name.name),
            ));
        }
        schema
            .tables
            .values()
            .find_map(|table| {
                table
                    .indexes
                    .iter()
                    .find(|index| index.name == name.name)
                    .map(|index| (table.as_ref(), index))
            })
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedObject,
                    format!("index {:?} does not exist", name.name),
                )
            })
    }

    pub(crate) fn rename_column_dependencies(
        &mut self,
        table_id: TableId,
        old_name: &str,
        new_name: &str,
    ) {
        for table in Arc::make_mut(&mut self.relations)
            .schemas
            .values_mut()
            .flat_map(|schema| schema.tables.values_mut())
        {
            if table.id != table_id && !table.constraints.iter().any(|constraint| {
                matches!(constraint, Constraint::ForeignKey(foreign_key) if foreign_key.foreign_table_id == table_id)
            }) {
                continue;
            }
            let table = Arc::make_mut(table);
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
        for sequence in Arc::make_mut(&mut self.relations)
            .schemas
            .values_mut()
            .flat_map(|schema| schema.sequences.values_mut())
        {
            if sequence
                .owned_by
                .as_ref()
                .is_some_and(|(owner, column)| *owner == table_id && column == old_name)
            {
                Arc::make_mut(sequence)
                    .owned_by
                    .as_mut()
                    .expect("sequence owner was checked")
                    .1 = new_name.to_owned();
            }
        }
        self.rebuild_foreign_key_metadata();
    }

    pub(crate) fn rename_table_dependencies(&mut self, table_id: TableId, new_name: &str) {
        for table in Arc::make_mut(&mut self.relations)
            .schemas
            .values_mut()
            .flat_map(|schema| schema.tables.values_mut())
        {
            if !table.constraints.iter().any(|constraint| {
                matches!(constraint, Constraint::ForeignKey(foreign_key) if foreign_key.foreign_table_id == table_id)
            }) {
                continue;
            }
            let table = Arc::make_mut(table);
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
        self.iterate_shared_tables().map(Arc::as_ref)
    }

    pub(crate) fn iterate_shared_tables(&self) -> impl Iterator<Item = &Arc<TableSchema>> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.tables.values())
    }

    pub(crate) fn iterate_views(&self) -> impl Iterator<Item = &ViewSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.views.values())
            .map(Arc::as_ref)
    }

    pub(crate) fn iterate_functions(&self) -> impl Iterator<Item = &FunctionSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.functions.values())
            .map(Arc::as_ref)
    }

    pub(crate) fn iterate_views_mut(&mut self) -> impl Iterator<Item = &mut ViewSchema> {
        Arc::make_mut(&mut self.relations)
            .schemas
            .values_mut()
            .flat_map(|schema| schema.views.values_mut())
            .map(Arc::make_mut)
    }

    pub(crate) fn create_named_view(
        &mut self,
        name: ResolvedRelationName,
        columns: Vec<ViewColumn>,
        query: Box<ast::Query>,
        dependencies: BTreeSet<ViewDependency>,
        column_dependencies: BTreeMap<TableId, BTreeSet<String>>,
    ) -> Result<ViewId> {
        if self.has_resolved_relation(&name) {
            return Err(PgError::create(
                SqlState::DuplicateTable,
                format!("relation {:?} already exists", name.name),
            ));
        }
        let id = ViewId(self.next_view_id);
        self.next_view_id += 1;
        let previous = self.get_schema_by_id_mut(name.schema_id).views.insert(
            name.name.clone(),
            Arc::new(ViewSchema {
                id,
                schema_id: name.schema_id,
                name: name.name,
                columns,
                query,
                comment: None,
                dependencies,
                column_dependencies,
            }),
        );
        assert!(previous.is_none(), "new view must not replace a relation");
        Ok(id)
    }

    pub(crate) fn replace_view(&mut self, view: ViewSchema) -> Result<()> {
        let current = self
            .iterate_views()
            .find(|candidate| candidate.id == view.id)
            .cloned()
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation {:?} does not exist", view.name),
                )
            })?;
        self.get_schema_by_id_mut(current.schema_id)
            .views
            .remove(&current.name)
            .expect("required view must exist");
        let previous = self
            .get_schema_by_id_mut(view.schema_id)
            .views
            .insert(view.name.clone(), Arc::new(view));
        assert!(previous.is_none(), "replacement view name must be free");
        Ok(())
    }

    pub(crate) fn drop_named_views(&mut self, names: &[RelationName]) -> Result<Vec<ViewSchema>> {
        let targets = names
            .iter()
            .map(|name| self.require_named_view(name).cloned())
            .collect::<Result<Vec<_>>>()?;
        let target_ids = targets.iter().map(|view| view.id).collect::<BTreeSet<_>>();
        if let Some(dependent) = self.iterate_views().find(|view| {
            !target_ids.contains(&view.id)
                && view.dependencies.iter().any(
                    |dependency| matches!(dependency, ViewDependency::View(id) if target_ids.contains(id)),
                )
        }) {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                format!("cannot drop view because view {:?} depends on it", dependent.name),
            ));
        }
        Ok(targets
            .into_iter()
            .map(|view| {
                self.get_schema_by_id_mut(view.schema_id)
                    .views
                    .remove(&view.name)
                    .expect("required view must exist")
            })
            .map(Arc::unwrap_or_clone)
            .collect())
    }

    pub(crate) fn has_dependent_views_for_sequence(&self, sequence_id: SequenceId) -> bool {
        self.iterate_views().any(|view| {
            view.dependencies
                .contains(&ViewDependency::Sequence(sequence_id))
        })
    }

    pub(crate) fn has_dependent_views_for_constraint(&self, constraint_id: ConstraintId) -> bool {
        self.iterate_views().any(|view| {
            view.dependencies
                .contains(&ViewDependency::Constraint(constraint_id))
        })
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
            .map(Arc::unwrap_or_clone)
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
        if self.iterate_views().any(|view| {
            view.dependencies.iter().any(
                |dependency| matches!(dependency, ViewDependency::Table(id) if target_ids.contains(id)),
            )
        }) {
            return Err(PgError::create(
                SqlState::DependentObjectsStillExist,
                "cannot drop table because a view depends on it",
            ));
        }
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
            .map(Arc::unwrap_or_clone)
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
        Ok(Arc::unwrap_or_clone(dropped))
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
        self.relations
            .deferrable_foreign_keys
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
        self.relations
            .referencing_foreign_keys
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
        self.relations
            .referencing_foreign_keys
            .contains_key(&parent)
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
        let relations = Arc::make_mut(&mut self.relations);
        relations.deferrable_foreign_keys = deferrable_foreign_keys;
        relations.referencing_foreign_keys = referencing_foreign_keys;
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_relation(&self, name: &str) -> bool {
        let schema = self.get_default_schema();
        schema.tables.contains_key(name)
            || schema.views.contains_key(name)
            || schema.sequences.contains_key(name)
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
            .insert(sequence.name.clone(), Arc::new(sequence));
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
            .insert(sequence.name.clone(), Arc::new(sequence));
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
        schema.sequences.get(name).map(Arc::as_ref).ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            )
        })
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn iterate_sequences(&self) -> impl Iterator<Item = &SequenceSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.sequences.values())
            .map(Arc::as_ref)
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
        Ok(Arc::unwrap_or_clone(
            self.get_schema_by_id_mut(sequence.schema_id)
                .sequences
                .remove(&sequence.name)
                .expect("required sequence must exist"),
        ))
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn drop_owned_sequences(&mut self, table_id: TableId) -> Vec<SequenceSchema> {
        let names = self
            .relations
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
            .map(Arc::unwrap_or_clone)
            .collect()
    }

    pub(crate) fn drop_column_owned_sequences(
        &mut self,
        table_id: TableId,
        column_name: &str,
    ) -> Vec<SequenceSchema> {
        let names = self
            .relations
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
            .map(Arc::unwrap_or_clone)
            .collect()
    }
}

#[cfg(test)]
mod tests;
