use sqlparser::ast::{self, Spanned as _};
use std::collections::BTreeSet;

use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState},
    executor::{
        normalize_identifier,
        scope::{BoundScope, RowScope, bind_query_scope},
    },
    value::Value,
};

pub(super) enum OuterReferenceContext {
    Subquery,
    Lateral,
    Procedural,
    Independent,
}

struct OuterReferenceSubstituter<'a> {
    referenced_slots: BTreeSet<usize>,
    catalog: &'a Catalog,
    outer_scope: &'a BoundScope,
    outer_row: &'a [Value],
    scopes: Vec<BoundScope>,
    output_aliases: Vec<BTreeSet<String>>,
    protected_order_identifiers: Vec<bool>,
    group_by_depth: usize,
    group_expression_depth: usize,
    error: Option<PgError>,
    reference_context: OuterReferenceContext,
    cte_scopes: Vec<CteScope>,
}

struct CteScope {
    definitions: Vec<super::ctes::InlineCte>,
    queries: Vec<sqlparser::tokenizer::Span>,
    inherited: usize,
    recursive: bool,
}

impl ast::VisitorMut for OuterReferenceSubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let inherited = self
            .cte_scopes
            .last()
            .map(|scope| {
                let length = if !scope.recursive {
                    scope
                        .queries
                        .iter()
                        .position(|span| *span == query.span())
                        .map(|index| scope.inherited + index)
                        .unwrap_or(scope.definitions.len())
                } else {
                    scope.definitions.len()
                };
                scope.definitions[..length].to_vec()
            })
            .unwrap_or_default();
        let mut cte_analysis = query.clone();
        let _ = ast::visit_expressions_mut(&mut cte_analysis, |expression| {
            let identifiers = match expression {
                ast::Expr::Identifier(identifier) => std::slice::from_ref(identifier),
                ast::Expr::CompoundIdentifier(identifiers) => identifiers.as_slice(),
                _ => return std::ops::ControlFlow::<()>::Continue(()),
            };
            if let Ok((_, data_type)) = self.outer_scope.resolve_column(identifiers)
                && let Ok(value) = RowScope::Bound(self.outer_scope)
                    .resolve_column_value(identifiers, self.outer_row)
            {
                *expression = crate::analyzer::create_typed_literal(value, data_type);
            }
            std::ops::ControlFlow::Continue(())
        });
        let locals = match super::ctes::collect_query_cte_scope(&cte_analysis) {
            Ok(locals) => locals,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        self.cte_scopes.push(CteScope {
            inherited: inherited.len(),
            definitions: inherited.iter().cloned().chain(locals).collect(),
            queries: query
                .with
                .as_ref()
                .map(|with| with.cte_tables.iter().map(|cte| cte.query.span()).collect())
                .unwrap_or_default(),
            recursive: query.with.as_ref().is_some_and(|with| with.recursive),
        });
        let ast::SetExpr::Select(select) = query.body.as_ref() else {
            self.scopes.push(BoundScope {
                columns: Vec::new(),
            });
            self.output_aliases.push(BTreeSet::new());
            return std::ops::ControlFlow::Continue(());
        };
        let output_aliases = select
            .projection
            .iter()
            .filter_map(|item| match item {
                ast::SelectItem::ExprWithAlias { alias, .. } => Some(normalize_identifier(alias)),
                _ => None,
            })
            .collect();
        let analysis_query = cte_analysis;
        let bound = if query.with.is_some() || !inherited.is_empty() {
            super::ctes::inline_query_with_cte_scope(&analysis_query, self.catalog, &inherited)
                .and_then(|query| {
                    let ast::SetExpr::Select(select) = query.body.as_ref() else {
                        unreachable!("SELECT remains SELECT")
                    };
                    bind_query_scope(self.catalog, select)
                })
        } else {
            let ast::SetExpr::Select(select) = analysis_query.body.as_ref() else {
                unreachable!("SELECT")
            };
            bind_query_scope(self.catalog, select)
        };
        match bound {
            Ok(scope) => self.scopes.push(scope),
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        self.output_aliases.push(output_aliases);
        let ast::SetExpr::Select(select) = query.body.as_mut() else {
            unreachable!("SELECT")
        };
        let mut projection = Vec::new();
        for item in std::mem::take(&mut select.projection) {
            if let ast::SelectItem::QualifiedWildcard(
                ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                _,
            ) = &item
                && let Ok(qualifier) = super::normalize_unqualified_object_name(name)
                && !self.scopes.iter().any(|scope| {
                    scope
                        .columns
                        .iter()
                        .any(|column| column.qualifier == qualifier)
                })
            {
                let columns = self.outer_scope.select_wildcard_columns(Some(&qualifier));
                if !columns.is_empty() {
                    projection.extend(columns.into_iter().map(|column| {
                        ast::SelectItem::ExprWithAlias {
                            expr: ast::Expr::CompoundIdentifier(vec![
                                ast::Ident::with_quote('"', &qualifier),
                                ast::Ident::with_quote('"', &column.name),
                            ]),
                            alias: ast::Ident::with_quote('"', &column.name),
                        }
                    }));
                    continue;
                }
            }
            projection.push(item);
        }
        select.projection = projection;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte_scopes.pop().expect("query pushed CTE scope");
        self.scopes.pop().expect("visited query pushed a scope");
        self.output_aliases
            .pop()
            .expect("visited query pushed output aliases");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_order_by_expr(
        &mut self,
        order_by: &mut ast::OrderByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.protected_order_identifiers.push(
            matches!(&order_by.expr, ast::Expr::Identifier(identifier)
                if self.output_aliases.last().is_some_and(|aliases| aliases.contains(&normalize_identifier(identifier)))),
        );
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_order_by_expr(
        &mut self,
        _order_by: &mut ast::OrderByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.protected_order_identifiers
            .pop()
            .expect("visited ORDER BY expression pushed alias protection");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_group_by(
        &mut self,
        _group_by: &mut ast::GroupByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.group_by_depth += 1;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_group_by(
        &mut self,
        _group_by: &mut ast::GroupByExpr,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.group_by_depth -= 1;
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        if matches!(self.reference_context, OuterReferenceContext::Lateral)
            && let ast::Expr::Function(function) = expression
            && super::aggregates::is_aggregate_function(function)
        {
            let mut has_outer = false;
            let mut has_inner = false;
            let _ = ast::visit_expressions(function, |expr| {
                let identifiers = match expr {
                    ast::Expr::Identifier(identifier) => std::slice::from_ref(identifier),
                    ast::Expr::CompoundIdentifier(identifiers) => identifiers.as_slice(),
                    _ => return std::ops::ControlFlow::<()>::Continue(()),
                };
                if self
                    .scopes
                    .iter()
                    .rev()
                    .any(|scope| scope.resolve_column(identifiers).is_ok())
                {
                    has_inner = true;
                } else if self.outer_scope.resolve_column(identifiers).is_ok() {
                    has_outer = true;
                }
                std::ops::ControlFlow::Continue(())
            });
            if has_outer && !has_inner {
                self.error = Some(PgError::create(
                    SqlState::GroupingError,
                    "aggregate functions are not allowed in FROM clause of their own query level",
                ));
                return std::ops::ControlFlow::Break(());
            }
        }
        let protected_group_identifier =
            self.group_by_depth != 0 && self.group_expression_depth == 0;
        if self.group_by_depth != 0 {
            self.group_expression_depth += 1;
        }
        let identifiers = match expression {
            ast::Expr::Identifier(identifier) => std::slice::from_ref(identifier),
            ast::Expr::CompoundIdentifier(identifiers) => identifiers.as_slice(),
            _ => return std::ops::ControlFlow::Continue(()),
        };
        if (self.protected_order_identifiers.last() == Some(&true) || protected_group_identifier)
            && identifiers.len() == 1
            && self
                .output_aliases
                .last()
                .is_some_and(|aliases| aliases.contains(&normalize_identifier(&identifiers[0])))
        {
            return std::ops::ControlFlow::Continue(());
        }
        for scope in self.scopes.iter().rev() {
            if identifiers.len() == 2 {
                let qualifier = normalize_identifier(&identifiers[0]);
                if !scope
                    .columns
                    .iter()
                    .any(|column| column.qualifier == qualifier)
                {
                    continue;
                }
            }
            match scope.resolve_column(identifiers) {
                Ok(_) => {
                    if matches!(self.reference_context, OuterReferenceContext::Procedural)
                        && identifiers.len() == 1
                        && self.outer_scope.resolve_column(identifiers).is_ok()
                    {
                        self.error = Some(PgError::create(
                            SqlState::AmbiguousColumn,
                            "column reference is ambiguous",
                        ));
                        return std::ops::ControlFlow::Break(());
                    }
                    return std::ops::ControlFlow::Continue(());
                }
                Err(error)
                    if identifiers.len() == 1 && error.sqlstate == SqlState::UndefinedColumn => {}
                Err(error) => {
                    self.error = Some(error);
                    return std::ops::ControlFlow::Break(());
                }
            }
        }
        match self.outer_scope.resolve_column(identifiers) {
            Ok((slot, data_type)) => {
                self.referenced_slots.insert(slot);
                match RowScope::Bound(self.outer_scope)
                    .resolve_column_value(identifiers, self.outer_row)
                {
                    Ok(value) => {
                        *expression = crate::analyzer::create_typed_literal(value, data_type);
                    }
                    Err(error) => {
                        self.error = Some(error);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            Err(error)
                if identifiers.len() == 2
                    && self.outer_scope.columns.iter().any(|column| {
                        column.qualifier == normalize_identifier(&identifiers[0])
                    }) =>
            {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
            Err(error)
                if matches!(
                    error.sqlstate,
                    SqlState::UndefinedColumn | SqlState::UndefinedTable
                ) && !matches!(self.reference_context, OuterReferenceContext::Independent) => {}
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_expr(
        &mut self,
        _expression: &mut ast::Expr,
    ) -> std::ops::ControlFlow<Self::Break> {
        if self.group_by_depth != 0 {
            self.group_expression_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn collect_outer_reference_slots(
    catalog: &Catalog,
    expression: &ast::Expr,
    outer_scope: &BoundScope,
) -> Result<BTreeSet<usize>> {
    let mut expression = expression.clone();
    let outer_row = vec![Value::Null; outer_scope.columns.len()];
    substitute_outer_references(
        catalog,
        &mut expression,
        outer_scope,
        &outer_row,
        Vec::new(),
        OuterReferenceContext::Subquery,
    )
}

pub(super) fn substitute_outer_references<V: ast::VisitMut>(
    catalog: &Catalog,
    value: &mut V,
    outer_scope: &BoundScope,
    outer_row: &[Value],
    scopes: Vec<BoundScope>,
    reference_context: OuterReferenceContext,
) -> Result<BTreeSet<usize>> {
    let mut substituter = OuterReferenceSubstituter {
        referenced_slots: BTreeSet::new(),
        catalog,
        outer_scope,
        outer_row,
        scopes,
        output_aliases: Vec::new(),
        protected_order_identifiers: Vec::new(),
        group_by_depth: 0,
        group_expression_depth: 0,
        error: None,
        reference_context,
        cte_scopes: Vec::new(),
    };
    let _ = value.visit(&mut substituter);
    substituter
        .error
        .map_or(Ok(substituter.referenced_slots), Err)
}
