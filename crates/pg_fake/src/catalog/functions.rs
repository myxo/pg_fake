use super::{Catalog, RelationName, ResolvedRelationName, SchemaId};
use crate::error::{PgError, Result, SqlState};
use sqlparser::ast;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FunctionId(pub(crate) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionSchema {
    pub(crate) id: FunctionId,
    pub(crate) schema_id: SchemaId,
    pub(crate) name: String,
    pub(crate) definition: ast::CreateFunction,
    pub(crate) body: ast::PlPgSqlBlock,
}

impl Catalog {
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

    pub(crate) fn iterate_functions(&self) -> impl Iterator<Item = &FunctionSchema> {
        self.relations
            .schemas
            .values()
            .flat_map(|schema| schema.functions.values())
            .map(Arc::as_ref)
    }
}
