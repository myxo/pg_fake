use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use crate::error::{PgError, Result, SqlState, reject_unsupported};

mod constraints;
mod functions;
mod history;
mod names;
mod sequences;
mod tables;
mod views;

pub(crate) use constraints::{Constraint, ConstraintId, ForeignKey, ForeignKeyAction};
pub(crate) use functions::{FunctionId, FunctionSchema};
pub(crate) use names::{RelationName, ResolvedRelationName};
pub(crate) use sequences::{SequenceId, SequenceSchema};
pub(crate) use tables::{
    ColumnDef, IdentityKind, IndexColumnDefinition, IndexId, IndexSchema, TableId,
    TablePersistence, TableSchema, TriggerSchema,
};
pub(crate) use views::{ViewColumn, ViewDependency, ViewId, ViewSchema};

pub(crate) use history::{CatalogHistory, CatalogVisibility};

pub(crate) const DEFAULT_SCHEMA: &str = "public";
pub(crate) const TEMP_SCHEMA: &str = "pg_temp";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SchemaId(pub(crate) u64);

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
    pub(crate) search_path: Vec<String>,
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
            search_path: vec![DEFAULT_SCHEMA.into()],
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

    pub(crate) fn set_search_path(&mut self, search_path: &[String]) {
        self.search_path = search_path.to_vec();
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

    pub(crate) fn iterate_schemas(&self) -> impl Iterator<Item = &Schema> {
        self.relations.schemas.values()
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
}

#[cfg(test)]
mod tests;
