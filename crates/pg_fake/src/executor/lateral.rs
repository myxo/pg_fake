use super::{
    json,
    outer_references::{OuterReferenceContext, substitute_outer_references},
    scope::BoundScope,
};
use crate::{catalog::Catalog, error::Result, value::Value};
use sqlparser::ast;
use std::collections::BTreeSet;

mod initplans;
pub(crate) use initplans::{InitplanCache, collect_lateral_initplans};

pub(super) fn skips_lateral_rows(
    selection: Option<&ast::Expr>,
    context: &super::StatementContext,
) -> bool {
    context.lateral_invocation
        && selection.is_some_and(|selection| {
            !super::query::contains_volatile_expression(selection)
                && matches!(
                    super::expressions::evaluate(
                        selection,
                        super::scope::RowScope::Bound(&BoundScope {
                            columns: Vec::new()
                        }),
                        &[],
                        context
                    ),
                    Ok(Value::Bool(false) | Value::Null)
                )
        })
}
pub(super) use initplans::{InitplanKey, RecursiveRows};

pub(super) fn contains_lateral_source(factor: &ast::TableFactor) -> bool {
    match factor {
        ast::TableFactor::Derived { lateral: true, .. } => true,
        ast::TableFactor::UNNEST { .. } => true,
        ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            contains_lateral_source(&table_with_joins.relation)
                || table_with_joins
                    .joins
                    .iter()
                    .any(|join| contains_lateral_source(&join.relation))
        }
        _ => json::contains_json_expansion(factor),
    }
}

pub(crate) fn bind_lateral_query(
    catalog: &Catalog,
    query: &ast::Query,
    outer: &BoundScope,
    row: &[Value],
) -> Result<(ast::Query, BTreeSet<usize>)> {
    struct ProjectionNamer;
    impl ast::VisitorMut for ProjectionNamer {
        type Break = ();

        fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<()> {
            fn wrap_set_branches(expression: &mut ast::SetExpr) {
                if let ast::SetExpr::SetOperation { left, right, .. } = expression {
                    for branch in [left, right] {
                        if matches!(branch.as_ref(), ast::SetExpr::Select(_)) {
                            **branch = ast::SetExpr::Query(Box::new(ast::Query {
                                with: None,
                                body: branch.clone(),
                                order_by: None,
                                limit_clause: None,
                                fetch: None,
                                locks: Vec::new(),
                                for_clause: None,
                                settings: None,
                                format_clause: None,
                                pipe_operators: Vec::new(),
                            }));
                        } else {
                            wrap_set_branches(branch);
                        }
                    }
                }
            }
            wrap_set_branches(&mut query.body);
            if let ast::SetExpr::Select(select) = query.body.as_mut() {
                for item in &mut select.projection {
                    let ast::SelectItem::UnnamedExpr(expr) = item else {
                        continue;
                    };
                    let alias = match expr {
                        ast::Expr::Identifier(name) => name.clone(),
                        ast::Expr::CompoundIdentifier(names) => {
                            names.last().expect("qualified column").clone()
                        }
                        _ => continue,
                    };
                    *item = ast::SelectItem::ExprWithAlias {
                        expr: expr.clone(),
                        alias,
                    };
                }
            }
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut query = query.clone();
    let _ = ast::VisitMut::visit(&mut query, &mut ProjectionNamer);
    let slots = substitute_outer_references(
        catalog,
        &mut query,
        outer,
        row,
        Vec::new(),
        OuterReferenceContext::Lateral,
    )?;
    Ok((query, slots))
}
