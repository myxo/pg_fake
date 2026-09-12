use crate::executor::normalize_identifier;
use crate::{
    catalog::{TableId, TableSchema},
    error::{PgError, Result, SqlState},
    value::PgType,
};
use sqlparser::ast;

mod joins;
mod output;
mod sources;
mod subqueries;

pub(crate) use joins::bind_join;
pub(super) use joins::bind_table_with_joins;
pub(crate) use output::{
    identify_unknown_query_columns, identify_unknown_set_operand_columns,
    infer_query_output_columns,
};
pub(super) use sources::bind_select_scope;
pub(crate) use sources::{
    bind_from_scope, bind_query_scope, bind_query_scope_with_outer, bind_table_factor,
    bind_table_factor_scope,
};
pub(super) use subqueries::infer_expression_data_type;
pub(crate) use subqueries::substitute_typed_subqueries;

#[derive(Clone)]
pub(super) struct BoundColumn {
    pub(super) name: String,
    pub(super) data_type: PgType,
    pub(super) qualifier: String,
    pub(super) slot: usize,
    pub(super) output_order: usize,
    pub(super) qualified_order: usize,
    pub(super) qualified_merged: Option<Vec<usize>>,
    pub(super) merged: Option<Vec<usize>>,
    pub(super) unqualified: bool,
    pub(super) wildcard: bool,
    pub(super) depth: usize,
    pub(super) table_id: Option<TableId>,
    pub(super) source_name: String,
}

#[derive(Clone)]
pub(crate) struct BoundScope {
    pub(super) columns: Vec<BoundColumn>,
}

#[derive(Clone, Copy)]
pub(crate) enum RowScope<'a> {
    Table(&'a TableSchema),
    Bound(&'a BoundScope),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn matches_identifier(name: &str, identifier: &ast::Ident) -> bool {
    if identifier.quote_style.is_some() {
        name == identifier.value
    } else {
        name.len() == identifier.value.len()
            && name
                .bytes()
                .zip(identifier.value.bytes())
                .all(|(name, identifier)| name == identifier.to_ascii_lowercase())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_bound_column<'a>(
    scope: &'a BoundScope,
    identifiers: &[ast::Ident],
) -> Result<&'a BoundColumn> {
    let mut selected = None;
    let mut ambiguous = false;
    match identifiers {
        [identifier] => {
            for column in &scope.columns {
                if !column.unqualified || !matches_identifier(&column.name, identifier) {
                    continue;
                }
                match selected {
                    None => selected = Some(column),
                    Some(current) if column.depth < current.depth => {
                        selected = Some(column);
                        ambiguous = false;
                    }
                    Some(current) if column.depth == current.depth => ambiguous = true,
                    Some(_) => {}
                }
            }
        }
        [qualifier, identifier] => {
            let depth = scope
                .columns
                .iter()
                .filter(|column| matches_identifier(&column.qualifier, qualifier))
                .map(|column| column.depth)
                .min()
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedTable,
                        format!(
                            "missing FROM-clause entry for table {:?}",
                            normalize_identifier(qualifier)
                        ),
                    )
                })?;
            for column in &scope.columns {
                if column.depth != depth
                    || !matches_identifier(&column.qualifier, qualifier)
                    || !matches_identifier(&column.name, identifier)
                {
                    continue;
                }
                if selected.is_some() {
                    ambiguous = true;
                } else {
                    selected = Some(column);
                }
            }
        }
        _ => {}
    }
    if ambiguous {
        return Err(PgError::create(
            SqlState::AmbiguousColumn,
            format!("column {:?} is ambiguous", identifiers),
        ));
    }
    selected.ok_or_else(|| {
        PgError::create(
            SqlState::UndefinedColumn,
            format!("column {:?} does not exist", identifiers),
        )
    })
}

impl RowScope<'_> {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn resolve_column(self, identifiers: &[ast::Ident]) -> Result<(usize, PgType)> {
        match self {
            RowScope::Table(schema) => {
                if identifiers.len() != 1 {
                    return Err(PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", identifiers),
                    ));
                }
                let index = schema
                    .columns
                    .iter()
                    .position(|column| matches_identifier(&column.name, &identifiers[0]))
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", identifiers[0].value),
                        )
                    })?;
                Ok((index, schema.columns[index].data_type))
            }
            RowScope::Bound(scope) => {
                let column = resolve_bound_column(scope, identifiers)?;
                Ok((column.slot, column.data_type))
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn resolve_column_value(
        self,
        identifiers: &[ast::Ident],
        row: &[crate::value::Value],
    ) -> Result<crate::value::Value> {
        match self {
            RowScope::Table(_) => Ok(row[self.resolve_column(identifiers)?.0].clone()),
            RowScope::Bound(scope) => {
                let column = resolve_bound_column(scope, identifiers)?;
                let merged = if identifiers.len() == 1 {
                    &column.merged
                } else {
                    &column.qualified_merged
                };
                if let Some(slots) = merged {
                    let value = slots
                        .iter()
                        .map(|slot| &row[*slot])
                        .find(|value| !value.is_null())
                        .cloned()
                        .unwrap_or(crate::value::Value::Null);
                    if value.is_null() {
                        return Ok(value);
                    }
                    return crate::coercion::coerce(
                        value.clone(),
                        value
                            .get_base_type()
                            .expect("non-null value has a base type"),
                        column.data_type,
                        crate::coercion::CastContext::Implicit,
                    );
                }
                Ok(row[column.slot].clone())
            }
        }
    }
}

impl BoundScope {
    pub(super) fn select_wildcard_columns(&self, qualifier: Option<&str>) -> Vec<&BoundColumn> {
        let mut columns = self
            .columns
            .iter()
            .filter(|column| match qualifier {
                Some(qualifier) => column.qualifier == qualifier && column.depth == 0,
                None => column.wildcard,
            })
            .collect::<Vec<_>>();
        columns.sort_by_key(|column| {
            if qualifier.is_some() {
                column.qualified_order
            } else {
                column.output_order
            }
        });
        columns
    }
    pub(crate) fn count_columns(&self) -> usize {
        self.columns.len()
    }
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn resolve_column(&self, identifiers: &[ast::Ident]) -> Result<(usize, PgType)> {
        RowScope::Bound(self).resolve_column(identifiers)
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_target_scope(schema: &TableSchema, alias: Option<&ast::Ident>) -> BoundScope {
    let qualifier = alias
        .map(normalize_identifier)
        .unwrap_or_else(|| schema.name.clone());
    BoundScope {
        columns: schema
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| BoundColumn {
                name: column.name.clone(),
                data_type: column.data_type,
                qualifier: qualifier.clone(),
                slot: index,
                output_order: index,
                qualified_order: index,
                qualified_merged: None,
                merged: None,
                unqualified: true,
                wildcard: true,
                depth: 0,
                table_id: Some(schema.id),
                source_name: column.name.clone(),
            })
            .collect(),
    }
}

pub(crate) fn create_value_scope(columns: impl Iterator<Item = (String, PgType)>) -> BoundScope {
    BoundScope {
        columns: columns
            .enumerate()
            .map(|(slot, (name, data_type))| BoundColumn {
                source_name: name.clone(),
                name,
                data_type,
                qualifier: String::new(),
                slot,
                output_order: slot,
                qualified_order: slot,
                qualified_merged: None,
                merged: None,
                unqualified: true,
                wildcard: false,
                depth: 0,
                table_id: None,
            })
            .collect(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn combine_bound_scopes(mut target: BoundScope, mut source: BoundScope) -> BoundScope {
    let start = target.columns.len();
    for column in &mut source.columns {
        column.slot += start;
        for slots in [&mut column.merged, &mut column.qualified_merged]
            .into_iter()
            .flatten()
        {
            for slot in slots {
                *slot += start;
            }
        }
        column.output_order += start;
        column.qualified_order += start;
    }
    target.columns.extend(source.columns);
    target
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn try_resolve_column_reference(
    expression: &ast::Expr,
    scope: &BoundScope,
) -> Option<(usize, PgType)> {
    match expression {
        ast::Expr::Identifier(identifier) => {
            scope.resolve_column(std::slice::from_ref(identifier)).ok()
        }
        ast::Expr::CompoundIdentifier(identifiers) => scope.resolve_column(identifiers).ok(),
        _ => None,
    }
}
