use super::{
    Catalog, Constraint, ConstraintId, FunctionId, RelationName, ResolvedRelationName, SchemaId,
    ViewDependency,
};
use crate::{
    error::{PgError, Result, SqlState},
    value::PgType,
};
use sqlparser::ast;
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TableId(pub(crate) u64);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct IndexId(pub(crate) u64);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TriggerId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TriggerSchema {
    pub(crate) id: TriggerId,
    pub(crate) name: String,
    pub(crate) function_id: FunctionId,
    pub(crate) definition: ast::CreateTrigger,
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

impl Catalog {
    pub(crate) fn require_named_table(&self, name: &RelationName) -> Result<&TableSchema> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.sequences.contains_key(&name.name) || schema.views.contains_key(&name.name) {
            return Err(PgError::create(
                SqlState::WrongObjectType,
                format!("{:?} is not a table", name.name),
            ));
        }
        if schema.tables.values().any(|table| {
            table.indexes.iter().any(|index| index.name == name.name)
                || table.constraints.iter().any(|constraint| {
                    matches!(
                        constraint,
                        Constraint::PrimaryKey {
                            name: constraint_name,
                            ..
                        } | Constraint::Unique {
                            name: constraint_name,
                            ..
                        } if constraint_name == &name.name
                    )
                })
        }) {
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

        let mut index_names = BTreeSet::new();
        for constraint_name in constraints
            .iter()
            .filter_map(|constraint| match constraint {
                Constraint::PrimaryKey { name, .. } | Constraint::Unique { name, .. } => Some(name),
                Constraint::Check { .. } | Constraint::ForeignKey(_) => None,
            })
        {
            let index_name = ResolvedRelationName {
                schema_id: name.schema_id,
                name: constraint_name.clone(),
            };
            if index_name.name == name.name
                || self.has_resolved_relation(&index_name)
                || !index_names.insert(index_name.name.clone())
            {
                return Err(PgError::create(
                    SqlState::DuplicateTable,
                    format!("relation {:?} already exists", index_name.name),
                ));
            }
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

    pub(crate) fn require_named_index(
        &self,
        name: &RelationName,
    ) -> Result<(&TableSchema, &IndexSchema)> {
        let name = self.resolve_relation_name(name)?;
        let schema = self.get_schema_by_id(name.schema_id);
        if schema.tables.contains_key(&name.name)
            || schema.views.contains_key(&name.name)
            || schema.sequences.contains_key(&name.name)
            || self.resolve_constraint_index(&name).is_some()
        {
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
}
