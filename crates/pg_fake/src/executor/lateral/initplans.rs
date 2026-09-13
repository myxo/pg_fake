use super::super::{
    ctes::{InlineCte, collect_query_cte_scope, inline_query_with_cte_scope},
    normalize_identifier,
    outer_references::{OuterReferenceContext, substitute_outer_references},
    scope::BoundScope,
};
use crate::{QueryResult, catalog::Catalog, value::Value};
use sqlparser::{
    ast::{self, Spanned as _},
    tokenizer::Span,
};

#[derive(Clone, PartialEq, Eq)]
pub(in crate::executor) enum InitplanKey {
    Scalar(Vec<(Span, String)>),
    DerivedCte(Vec<(Span, String)>),
    Cte(Span, String),
}

impl InitplanKey {
    pub(in crate::executor) fn create_scalar(query: &ast::Query) -> Self {
        fn collect_projection(expression: &ast::SetExpr, keys: &mut Vec<(Span, String)>) {
            match expression {
                ast::SetExpr::Select(select) => {
                    for item in &select.projection {
                        let (span, mut expression) = match item {
                            ast::SelectItem::UnnamedExpr(expr)
                            | ast::SelectItem::ExprWithAlias { expr, .. } => {
                                (expr.span(), expr.clone())
                            }
                            _ => {
                                keys.push((item.span(), item.to_string()));
                                continue;
                            }
                        };
                        struct AliasRemover;
                        impl ast::VisitorMut for AliasRemover {
                            type Break = ();
                            fn pre_visit_query(
                                &mut self,
                                query: &mut ast::Query,
                            ) -> std::ops::ControlFlow<()> {
                                if let ast::SetExpr::Select(select) = query.body.as_mut() {
                                    for item in &mut select.projection {
                                        if let ast::SelectItem::ExprWithAlias { expr, .. } = item {
                                            *item = ast::SelectItem::UnnamedExpr(expr.clone());
                                        }
                                    }
                                }
                                std::ops::ControlFlow::Continue(())
                            }
                        }
                        let _ = ast::VisitMut::visit(&mut expression, &mut AliasRemover);
                        keys.push((span, expression.to_string()));
                    }
                }
                ast::SetExpr::Query(query) => collect_projection(&query.body, keys),
                ast::SetExpr::SetOperation { left, right, .. } => {
                    collect_projection(left, keys);
                    collect_projection(right, keys);
                }
                _ => keys.push((expression.span(), expression.to_string())),
            }
        }
        let mut keys = Vec::new();
        collect_projection(&query.body, &mut keys);
        Self::Scalar(keys)
    }
}

#[derive(Default)]
pub(crate) struct InitplanCache {
    entries: Vec<(InitplanKey, Option<QueryResult>)>,
    recursive_rows: Vec<(InitplanKey, RecursiveRows)>,
}

pub(in crate::executor) struct RecursiveRows {
    pub(in crate::executor) rows: Vec<Vec<Value>>,
    pub(in crate::executor) working: Vec<Vec<Value>>,
}

impl InitplanCache {
    pub(in crate::executor) fn take_recursive_rows(
        &mut self,
        key: &InitplanKey,
    ) -> Option<RecursiveRows> {
        let index = self
            .recursive_rows
            .iter()
            .position(|(cached, _)| cached == key)?;
        Some(self.recursive_rows.remove(index).1)
    }

    pub(in crate::executor) fn set_recursive_rows(
        &mut self,
        key: InitplanKey,
        rows: RecursiveRows,
    ) {
        if self.entries.iter().any(|(cached, _)| cached == &key) {
            self.recursive_rows.push((key, rows));
        }
    }
    pub(in crate::executor) fn get_result(&self, key: &InitplanKey) -> Option<QueryResult> {
        self.entries
            .iter()
            .find(|(cached, _)| cached == key)
            .and_then(|(_, result)| result.clone())
    }

    pub(in crate::executor) fn set_result(&mut self, key: &InitplanKey, result: QueryResult) {
        if let Some((_, cached)) = self.entries.iter_mut().find(|(cached, _)| cached == key) {
            *cached = Some(result);
        }
    }
}

struct CteScope {
    definitions: Vec<InlineCte>,
    ctes: Vec<ast::Cte>,
    inherited: usize,
    recursive: bool,
}

struct InitplanCollector<'a> {
    catalog: &'a Catalog,
    scopes: Vec<CteScope>,
    cache: InitplanCache,
}

impl InitplanCollector<'_> {
    fn register_query(&mut self, key: InitplanKey, query: &ast::Query, inherited: &[InlineCte]) {
        let empty = BoundScope {
            columns: Vec::new(),
        };
        let independent = inline_query_with_cte_scope(query, self.catalog, inherited)
            .and_then(|query| super::bind_lateral_query(self.catalog, &query, &empty, &[]))
            .and_then(|(mut query, _)| {
                substitute_outer_references(
                    self.catalog,
                    &mut query,
                    &empty,
                    &[],
                    Vec::new(),
                    OuterReferenceContext::Independent,
                )
            })
            .is_ok();
        if independent && !self.cache.entries.iter().any(|(cached, _)| cached == &key) {
            if matches!(key, InitplanKey::Cte(..)) {
                let InitplanKey::Scalar(projection) = InitplanKey::create_scalar(query) else {
                    unreachable!("scalar key")
                };
                self.cache
                    .entries
                    .push((InitplanKey::DerivedCte(projection), None));
            }
            self.cache.entries.push((key, None));
        }
    }
}

impl ast::Visitor for InitplanCollector<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<()> {
        let parent = self.scopes.last();
        let cte = parent.and_then(|scope| {
            scope
                .ctes
                .iter()
                .position(|cte| cte.query.as_ref() == query)
        });
        let inherited = parent
            .map(|scope| {
                let length = if scope.recursive {
                    scope.definitions.len()
                } else {
                    cte.map_or(scope.definitions.len(), |index| scope.inherited + index)
                };
                scope.definitions[..length].to_vec()
            })
            .unwrap_or_default();
        if let Some(index) = cte {
            let cte = &parent.expect("CTE has a parent scope").ctes[index];
            self.register_query(
                InitplanKey::Cte(cte.alias.name.span, normalize_identifier(&cte.alias.name)),
                query,
                &inherited,
            );
        }
        let locals = collect_query_cte_scope(query).unwrap_or_default();
        self.scopes.push(CteScope {
            inherited: inherited.len(),
            definitions: inherited.into_iter().chain(locals).collect(),
            ctes: query
                .with
                .as_ref()
                .map(|with| with.cte_tables.clone())
                .unwrap_or_default(),
            recursive: query.with.as_ref().is_some_and(|with| with.recursive),
        });
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &ast::Query) -> std::ops::ControlFlow<()> {
        self.scopes.pop().expect("query pushed a CTE scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &ast::Expr) -> std::ops::ControlFlow<()> {
        let query = match expr {
            ast::Expr::Subquery(query)
            | ast::Expr::Exists {
                subquery: query, ..
            }
            | ast::Expr::InSubquery {
                subquery: query, ..
            } => query,
            _ => return std::ops::ControlFlow::Continue(()),
        };
        let inherited = self
            .scopes
            .last()
            .map(|scope| scope.definitions.clone())
            .unwrap_or_default();
        self.register_query(InitplanKey::create_scalar(query), query, &inherited);
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn collect_lateral_initplans(
    catalog: &Catalog,
    statement: &ast::Statement,
) -> InitplanCache {
    let expanded = match statement {
        ast::Statement::Query(query) => super::super::views::expand_query_views(catalog, query)
            .ok()
            .flatten()
            .map(|query| ast::Statement::Query(Box::new(query))),
        _ => None,
    };
    let statement = expanded.as_ref().unwrap_or(statement);
    struct LateralDetector;
    impl ast::Visitor for LateralDetector {
        type Break = ();
        fn pre_visit_table_factor(
            &mut self,
            factor: &ast::TableFactor,
        ) -> std::ops::ControlFlow<()> {
            if matches!(factor, ast::TableFactor::Derived { lateral: true, .. }) {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        }
    }
    if ast::Visit::visit(statement, &mut LateralDetector).is_continue() {
        return InitplanCache::default();
    }
    let mut collector = InitplanCollector {
        catalog,
        scopes: Vec::new(),
        cache: InitplanCache::default(),
    };
    let _ = ast::Visit::visit(statement, &mut collector);
    collector.cache
}
