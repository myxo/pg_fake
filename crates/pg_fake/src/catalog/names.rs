use super::{Catalog, Constraint, ConstraintId, Schema, SchemaId, TEMP_SCHEMA, TableId};
use crate::error::{PgError, Result, SqlState};

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

impl Catalog {
    pub(crate) fn resolve_relation_name(
        &self,
        name: &RelationName,
    ) -> Result<ResolvedRelationName> {
        let schema = match &name.schema {
            Some(schema) => self.require_schema(schema)?,
            None => {
                let contains = |schema: &&Schema| schema_has_relation(schema, &name.name);
                let implicit_temp = (!self.search_path.iter().any(|schema| schema == TEMP_SCHEMA))
                    .then(|| self.relations.schemas.get(TEMP_SCHEMA))
                    .flatten()
                    .filter(contains);
                implicit_temp
                    .or_else(|| {
                        self.search_path
                            .iter()
                            .filter_map(|name| self.relations.schemas.get(name))
                            .find(contains)
                    })
                    .or_else(|| {
                        self.search_path
                            .iter()
                            .find_map(|name| self.relations.schemas.get(name))
                    })
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedTable,
                            format!("relation {:?} does not exist", name.name),
                        )
                    })?
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
                None => self
                    .search_path
                    .iter()
                    .find_map(|name| self.relations.schemas.get(name))
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::InvalidSchemaName,
                            "no schema has been selected to create in",
                        )
                    })?,
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
        schema_has_relation(schema, &name.name)
    }

    pub(crate) fn resolve_constraint_index(
        &self,
        name: &ResolvedRelationName,
    ) -> Option<(TableId, ConstraintId)> {
        self.get_schema_by_id(name.schema_id)
            .tables
            .values()
            .find_map(|table| {
                table
                    .constraints
                    .iter()
                    .find_map(|constraint| match constraint {
                        Constraint::PrimaryKey {
                            id,
                            name: constraint_name,
                            ..
                        }
                        | Constraint::Unique {
                            id,
                            name: constraint_name,
                            ..
                        } if constraint_name == &name.name => Some((table.id, *id)),
                        _ => None,
                    })
            })
    }

    #[cfg(test)]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_relation(&self, name: &str) -> bool {
        let schema = self.get_default_schema();
        schema_has_relation(schema, name)
    }
}

fn schema_has_relation(schema: &Schema, name: &str) -> bool {
    schema.tables.contains_key(name)
        || schema.views.contains_key(name)
        || schema.sequences.contains_key(name)
        || schema.tables.values().any(|table| {
            table.indexes.iter().any(|index| index.name == name)
                || table.constraints.iter().any(|constraint| {
                    matches!(
                        constraint,
                        Constraint::PrimaryKey {
                            name: constraint_name,
                            ..
                        } | Constraint::Unique {
                            name: constraint_name,
                            ..
                        } if constraint_name == name
                    )
                })
        })
}
