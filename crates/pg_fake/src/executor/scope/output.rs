use super::{
    BoundColumn, BoundScope, RowScope,
    sources::{bind_query_scope, bind_query_scope_with_outer},
    subqueries::infer_expression_data_type,
};
use crate::executor::{
    expressions::{
        self, create_constant_expression_schema, extract_number_literal,
        extract_unknown_string_literal, is_null_literal,
    },
    normalize_identifier, normalize_unqualified_object_name,
};
use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn identify_unknown_query_columns(query: &ast::Query, columns: usize) -> Vec<bool> {
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return vec![false; columns];
    };
    let unknown = select
        .projection
        .iter()
        .map(|item| match item {
            ast::SelectItem::UnnamedExpr(expression)
            | ast::SelectItem::ExprWithAlias {
                expr: expression, ..
            } => {
                extract_unknown_string_literal(expression).is_some() || is_null_literal(expression)
            }
            _ => false,
        })
        .collect::<Vec<_>>();
    if unknown.len() == columns {
        unknown
    } else {
        vec![false; columns]
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn identify_unknown_set_operand_columns(
    expression: &ast::SetExpr,
    columns: usize,
) -> Vec<bool> {
    let query = match expression {
        ast::SetExpr::Select(select) => ast::Query {
            with: None,
            body: Box::new(ast::SetExpr::Select(select.clone())),
            order_by: None,
            limit_clause: None,
            fetch: None,
            locks: Vec::new(),
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: Vec::new(),
        },
        ast::SetExpr::Query(query) => (**query).clone(),
        _ => return vec![false; columns],
    };
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return vec![false; columns];
    };
    let mut unknown = identify_unknown_query_columns(&query, columns);
    if matches!(select.distinct, Some(ast::Distinct::Distinct)) {
        unknown.fill(false);
        return unknown;
    }
    let mut mark_resolved = |expression: &ast::Expr| {
        let index = if let Some(position) = extract_number_literal(expression)
            && !position.contains(['.', 'e', 'E'])
        {
            position
                .parse::<usize>()
                .ok()
                .and_then(|position| position.checked_sub(1))
        } else if let ast::Expr::Identifier(identifier) = expression {
            select.projection.iter().position(|item| {
                matches!(item, ast::SelectItem::ExprWithAlias { alias, .. }
                    if normalize_identifier(alias) == normalize_identifier(identifier))
            })
        } else {
            None
        };
        if let Some(index) = index
            && let Some(unknown) = unknown.get_mut(index)
        {
            *unknown = false;
        }
    };
    if let Some(ast::Distinct::On(expressions)) = &select.distinct {
        for expression in expressions {
            mark_resolved(expression);
        }
    }
    if let ast::GroupByExpr::Expressions(expressions, _) = &select.group_by {
        for expression in expressions {
            mark_resolved(expression);
        }
    }
    if let Some(order_by) = &query.order_by
        && let ast::OrderByKind::Expressions(orders) = &order_by.kind
    {
        for order in orders {
            mark_resolved(&order.expr);
        }
    }
    unknown
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn infer_query_output_columns(
    catalog: &Catalog,
    query: &ast::Query,
) -> Result<Vec<(String, PgType)>> {
    describe_bound_query_columns(catalog, query, None).map(|columns| {
        columns
            .into_iter()
            .map(|column| (column.name, column.data_type))
            .collect()
    })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn describe_bound_query_columns(
    catalog: &Catalog,
    query: &ast::Query,
    outer: Option<&BoundScope>,
) -> Result<Vec<BoundColumn>> {
    if query.with.is_some() {
        let mut query = query.clone();
        if let Some(outer) = outer {
            crate::executor::outer_references::substitute_outer_references(
                catalog,
                &mut query,
                outer,
                &vec![crate::value::Value::Null; outer.columns.len()],
                Vec::new(),
                crate::executor::outer_references::OuterReferenceContext::Subquery,
            )?;
        }
        let query = crate::executor::ctes::inline_query_ctes(&query, catalog, None, true)?;
        return describe_bound_query_columns(catalog, &query, None);
    }
    match query.body.as_ref() {
        ast::SetExpr::Select(select) => {
            let scope = match outer {
                Some(outer) => bind_query_scope_with_outer(catalog, select, outer)?,
                None => bind_query_scope(catalog, select)?,
            };
            select
                .projection
                .iter()
                .flat_map(|item| match item {
                    ast::SelectItem::Wildcard(_) => scope
                        .select_wildcard_columns(None)
                        .into_iter()
                        .map(|column| Ok((column.name.clone(), column.data_type)))
                        .collect::<Vec<_>>(),
                    ast::SelectItem::QualifiedWildcard(
                        ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                        _,
                    ) => {
                        let qualifier = match normalize_unqualified_object_name(name) {
                            Ok(qualifier) => qualifier,
                            Err(error) => return vec![Err(error)],
                        };
                        let columns = scope
                            .select_wildcard_columns(Some(&qualifier))
                            .into_iter()
                            .map(|column| Ok((column.name.clone(), column.data_type)))
                            .collect::<Vec<_>>();
                        if columns.is_empty()
                            && !scope
                                .columns
                                .iter()
                                .any(|column| column.qualifier == qualifier)
                        {
                            vec![Err(PgError::create(
                                SqlState::UndefinedTable,
                                format!("missing FROM-clause entry for table {qualifier:?}"),
                            ))]
                        } else {
                            columns
                        }
                    }
                    ast::SelectItem::UnnamedExpr(ast::Expr::Identifier(identifier)) => {
                        vec![
                            scope.resolve_column(std::slice::from_ref(identifier)).map(
                                |(_, data_type)| (normalize_identifier(identifier), data_type),
                            ),
                        ]
                    }
                    ast::SelectItem::UnnamedExpr(ast::Expr::CompoundIdentifier(identifiers)) => {
                        vec![scope.resolve_column(identifiers).map(|(_, data_type)| {
                            (
                                normalize_identifier(
                                    identifiers
                                        .last()
                                        .expect("compound identifier is non-empty"),
                                ),
                                data_type,
                            )
                        })]
                    }
                    ast::SelectItem::ExprWithAlias { expr, alias } => vec![
                        infer_expression_data_type(catalog, expr, &scope)
                            .map(|data_type| (normalize_identifier(alias), data_type)),
                    ],
                    ast::SelectItem::UnnamedExpr(expr) => {
                        vec![infer_expression_data_type(catalog, expr, &scope).and_then(
                            |data_type| {
                                let name = match expr {
                                    ast::Expr::Function(function) => {
                                        super::super::normalize_function_name(&function.name)?
                                    }
                                    ast::Expr::Extract { .. } => "extract".into(),
                                    _ => "?column?".into(),
                                };
                                Ok((name, data_type))
                            },
                        )]
                    }
                    _ => vec![reject_unsupported("SELECT projection is not implemented")],
                })
                .collect::<Result<Vec<_>>>()
                .map(|columns| {
                    columns
                        .into_iter()
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
                            wildcard: true,
                            depth: 0,
                            table_id: None,
                        })
                        .collect()
                })
        }
        ast::SetExpr::Values(values) => {
            let width = values.rows.first().map(|row| row.len()).unwrap_or(0);
            if values.rows.iter().any(|row| row.len() != width) {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "VALUES lists must all be the same length",
                ));
            }
            (0..width)
                .map(|slot| {
                    let data_type = values
                        .rows
                        .iter()
                        .map(|row| &row[slot])
                        .filter(|expr| {
                            !is_null_literal(expr) && extract_unknown_string_literal(expr).is_none()
                        })
                        .try_fold(None::<PgType>, |common, expr| {
                            let data_type = expressions::infer_expression_data_type(
                                expr,
                                RowScope::Table(&create_constant_expression_schema()),
                            )?;
                            Ok(Some(match common {
                                Some(common) => {
                                    let base = crate::coercion::resolve_common_type(
                                        common.base,
                                        data_type.base,
                                    )
                                    .ok_or_else(|| {
                                        PgError::create(
                                            SqlState::DatatypeMismatch,
                                            "VALUES types cannot be matched",
                                        )
                                    })?;
                                    PgType::create_with_typmod(
                                        base,
                                        if base == common.base
                                            && base == data_type.base
                                            && common.typmod == data_type.typmod
                                        {
                                            common.typmod
                                        } else {
                                            PgType::NO_TYPEMOD
                                        },
                                    )
                                }
                                None => data_type,
                            }))
                        })?
                        .unwrap_or(PgType::create(BaseType::Text));
                    Ok(BoundColumn {
                        name: format!("column{}", slot + 1),
                        data_type,
                        qualifier: String::new(),
                        slot,
                        output_order: slot,
                        qualified_order: slot,
                        qualified_merged: None,
                        merged: None,
                        unqualified: true,
                        wildcard: true,
                        depth: 0,
                        table_id: None,
                        source_name: format!("column{}", slot + 1),
                    })
                })
                .collect()
        }
        ast::SetExpr::Query(query) => describe_bound_query_columns(catalog, query, outer),
        ast::SetExpr::SetOperation {
            left: left_expression,
            right: right_expression,
            ..
        } => {
            let left = describe_bound_set_expression_columns(catalog, left_expression, outer)?;
            let right = describe_bound_set_expression_columns(catalog, right_expression, outer)?;
            if left.len() != right.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "each set-operation query must have the same number of columns",
                ));
            }
            let left_unknown = identify_unknown_set_operand_columns(left_expression, left.len());
            let right_unknown = identify_unknown_set_operand_columns(right_expression, right.len());
            left.into_iter()
                .zip(right)
                .zip(left_unknown.into_iter().zip(right_unknown))
                .map(|((mut left, right), (left_unknown, right_unknown))| {
                    let base = match (left_unknown, right_unknown) {
                        (true, false) => right.data_type.base,
                        (false, true) => left.data_type.base,
                        _ => crate::coercion::resolve_common_type(
                            left.data_type.base,
                            right.data_type.base,
                        )
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::DatatypeMismatch,
                                "set-operation types cannot be matched",
                            )
                        })?,
                    };
                    left.data_type = PgType::create(base);
                    Ok(left)
                })
                .collect()
        }
        _ => reject_unsupported("query source is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn describe_bound_set_expression_columns(
    catalog: &Catalog,
    expression: &ast::SetExpr,
    outer: Option<&BoundScope>,
) -> Result<Vec<BoundColumn>> {
    match expression {
        ast::SetExpr::Query(query) => describe_bound_query_columns(catalog, query, outer),
        ast::SetExpr::Select(_) | ast::SetExpr::Values(_) | ast::SetExpr::SetOperation { .. } => {
            let query = ast::Query {
                with: None,
                body: Box::new(expression.clone()),
                order_by: None,
                limit_clause: None,
                fetch: None,
                locks: Vec::new(),
                for_clause: None,
                settings: None,
                format_clause: None,
                pipe_operators: Vec::new(),
            };
            describe_bound_query_columns(catalog, &query, outer)
        }
        _ => reject_unsupported("set-operation input is not implemented"),
    }
}
