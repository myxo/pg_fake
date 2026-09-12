use std::{borrow::Cow, collections::BTreeSet};

use sqlparser::ast::{self, VisitMut as _};

use crate::{
    QueryResult,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        DatabaseState, normalize_identifier, normalize_unqualified_object_name,
        query::{describe_query_result_columns, detect_statement_features},
    },
};

use super::{
    convert_query_to_statement, is_data_modifying_query,
    recursive::{
        create_set_expression_query, validate_recursive_cte, validate_recursive_cte_types,
    },
    references::{collect_cte_references, create_cte_values_query, reject_cte_forward_references},
};

#[derive(Clone)]
struct InlineCte {
    name: String,
    query: Box<ast::Query>,
    alias: ast::TableAlias,
    masked_names: Vec<String>,
}

struct InlineCteReferenceReplacer<'a> {
    state: &'a DatabaseState,
    ctes: &'a [InlineCte],
    masked: Vec<Vec<String>>,
    pending_mask: Option<Vec<String>>,
    error: Option<PgError>,
}

impl ast::VisitorMut for InlineCteReferenceReplacer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked
            .push(self.pending_mask.take().unwrap_or_default());
        if query.with.is_none() {
            return std::ops::ControlFlow::Continue(());
        }
        match inline_query_ctes(query, self.state) {
            Ok(expanded) => *query = expanded,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.pop().expect("visited query pushed CTE mask");
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = normalize_unqualified_object_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(cte) = self.ctes.iter().rev().find(|cte| {
            cte.name == name && !self.masked.iter().any(|masked| masked.contains(&name))
        }) else {
            return std::ops::ControlFlow::Continue(());
        };
        let alias = match alias {
            Some(alias) if alias.columns.is_empty() => ast::TableAlias {
                columns: cte.alias.columns.clone(),
                ..alias.clone()
            },
            Some(alias) => alias.clone(),
            None => cte.alias.clone(),
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: cte.query.clone(),
            alias: Some(alias),
            sample: None,
        };
        self.pending_mask = Some(cte.masked_names.clone());
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn inline_query_ctes(query: &ast::Query, state: &DatabaseState) -> Result<ast::Query> {
    let mut query = query.clone();
    let Some(with) = query.with.take() else {
        let mut replacer = InlineCteReferenceReplacer {
            state,
            ctes: &[],
            masked: Vec::new(),
            pending_mask: None,
            error: None,
        };
        let _ = query.visit(&mut replacer);
        if let Some(error) = replacer.error {
            return Err(error);
        }
        return Ok(query);
    };
    if with.recursive {
        return inline_recursive_query_ctes(query, with, state);
    }
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut ctes = Vec::new();
    for (index, cte) in with.cte_tables.into_iter().enumerate() {
        let name = normalize_identifier(&cte.alias.name);
        if ctes
            .iter()
            .any(|existing: &InlineCte| existing.name == name)
        {
            return Err(PgError::create(
                SqlState::SyntaxError,
                format!("WITH query name {name:?} specified more than once"),
            ));
        }
        let mut cte_query = inline_query_ctes(&cte.query, state)?;
        reject_cte_forward_references(&cte_query, &names[index..], &state.catalog)?;
        let mut replacer = InlineCteReferenceReplacer {
            state,
            ctes: &ctes,
            masked: Vec::new(),
            pending_mask: None,
            error: None,
        };
        let _ = cte_query.visit(&mut replacer);
        if let Some(error) = replacer.error {
            return Err(error);
        }
        let mut alias = cte.alias;
        if is_data_modifying_query(&cte_query) {
            let statement = convert_query_to_statement(cte_query);
            let columns = describe_query_result_columns(state, &statement)?;
            if alias.columns.is_empty() {
                alias.columns = columns
                    .iter()
                    .map(|column| ast::TableAliasColumnDef {
                        name: ast::Ident::with_quote('"', column.name.clone()),
                        data_type: None,
                    })
                    .collect();
            }
            cte_query = create_cte_values_query(&QueryResult {
                columns,
                rows: Vec::new(),
            });
        }
        ctes.push(InlineCte {
            name,
            query: Box::new(cte_query),
            alias,
            masked_names: names[index..].to_vec(),
        });
    }
    let mut replacer = InlineCteReferenceReplacer {
        state,
        ctes: &ctes,
        masked: Vec::new(),
        pending_mask: None,
        error: None,
    };
    let _ = query.visit(&mut replacer);
    if let Some(error) = replacer.error {
        return Err(error);
    }
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn inline_recursive_query_ctes(
    mut query: ast::Query,
    with: ast::With,
    state: &DatabaseState,
) -> Result<ast::Query> {
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    for name in &names {
        if !seen.insert(name.clone()) {
            return Err(PgError::create(
                SqlState::SyntaxError,
                format!("WITH query name {name:?} specified more than once"),
            ));
        }
    }
    let mut pending = with.cte_tables.into_iter().map(Some).collect::<Vec<_>>();
    let mut ctes = Vec::new();
    while ctes.len() < pending.len() {
        let mut progressed = false;
        for index in 0..pending.len() {
            let Some(cte) = pending[index].as_ref() else {
                continue;
            };
            let name = &names[index];
            let dependencies = collect_cte_references(&cte.query, &names);
            if dependencies.iter().any(|dependency| {
                dependency != name && !ctes.iter().any(|cte: &InlineCte| &cte.name == dependency)
            }) {
                continue;
            }
            let cte = pending[index]
                .take()
                .expect("pending CTE was checked as present");
            let mut cte_query = inline_query_ctes(&cte.query, state)?;
            let mut replacer = InlineCteReferenceReplacer {
                state,
                ctes: &ctes,
                masked: Vec::new(),
                pending_mask: None,
                error: None,
            };
            let _ = cte_query.visit(&mut replacer);
            if let Some(error) = replacer.error {
                return Err(error);
            }
            if validate_recursive_cte(&cte_query, name)? {
                let ast::SetExpr::SetOperation { left, .. } = cte_query.body.as_ref() else {
                    unreachable!("recursive CTE shape was validated");
                };
                let seed = InlineCte {
                    name: name.clone(),
                    query: Box::new(create_set_expression_query((**left).clone())),
                    alias: cte.alias.clone(),
                    masked_names: names.clone(),
                };
                let mut replacer = InlineCteReferenceReplacer {
                    state,
                    ctes: std::slice::from_ref(&seed),
                    masked: Vec::new(),
                    pending_mask: None,
                    error: None,
                };
                let _ = cte_query.visit(&mut replacer);
                if let Some(error) = replacer.error {
                    return Err(error);
                }
                validate_recursive_cte_types(&state.catalog, &cte_query)?;
            }
            ctes.push(InlineCte {
                name: name.clone(),
                query: Box::new(cte_query),
                alias: cte.alias,
                masked_names: names.clone(),
            });
            progressed = true;
        }
        if !progressed {
            return reject_unsupported("mutual recursion between WITH items is not implemented");
        }
    }
    let mut replacer = InlineCteReferenceReplacer {
        state,
        ctes: &ctes,
        masked: Vec::new(),
        pending_mask: None,
        error: None,
    };
    let _ = query.visit(&mut replacer);
    if let Some(error) = replacer.error {
        return Err(error);
    }
    Ok(query)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn expand_ctes_for_analysis<'a>(
    statement: &'a ast::Statement,
    state: &DatabaseState,
) -> Result<(Cow<'a, ast::Statement>, Vec<ast::Statement>)> {
    let ast::Statement::Query(query) = statement else {
        return Ok((Cow::Borrowed(statement), Vec::new()));
    };
    if query.with.is_none()
        && !matches!(
            query.body.as_ref(),
            ast::SetExpr::Insert(_) | ast::SetExpr::Update(_) | ast::SetExpr::Delete(_)
        )
        && !detect_statement_features(statement).0
    {
        return Ok((Cow::Borrowed(statement), Vec::new()));
    }
    let mut mutations = Vec::new();
    if let Some(with) = &query.with
        && with.recursive
        && with
            .cte_tables
            .iter()
            .any(|cte| is_data_modifying_query(&cte.query))
    {
        let mut query = query.as_ref().clone();
        let with = query
            .with
            .take()
            .expect("WITH clause was checked as present");
        let names = with
            .cte_tables
            .iter()
            .map(|cte| normalize_identifier(&cte.alias.name))
            .collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        for name in &names {
            if !seen.insert(name.clone()) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    format!("WITH query name {name:?} specified more than once"),
                ));
            }
        }
        let mut pending = with.cte_tables.into_iter().map(Some).collect::<Vec<_>>();
        let mut ctes = Vec::new();
        while ctes.len() < pending.len() {
            let mut progressed = false;
            for index in 0..pending.len() {
                let Some(cte) = pending[index].as_ref() else {
                    continue;
                };
                let name = &names[index];
                let dependencies = collect_cte_references(&cte.query, &names);
                if dependencies.iter().any(|dependency| {
                    dependency != name
                        && !ctes.iter().any(|cte: &InlineCte| &cte.name == dependency)
                }) {
                    continue;
                }
                let cte = pending[index]
                    .take()
                    .expect("pending CTE was checked as present");
                let modifying = is_data_modifying_query(&cte.query);
                if modifying && dependencies.contains(name) {
                    return Err(PgError::create(
                        SqlState::InvalidRecursion,
                        "recursive query must not contain data-modifying statements",
                    ));
                }
                let mut cte_query = inline_query_ctes(&cte.query, state)?;
                let mut replacer = InlineCteReferenceReplacer {
                    state,
                    ctes: &ctes,
                    masked: Vec::new(),
                    pending_mask: None,
                    error: None,
                };
                let _ = cte_query.visit(&mut replacer);
                if let Some(error) = replacer.error {
                    return Err(error);
                }
                let mut alias = cte.alias;
                if modifying {
                    let mutation = convert_query_to_statement(cte_query.clone());
                    let columns = describe_query_result_columns(state, &mutation)?;
                    if alias.columns.is_empty() {
                        alias.columns = columns
                            .iter()
                            .map(|column| ast::TableAliasColumnDef {
                                name: ast::Ident::with_quote('"', column.name.clone()),
                                data_type: None,
                            })
                            .collect();
                    }
                    mutations.push(mutation);
                    cte_query = create_cte_values_query(&QueryResult {
                        columns,
                        rows: Vec::new(),
                    });
                } else if validate_recursive_cte(&cte_query, name)? {
                    let ast::SetExpr::SetOperation { left, .. } = cte_query.body.as_ref() else {
                        unreachable!("recursive CTE shape was validated");
                    };
                    let seed = InlineCte {
                        name: name.clone(),
                        query: Box::new(create_set_expression_query((**left).clone())),
                        alias: alias.clone(),
                        masked_names: names.clone(),
                    };
                    let mut replacer = InlineCteReferenceReplacer {
                        state,
                        ctes: std::slice::from_ref(&seed),
                        masked: Vec::new(),
                        pending_mask: None,
                        error: None,
                    };
                    let _ = cte_query.visit(&mut replacer);
                    if let Some(error) = replacer.error {
                        return Err(error);
                    }
                    validate_recursive_cte_types(&state.catalog, &cte_query)?;
                }
                ctes.push(InlineCte {
                    name: name.clone(),
                    query: Box::new(cte_query),
                    alias,
                    masked_names: names.clone(),
                });
                progressed = true;
            }
            if !progressed {
                return reject_unsupported(
                    "mutual recursion between WITH items is not implemented",
                );
            }
        }
        let mut replacer = InlineCteReferenceReplacer {
            state,
            ctes: &ctes,
            masked: Vec::new(),
            pending_mask: None,
            error: None,
        };
        let _ = query.visit(&mut replacer);
        if let Some(error) = replacer.error {
            return Err(error);
        }
        return Ok((Cow::Owned(convert_query_to_statement(query)), mutations));
    }
    if let Some(with) = &query.with
        && !with.recursive
        && with
            .cte_tables
            .iter()
            .any(|cte| is_data_modifying_query(&cte.query))
    {
        let names = with
            .cte_tables
            .iter()
            .map(|cte| normalize_identifier(&cte.alias.name))
            .collect::<Vec<_>>();
        let mut ctes = Vec::new();
        for (index, cte) in with.cte_tables.iter().enumerate() {
            let name = normalize_identifier(&cte.alias.name);
            let mut cte_query = inline_query_ctes(&cte.query, state)?;
            reject_cte_forward_references(&cte_query, &names[index..], &state.catalog)?;
            let mut replacer = InlineCteReferenceReplacer {
                state,
                ctes: &ctes,
                masked: Vec::new(),
                pending_mask: None,
                error: None,
            };
            let _ = cte_query.visit(&mut replacer);
            if let Some(error) = replacer.error {
                return Err(error);
            }
            let mut alias = cte.alias.clone();
            if is_data_modifying_query(&cte_query) {
                let mutation = convert_query_to_statement(cte_query.clone());
                let columns = describe_query_result_columns(state, &mutation)?;
                if alias.columns.is_empty() {
                    alias.columns = columns
                        .iter()
                        .map(|column| ast::TableAliasColumnDef {
                            name: ast::Ident::with_quote('"', column.name.clone()),
                            data_type: None,
                        })
                        .collect();
                }
                mutations.push(mutation);
                cte_query = create_cte_values_query(&QueryResult {
                    columns,
                    rows: Vec::new(),
                });
            }
            ctes.push(InlineCte {
                name,
                query: Box::new(cte_query),
                alias,
                masked_names: names[index..].to_vec(),
            });
        }
    }
    Ok((
        Cow::Owned(convert_query_to_statement(inline_query_ctes(query, state)?)),
        mutations,
    ))
}
