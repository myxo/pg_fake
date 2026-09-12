use sqlparser::ast;
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

pub(super) enum NameConflictPolicy {
    PreferInner,
    RejectAmbiguous,
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
    name_conflict_policy: NameConflictPolicy,
}

impl ast::VisitorMut for OuterReferenceSubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
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
        let mut analysis = select.as_ref().clone();
        let _ = ast::visit_expressions_mut(&mut analysis, |expression| {
            let ast::Expr::Identifier(identifier) = expression else {
                return std::ops::ControlFlow::<()>::Continue(());
            };
            let identifiers = std::slice::from_ref(identifier);
            let Ok((_, data_type)) = self.outer_scope.resolve_column(identifiers) else {
                return std::ops::ControlFlow::Continue(());
            };
            let Ok(value) =
                RowScope::Bound(self.outer_scope).resolve_column_value(identifiers, self.outer_row)
            else {
                return std::ops::ControlFlow::Continue(());
            };
            *expression = crate::analyzer::create_typed_literal(value, data_type);
            std::ops::ControlFlow::Continue(())
        });
        match bind_query_scope(self.catalog, &analysis) {
            Ok(scope) => self.scopes.push(scope),
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        self.output_aliases.push(output_aliases);
        std::ops::ControlFlow::Continue(())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
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
                    if matches!(
                        self.name_conflict_policy,
                        NameConflictPolicy::RejectAmbiguous
                    ) && identifiers.len() == 1
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
                if matches!(
                    error.sqlstate,
                    SqlState::UndefinedColumn | SqlState::UndefinedTable
                ) => {}
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
        NameConflictPolicy::PreferInner,
    )
}

pub(super) fn substitute_outer_references<V: ast::VisitMut>(
    catalog: &Catalog,
    value: &mut V,
    outer_scope: &BoundScope,
    outer_row: &[Value],
    scopes: Vec<BoundScope>,
    name_conflict_policy: NameConflictPolicy,
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
        name_conflict_policy,
    };
    let _ = value.visit(&mut substituter);
    substituter
        .error
        .map_or(Ok(substituter.referenced_slots), Err)
}
