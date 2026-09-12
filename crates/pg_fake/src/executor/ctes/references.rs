use std::collections::BTreeSet;

use sqlparser::ast::{self, VisitMut as _};

use crate::{
    QueryResult,
    catalog::{Catalog, RelationName},
    error::{PgError, Result, SqlState},
    executor::{normalize_identifier, normalize_unqualified_object_name},
    value::{BaseType, PgType, Value},
};

use super::{MaterializedCte, is_data_modifying_query};

struct CteForwardReferenceDetector<'a> {
    catalog: &'a Catalog,
    names: &'a [String],
    error: Option<PgError>,
}

impl ast::VisitorMut for CteForwardReferenceDetector<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Table {
            name, args: None, ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = normalize_unqualified_object_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if self.names.contains(&name)
            && self
                .catalog
                .require_named_table(&RelationName::create_unqualified(name.clone()))
                .is_err()
        {
            self.error = Some(PgError::create(
                SqlState::UndefinedTable,
                format!("relation {name:?} does not exist"),
            ));
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn reject_cte_forward_references(
    query: &ast::Query,
    names: &[String],
    catalog: &Catalog,
) -> Result<()> {
    let mut query = query.clone();
    let mut detector = CteForwardReferenceDetector {
        catalog,
        names,
        error: None,
    };
    let _ = query.visit(&mut detector);
    detector.error.map_or(Ok(()), Err)
}

struct CteReferenceCollector<'a> {
    names: &'a [String],
    masked: Vec<Vec<String>>,
    found: BTreeSet<String>,
}

impl ast::VisitorMut for CteReferenceCollector<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.push(
            query
                .with
                .as_ref()
                .map(|with| {
                    with.cte_tables
                        .iter()
                        .map(|cte| normalize_identifier(&cte.alias.name))
                        .collect()
                })
                .unwrap_or_default(),
        );
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
            name, args: None, ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(name) = normalize_unqualified_object_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if self.names.contains(&name) && !self.masked.iter().any(|masked| masked.contains(&name)) {
            self.found.insert(name);
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn collect_cte_references(
    query: &ast::Query,
    names: &[String],
) -> BTreeSet<String> {
    let mut query = query.clone();
    let mut collector = CteReferenceCollector {
        names,
        masked: Vec::new(),
        found: BTreeSet::new(),
    };
    let _ = query.visit(&mut collector);
    collector.found
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn collect_reachable_cte_names(query: &ast::Query) -> BTreeSet<String> {
    let Some(with) = &query.with else {
        return BTreeSet::new();
    };
    let names = with
        .cte_tables
        .iter()
        .map(|cte| normalize_identifier(&cte.alias.name))
        .collect::<Vec<_>>();
    let mut body = query.clone();
    body.with = None;
    let mut reachable = collect_cte_references(&body, &names);
    reachable.extend(
        with.cte_tables
            .iter()
            .filter(|cte| is_data_modifying_query(&cte.query))
            .map(|cte| normalize_identifier(&cte.alias.name)),
    );
    for (index, cte) in with.cte_tables.iter().enumerate().rev() {
        if reachable.contains(&names[index]) {
            reachable.extend(collect_cte_references(&cte.query, &names[..index]));
        }
    }
    reachable
}

pub(super) fn replace_cte_references(query: &mut ast::Query, ctes: &[MaterializedCte]) {
    let _ = query.visit(&mut CteReferenceReplacer {
        ctes,
        masked: Vec::new(),
    });
}

struct CteReferenceReplacer<'a> {
    ctes: &'a [MaterializedCte],
    masked: Vec<Vec<String>>,
}

impl ast::VisitorMut for CteReferenceReplacer<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.push(
            query
                .with
                .as_ref()
                .map(|with| {
                    with.cte_tables
                        .iter()
                        .map(|cte| normalize_identifier(&cte.alias.name))
                        .collect()
                })
                .unwrap_or_default(),
        );
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
        let columns = if cte.alias.columns.is_empty() {
            cte.result
                .columns
                .iter()
                .map(|column| ast::TableAliasColumnDef {
                    name: ast::Ident::with_quote('"', column.name.clone()),
                    data_type: None,
                })
                .collect::<Vec<_>>()
        } else {
            cte.alias.columns.clone()
        };
        let alias = match alias {
            Some(alias) if alias.columns.is_empty() => ast::TableAlias {
                columns,
                ..alias.clone()
            },
            Some(alias) => alias.clone(),
            None => ast::TableAlias {
                name: cte.alias.name.clone(),
                columns,
                ..cte.alias.clone()
            },
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: Box::new(create_cte_values_query(&cte.result)),
            alias: Some(alias),
            sample: None,
        };
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn create_cte_values_query(result: &QueryResult) -> ast::Query {
    let rows = if result.rows.is_empty() {
        let row = result
            .columns
            .iter()
            .map(|column| {
                crate::analyzer::create_typed_literal(
                    Value::Null,
                    PgType::create_with_typmod(
                        BaseType::resolve_oid(column.type_oid)
                            .expect("CTE result type OID is supported"),
                        column.typmod,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let values = ast::Values {
            explicit_row: false,
            value_keyword: false,
            rows: vec![ast::Parens::with_empty_span(row.clone())],
        };
        ast::SetExpr::SetOperation {
            op: ast::SetOperator::Except,
            set_quantifier: ast::SetQuantifier::All,
            left: Box::new(ast::SetExpr::Values(values.clone())),
            right: Box::new(ast::SetExpr::Values(values)),
        }
    } else {
        ast::SetExpr::Values(ast::Values {
            explicit_row: false,
            value_keyword: false,
            rows: result
                .rows
                .iter()
                .map(|row| {
                    ast::Parens::with_empty_span(
                        row.iter()
                            .zip(&result.columns)
                            .map(|(value, column)| {
                                crate::analyzer::create_typed_literal(
                                    value.clone(),
                                    PgType::create_with_typmod(
                                        BaseType::resolve_oid(column.type_oid)
                                            .expect("CTE result type OID is supported"),
                                        column.typmod,
                                    ),
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        })
    };
    ast::Query {
        with: None,
        body: Box::new(rows),
        order_by: None,
        limit_clause: None,
        fetch: None,
        locks: Vec::new(),
        for_clause: None,
        settings: None,
        format_clause: None,
        pipe_operators: Vec::new(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn contains_query_ctes(query: &ast::Query) -> bool {
    struct CteDetector;

    impl ast::Visitor for CteDetector {
        type Break = ();

        fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<()> {
            if query.with.is_some() {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
    }

    ast::Visit::visit(query, &mut CteDetector).is_break()
}
