use sqlparser::ast;

use crate::{
    error::Result,
    executor::{
        DatabaseState,
        query::{
            ProjectionSource, build_projection_plan, contains_query_aggregate,
            contains_volatile_expression,
        },
        scope::{BoundScope, bind_select_scope, try_resolve_column_reference},
    },
};

pub(super) fn push_derived_filters(
    state: &DatabaseState,
    query: &ast::Query,
    outer: &BoundScope,
    start: usize,
    selection: Option<&ast::Expr>,
    inherited: bool,
) -> Result<Option<ast::Query>> {
    let Some(selection) = selection else {
        return Ok(None);
    };
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };
    if (!inherited && query.locks.is_empty())
        || query.with.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || select.distinct.is_some()
        || select.having.is_some()
        || contains_query_aggregate(query)
        || !matches!(&select.group_by, ast::GroupByExpr::Expressions(expressions, modifiers) if expressions.is_empty() && modifiers.is_empty())
    {
        return Ok(None);
    }
    let scope = bind_select_scope(state, select)?;
    let (projections, _) = build_projection_plan(state, &select.projection, &scope)?;
    let mut filters = Vec::new();
    super::scans::collect_pushdown_filters(
        selection,
        outer,
        start,
        start + projections.len(),
        &mut filters,
    );
    if filters.is_empty() {
        return Ok(None);
    }
    let mut pushed = Vec::new();
    for filter in filters {
        let mut filter = filter.clone();
        let mut valid = true;
        let _ = ast::visit_expressions_mut(&mut filter, |expression| {
            if let Some((slot, _)) = try_resolve_column_reference(expression, outer) {
                let replacement = match &projections[slot - start] {
                    ProjectionSource::Column(slot) => {
                        let column = &scope.columns[*slot];
                        ast::Expr::CompoundIdentifier(vec![
                            ast::Ident::with_quote('"', &column.qualifier),
                            ast::Ident::with_quote('"', &column.name),
                        ])
                    }
                    ProjectionSource::Expression(expression)
                        if !contains_volatile_expression(expression) =>
                    {
                        (*expression).clone()
                    }
                    _ => {
                        valid = false;
                        return std::ops::ControlFlow::Break(());
                    }
                };
                *expression = replacement;
            }
            std::ops::ControlFlow::Continue(())
        });
        if valid {
            pushed.push(filter);
        }
    }
    if pushed.is_empty() {
        return Ok(None);
    }
    let mut query = query.clone();
    let ast::SetExpr::Select(select) = query.body.as_mut() else {
        unreachable!("derived SELECT")
    };
    for filter in pushed {
        select.selection = Some(match select.selection.take() {
            Some(selection) => ast::Expr::BinaryOp {
                left: Box::new(selection),
                op: ast::BinaryOperator::And,
                right: Box::new(filter),
            },
            None => filter,
        });
    }
    Ok(Some(query))
}
