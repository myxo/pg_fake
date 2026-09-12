use super::{
    Catalog, ConstraintId, RelationName, ResolvedRelationName, SchemaId, SequenceId, TableId,
};
use crate::{
    error::{PgError, Result, SqlState},
    value::PgType,
};
use sqlparser::ast;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ViewId(pub(crate) u64);

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

impl Catalog {
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

    pub(crate) fn iterate_views(&self) -> impl Iterator<Item = &ViewSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.views.values())
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
}
