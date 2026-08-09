use super::{DatabaseState, identifier_name, name};
use crate::{
    catalog::{Catalog, TableSchema},
    error::{PgError, Result, SqlState},
    value::PgType,
};
use sqlparser::ast::{Ident, TableFactor};

#[derive(Clone)]
pub(super) struct BoundColumn {
    pub(super) name: String,
    pub(super) data_type: PgType,
    pub(super) relation: String,
    pub(super) qualifier: String,
    pub(super) slot: usize,
}

#[derive(Clone)]
pub(crate) struct BoundScope {
    pub(super) columns: Vec<BoundColumn>,
    pub(super) relation: Option<String>,
    pub(super) qualifier: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum RowScope<'a> {
    Table(&'a TableSchema),
    Bound(&'a BoundScope),
}

impl RowScope<'_> {
    pub(super) fn resolve_column(self, identifiers: &[Ident]) -> Result<(usize, PgType)> {
        match self {
            RowScope::Table(schema) => {
                if identifiers.len() != 1 {
                    return Err(PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", identifiers),
                    ));
                }
                let index = schema
                    .columns
                    .iter()
                    .position(|column| column.name == identifier_name(&identifiers[0]))
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", identifiers[0].value),
                        )
                    })?;
                Ok((index, schema.columns[index].data_type))
            }
            RowScope::Bound(scope) => {
                let names = identifiers.iter().map(identifier_name).collect::<Vec<_>>();
                let matches = match names.as_slice() {
                    [column] => scope
                        .columns
                        .iter()
                        .filter(|bound| bound.name == *column)
                        .collect::<Vec<_>>(),
                    [qualifier, column] => scope
                        .columns
                        .iter()
                        .filter(|bound| bound.qualifier == *qualifier && bound.name == *column)
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                match matches.as_slice() {
                    [] if names.len() == 2
                        && scope.qualifier.as_deref() != Some(names[0].as_str()) =>
                    {
                        Err(PgError::new(
                            SqlState::UndefinedTable,
                            format!("missing FROM-clause entry for table {:?}", names[0]),
                        ))
                    }
                    [] => Err(PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", identifiers),
                    )),
                    [column] => Ok((column.slot, column.data_type)),
                    _ => Err(PgError::new(
                        SqlState::AmbiguousColumn,
                        format!("column {:?} is ambiguous", identifiers),
                    )),
                }
            }
        }
    }
}

impl BoundScope {
    pub(super) fn resolve_column(&self, identifiers: &[Ident]) -> Result<(usize, PgType)> {
        RowScope::Bound(self).resolve_column(identifiers)
    }

    pub(super) fn source_relation(&self) -> Option<&str> {
        self.columns
            .first()
            .map(|column| column.relation.as_str())
            .or(self.relation.as_deref())
    }

    fn bind_table(
        schema: &TableSchema,
        alias: Option<&sqlparser::ast::TableAlias>,
    ) -> Result<Self> {
        let qualifier = alias
            .map(|alias| identifier_name(&alias.name))
            .unwrap_or_else(|| schema.name.clone());
        if alias.is_some_and(|alias| alias.columns.len() > schema.columns.len()) {
            return Err(PgError::new(
                SqlState::InvalidColumnReference,
                "table has fewer columns than specified in the column alias list",
            ));
        }
        Ok(BoundScope {
            relation: Some(schema.name.clone()),
            qualifier: Some(qualifier.clone()),
            columns: schema
                .columns
                .iter()
                .enumerate()
                .map(|(slot, column)| BoundColumn {
                    name: alias
                        .and_then(|alias| alias.columns.get(slot))
                        .map(|alias| identifier_name(&alias.name))
                        .unwrap_or_else(|| column.name.clone()),
                    data_type: column.data_type,
                    relation: schema.name.clone(),
                    qualifier: qualifier.clone(),
                    slot,
                })
                .collect(),
        })
    }
}

pub(crate) fn bind_query_scope(
    catalog: &Catalog,
    select: &sqlparser::ast::Select,
) -> Result<BoundScope> {
    if select.from.is_empty() {
        return Ok(BoundScope {
            columns: Vec::new(),
            relation: None,
            qualifier: None,
        });
    }
    if select.from.len() != 1 || !select.from[0].joins.is_empty() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "joins are not implemented",
        ));
    }
    let TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = &select.from[0].relation
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "FROM source is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "table functions are not implemented",
        ));
    }
    BoundScope::bind_table(catalog.table(&name(table_name)?)?, alias.as_ref())
}

pub(super) fn bind_select_scope(
    state: &DatabaseState,
    select: &sqlparser::ast::Select,
) -> Result<BoundScope> {
    bind_query_scope(&state.catalog, select)
}
