use std::cmp::Ordering;

use sqlparser::ast::{self, Spanned as _};

use crate::{
    ColumnMeta, QueryResult,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        StatementContext,
        expressions::{
            compare_values, create_constant_expression_schema, evaluate_and_coerce,
            extract_number_literal, extract_unknown_string_literal, infer_expression_data_type,
            is_null_literal, validate_ordering_type,
        },
        resolve_order_ascending,
        scope::{BoundColumn, BoundScope, RowScope},
    },
    value::{BaseType, PgType, Value},
};

use super::limits::resolve_select_limit;

#[derive(Clone)]
pub(crate) struct PreparedValues {
    occurrence: sqlparser::tokenizer::Span,
    sql: String,
    rows: Vec<Vec<Value>>,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn bind_values_scope(values: &ast::Values) -> Result<BoundScope> {
    let width = values.rows.first().map(|row| row.len()).unwrap_or(0);
    if values.rows.iter().any(|row| row.len() != width) {
        return Err(PgError::create(
            SqlState::SyntaxError,
            "VALUES lists must all be the same length",
        ));
    }
    let constants = create_constant_expression_schema();
    let columns = (0..width)
        .map(|slot| {
            let data_type = values
                .rows
                .iter()
                .map(|row| &row[slot])
                .filter(|expression| {
                    !is_null_literal(expression)
                        && extract_unknown_string_literal(expression).is_none()
                })
                .try_fold(None::<PgType>, |common, expression| {
                    let data_type =
                        infer_expression_data_type(expression, RowScope::Table(&constants))?;
                    Ok(Some(match common {
                        Some(common) => {
                            let base = coercion::resolve_common_type(common.base, data_type.base)
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
        .collect::<Result<Vec<_>>>()?;
    Ok(BoundScope { columns })
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_values_query(
    query: &ast::Query,
    values: &ast::Values,
    context: &StatementContext,
    maximum_rows: Option<usize>,
) -> Result<super::QueryOutput> {
    let scope = bind_values_scope(values)?;
    let columns = scope
        .columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name.clone(),
            type_oid: column.data_type.map_to_oid(),
            typmod: column.data_type.typmod,
        })
        .collect::<Vec<_>>();
    let orders = if let Some(order_by) = &query.order_by {
        let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
            return reject_unsupported("ORDER BY ALL is not implemented");
        };
        Some(
            orders
                .iter()
                .map(|order| {
                    let index = if let Some(position) = extract_number_literal(&order.expr)
                        && !position.contains(['.', 'e', 'E'])
                    {
                        position
                            .parse::<usize>()
                            .ok()
                            .and_then(|position| position.checked_sub(1))
                    } else if let ast::Expr::Identifier(identifier) = &order.expr {
                        scope
                            .resolve_column(std::slice::from_ref(identifier))
                            .ok()
                            .map(|(slot, _)| slot)
                    } else {
                        None
                    }
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::InvalidColumnReference,
                            "ORDER BY position is not in select list",
                        )
                    })?;
                    if index >= columns.len() {
                        return Err(PgError::create(
                            SqlState::InvalidColumnReference,
                            "ORDER BY position is not in select list",
                        ));
                    }
                    validate_ordering_type(scope.columns[index].data_type.base)?;
                    let ascending = resolve_order_ascending(&order.options)?;
                    Ok((
                        index,
                        ascending,
                        order.options.nulls_first.unwrap_or(!ascending),
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        )
    } else {
        None
    };
    let (limit, offset) = resolve_select_limit(query, context)?;
    let complete = maximum_rows.is_none_or(|maximum| {
        maximum
            >= values
                .rows
                .len()
                .saturating_sub(offset)
                .min(limit.unwrap_or(usize::MAX))
    });
    let limit = match (limit, maximum_rows) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(limit), None) | (None, Some(limit)) => Some(limit),
        (None, None) => None,
    };
    let demand = if limit == Some(0) {
        0
    } else if orders.is_none() {
        limit
            .map(|limit| limit.saturating_add(offset))
            .unwrap_or(values.rows.len())
    } else {
        values.rows.len()
    };
    let cache_key = context.capture_lock_queries.then(|| {
        (
            query.span(),
            format!("{:?} {query}", context.query_invocation),
        )
    });
    let mut rows = if let Some((occurrence, sql)) = &cache_key {
        let mut cached = context
            .prepared_values
            .lock()
            .expect("prepared VALUES mutex is poisoned");
        cached
            .iter()
            .position(|cached| cached.occurrence == *occurrence && cached.sql == *sql)
            .map(|index| cached.remove(index).rows)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let constants = create_constant_expression_schema();
    let evaluated = (|| {
        for (index, expressions) in values.rows.iter().take(demand).enumerate() {
            if rows.len() == index {
                rows.push(Vec::new());
            }
            for slot in rows[index].len()..expressions.len() {
                rows[index].push(evaluate_and_coerce(
                    &expressions[slot],
                    scope.columns[slot].data_type.base,
                    CastContext::Implicit,
                    RowScope::Table(&constants),
                    &[],
                    context,
                )?);
            }
        }
        Ok(())
    })();
    if let Some((occurrence, sql)) = cache_key {
        context
            .prepared_values
            .lock()
            .expect("prepared VALUES mutex is poisoned")
            .push(PreparedValues {
                occurrence,
                sql,
                rows: rows.clone(),
            });
    }
    evaluated?;
    if let Some(orders) = &orders {
        rows.sort_by(|left, right| {
            orders
                .iter()
                .find_map(|(index, ascending, nulls_first)| {
                    let ordering = match (&left[*index], &right[*index]) {
                        (Value::Null, Value::Null) => Ordering::Equal,
                        (Value::Null, _) => {
                            if *nulls_first {
                                Ordering::Less
                            } else {
                                Ordering::Greater
                            }
                        }
                        (_, Value::Null) => {
                            if *nulls_first {
                                Ordering::Greater
                            } else {
                                Ordering::Less
                            }
                        }
                        (left, right) => {
                            let ordering = compare_values(left, right)
                                .expect("VALUES columns have one common type");
                            if *ascending {
                                ordering
                            } else {
                                ordering.reverse()
                            }
                        }
                    };
                    (ordering != Ordering::Equal).then_some(ordering)
                })
                .unwrap_or(Ordering::Equal)
        });
    }
    let mut output = super::QueryOutput::create(QueryResult {
        columns,
        rows: rows
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect(),
    });
    output.complete = complete;
    Ok(output)
}
