use super::{Catalog, RelationName, ResolvedRelationName, SchemaId, TableId};
use crate::{
    error::{PgError, Result, SqlState},
    value::BaseType,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SequenceId(pub(crate) u64);

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

impl Catalog {
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
