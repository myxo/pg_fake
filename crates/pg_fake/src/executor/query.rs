use super::*;

pub(crate) fn query_columns(
    state: &DatabaseState,
    statement: &Statement,
) -> Result<Vec<ColumnMeta>> {
    let Statement::Query(query) = statement else {
        return Ok(Vec::new());
    };
    match query.body.as_ref() {
        SetExpr::Select(select) => bind_select_scope(state, select).and_then(|scope| {
            projections_and_columns(&select.projection, &scope).map(|(_, columns)| columns)
        }),
        SetExpr::Values(values) => bind_values_scope(values).map(|scope| {
            scope
                .columns
                .iter()
                .map(|column| ColumnMeta {
                    name: column.name.clone(),
                    type_oid: column.data_type.oid(),
                    typmod: column.data_type.typmod,
                })
                .collect()
        }),
        _ => Ok(Vec::new()),
    }
}
enum Projection<'a> {
    Column(usize),
    Expression(&'a Expr),
}
enum OrderKey<'a> {
    Output(usize),
    Expression(&'a Expr),
}
enum RowCountClause {
    Limit,
    Offset,
}
struct OrderSpec<'a> {
    key: OrderKey<'a>,
    ascending: bool,
    nulls_first: bool,
}
struct OrderedRow {
    values: Vec<Value>,
    keys: Vec<Value>,
}
pub(super) fn select_lock_mode(query: &sqlparser::ast::Query) -> Result<Option<RowLockMode>> {
    if query.locks.len() > 1 {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "multiple row-lock clauses are not implemented",
        ));
    }
    let Some(lock) = query.locks.first() else {
        return Ok(None);
    };
    if lock.of.is_some() || lock.nonblock.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "row-lock clause variant is not implemented",
        ));
    }
    Ok(Some(match lock.lock_type {
        LockType::Share => RowLockMode::Share,
        LockType::Update => RowLockMode::Update,
    }))
}

fn bind_values_scope(values: &sqlparser::ast::Values) -> Result<BoundScope> {
    let width = values.rows.first().map(|row| row.len()).unwrap_or(0);
    if values.rows.iter().any(|row| row.len() != width) {
        return Err(PgError::new(
            SqlState::SyntaxError,
            "VALUES lists must all be the same length",
        ));
    }
    let constants = constant_schema();
    let columns = (0..width)
        .map(|slot| {
            let data_type = values
                .rows
                .iter()
                .map(|row| &row[slot])
                .filter(|expression| {
                    !null_expression(expression) && unknown_string(expression).is_none()
                })
                .try_fold(None, |common, expression| {
                    let data_type = expression_type(expression, RowScope::Table(&constants))?;
                    Ok(Some(match common {
                        Some(common) => {
                            coercion::common_type(common, data_type).ok_or_else(|| {
                                PgError::new(
                                    SqlState::DatatypeMismatch,
                                    "VALUES types cannot be matched",
                                )
                            })?
                        }
                        None => data_type,
                    }))
                })?
                .unwrap_or(BaseType::Text);
            Ok(BoundColumn {
                name: format!("column{}", slot + 1),
                data_type: PgType::new(data_type),
                relation: String::new(),
                qualifier: String::new(),
                slot,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(BoundScope {
        columns,
        relation: None,
        qualifier: None,
    })
}

fn values_rows(
    query: &sqlparser::ast::Query,
    values: &sqlparser::ast::Values,
    context: &ExecutionContext,
) -> Result<StatementResult> {
    let scope = bind_values_scope(values)?;
    let columns = scope
        .columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name.clone(),
            type_oid: column.data_type.oid(),
            typmod: column.data_type.typmod,
        })
        .collect::<Vec<_>>();
    let constants = constant_schema();
    let mut rows = values
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .zip(&scope.columns)
                .map(|(expression, column)| {
                    evaluate_as(
                        expression,
                        column.data_type.base,
                        CastContext::Implicit,
                        RowScope::Table(&constants),
                        &[],
                        context,
                    )
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(order_by) = &query.order_by {
        let sqlparser::ast::OrderByKind::Expressions(orders) = &order_by.kind else {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "ORDER BY ALL is not implemented",
            ));
        };
        let orders = orders
            .iter()
            .map(|order| {
                let index = if let Some(position) = number_literal(&order.expr)
                    && !position.contains(['.', 'e', 'E'])
                {
                    position
                        .parse::<usize>()
                        .ok()
                        .and_then(|position| position.checked_sub(1))
                } else if let Expr::Identifier(identifier) = &order.expr {
                    scope
                        .resolve_column(std::slice::from_ref(identifier))
                        .ok()
                        .map(|(slot, _)| slot)
                } else {
                    None
                }
                .ok_or_else(|| {
                    PgError::new(
                        SqlState::InvalidColumnReference,
                        "ORDER BY position is not in select list",
                    )
                })?;
                if index >= columns.len() {
                    return Err(PgError::new(
                        SqlState::InvalidColumnReference,
                        "ORDER BY position is not in select list",
                    ));
                }
                let ascending = order.options.asc.unwrap_or(true);
                Ok((
                    index,
                    ascending,
                    order.options.nulls_first.unwrap_or(!ascending),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
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
                            let ordering = value_ordering(left, right)
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
    let (limit, offset) = match &query.limit_clause {
        None => (None, 0),
        Some(sqlparser::ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) if limit_by.is_empty() => (
            limit
                .as_ref()
                .map(|limit| row_count(limit, RowCountClause::Limit, context))
                .transpose()?
                .flatten(),
            offset
                .as_ref()
                .map(|offset| row_count(&offset.value, RowCountClause::Offset, context))
                .transpose()?
                .flatten()
                .unwrap_or(0),
        ),
        _ => {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "LIMIT clause is not implemented",
            ));
        }
    };
    Ok(StatementResult::Query(QueryResult {
        columns,
        rows: rows
            .into_iter()
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
            .collect(),
    }))
}

pub(super) fn select_rows(
    state: &DatabaseState,
    query: &sqlparser::ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
) -> Result<StatementResult> {
    if query.with.is_some() || query.fetch.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "query clause is not implemented",
        ));
    }
    select_lock_mode(query)?;
    let SetExpr::Select(select) = query.body.as_ref() else {
        if let SetExpr::Values(values) = query.body.as_ref() {
            return values_rows(query, values, context);
        }
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "query source is not implemented",
        ));
    };
    let GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "GROUP BY is not implemented",
        ));
    };
    if select.distinct.is_some()
        || select.into.is_some()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || select.having.is_some()
    {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "SELECT feature is not implemented",
        ));
    }
    let scope = bind_select_scope(state, select)?;
    let (limit, offset) = match &query.limit_clause {
        None => (None, 0),
        Some(sqlparser::ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) => {
            if !limit_by.is_empty() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "LIMIT BY is not implemented",
                ));
            }
            let limit = limit
                .as_ref()
                .map(|limit| row_count(limit, RowCountClause::Limit, context))
                .transpose()?
                .flatten();
            let offset = offset
                .as_ref()
                .map(|offset| row_count(&offset.value, RowCountClause::Offset, context))
                .transpose()?
                .flatten()
                .unwrap_or(0);
            (limit, offset)
        }
        Some(sqlparser::ast::LimitClause::OffsetCommaLimit { .. }) => {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "LIMIT clause is not implemented",
            ));
        }
    };
    if let Some(selection) = &select.selection {
        let base = expression_type(selection, RowScope::Bound(&scope))?;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (projections, columns) = projections_and_columns(&select.projection, &scope)?;
    let order_specs = query
        .order_by
        .as_ref()
        .map(|order_by| {
            if order_by.interpolate.is_some() {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "ORDER BY INTERPOLATE is not implemented",
                ));
            }
            let sqlparser::ast::OrderByKind::Expressions(orders) = &order_by.kind else {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "ORDER BY ALL is not implemented",
                ));
            };
            orders
                .iter()
                .map(|order| {
                    if order.with_fill.is_some() {
                        return Err(PgError::new(
                            SqlState::FeatureNotSupported,
                            "ORDER BY WITH FILL is not implemented",
                        ));
                    }
                    let key = if let Some(position) = number_literal(&order.expr)
                        && !position.contains(['.', 'e', 'E'])
                    {
                        let position = position.parse::<usize>().map_err(|_| {
                            PgError::new(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            )
                        })?;
                        if position == 0 || position > projections.len() {
                            return Err(PgError::new(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            ));
                        }
                        OrderKey::Output(position - 1)
                    } else if let Expr::Identifier(identifier) = &order.expr
                        && let Some(index) = columns
                            .iter()
                            .position(|column| column.name == identifier_name(identifier))
                    {
                        OrderKey::Output(index)
                    } else {
                        expression_type(&order.expr, RowScope::Bound(&scope))?;
                        OrderKey::Expression(&order.expr)
                    };
                    let ascending = order.options.asc.unwrap_or(true);
                    Ok(OrderSpec {
                        key,
                        ascending,
                        nulls_first: order.options.nulls_first.unwrap_or(!ascending),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let input_rows = if select.from.is_empty() {
        vec![Vec::new()]
    } else {
        let schema = state.catalog.table(
            scope
                .source_relation()
                .expect("FROM binding must retain its relation"),
        )?;
        state
            .tables
            .get(&schema.id)
            .expect("catalog table must have storage")
            .rows()
            .filter_map(|(_, chain)| visible_version(chain, snapshot, xid, &state.transactions))
            .map(|version| version.row.clone())
            .collect()
    };
    let mut rows =
        input_rows
            .iter()
            .try_fold(Vec::new(), |mut rows, row| -> Result<Vec<OrderedRow>> {
                if let Some(selection) = &select.selection {
                    match evaluate(selection, RowScope::Bound(&scope), row, context)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) | Value::Null => return Ok(rows),
                        _ => unreachable!("WHERE expression was type-checked"),
                    }
                }
                let values = projections
                    .iter()
                    .map(|projection| match projection {
                        Projection::Column(index) => Ok(row[*index].clone()),
                        Projection::Expression(expr) => {
                            evaluate(expr, RowScope::Bound(&scope), row, context)
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                let keys = order_specs
                    .iter()
                    .map(|order| match order.key {
                        OrderKey::Output(index) => Ok(values[index].clone()),
                        OrderKey::Expression(expression) => {
                            evaluate(expression, RowScope::Bound(&scope), row, context)
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                rows.push(OrderedRow { values, keys });
                Ok(rows)
            })?;
    rows.sort_by(|left, right| {
        order_specs
            .iter()
            .zip(left.keys.iter().zip(&right.keys))
            .find_map(|(spec, (left, right))| {
                let ordering = match (left, right) {
                    (Value::Null, Value::Null) => Ordering::Equal,
                    (Value::Null, _) => {
                        if spec.nulls_first {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (_, Value::Null) => {
                        if spec.nulls_first {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    _ => {
                        let ordering = value_ordering(left, right)
                            .expect("ORDER BY expression type was checked");
                        if spec.ascending {
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
    let rows = rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| row.values)
        .collect();
    Ok(StatementResult::Query(QueryResult { columns, rows }))
}

fn projections_and_columns<'a>(
    projection: &'a [SelectItem],
    scope: &BoundScope,
) -> Result<(Vec<Projection<'a>>, Vec<ColumnMeta>)> {
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        match item {
            SelectItem::Wildcard(_) => {
                for column in &scope.columns {
                    projections.push(Projection::Column(column.slot));
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            SelectItem::QualifiedWildcard(
                SelectItemQualifiedWildcardKind::ObjectName(object_name),
                _,
            ) => {
                let qualifier = name(object_name)?;
                let matching = scope
                    .columns
                    .iter()
                    .filter(|column| column.qualifier == qualifier)
                    .collect::<Vec<_>>();
                if matching.is_empty() && scope.qualifier.as_deref() != Some(qualifier.as_str()) {
                    return Err(PgError::new(
                        SqlState::UndefinedTable,
                        format!("missing FROM-clause entry for table {qualifier:?}"),
                    ));
                }
                for column in matching {
                    projections.push(Projection::Column(column.slot));
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            SelectItem::UnnamedExpr(Expr::Identifier(column)) => {
                let (index, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                projections.push(Projection::Column(index));
                columns.push(ColumnMeta {
                    name: column.value.clone(),
                    type_oid: data_type.oid(),
                    typmod: data_type.typmod,
                });
            }
            SelectItem::UnnamedExpr(Expr::CompoundIdentifier(identifiers)) => {
                let (index, data_type) = scope.resolve_column(identifiers)?;
                projections.push(Projection::Column(index));
                columns.push(ColumnMeta {
                    name: identifiers
                        .last()
                        .expect("compound identifier is non-empty")
                        .value
                        .clone(),
                    type_oid: data_type.oid(),
                    typmod: data_type.typmod,
                });
            }
            SelectItem::UnnamedExpr(expr) => {
                let data_type = expression_type(expr, RowScope::Bound(scope))?;
                projections.push(Projection::Expression(expr));
                columns.push(ColumnMeta {
                    name: "?column?".into(),
                    type_oid: data_type.oid(),
                    typmod: PgType::NO_TYPEMOD,
                });
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                let resolved = match expr {
                    Expr::Identifier(column) => {
                        Some(scope.resolve_column(std::slice::from_ref(column))?)
                    }
                    Expr::CompoundIdentifier(identifiers) => {
                        Some(scope.resolve_column(identifiers)?)
                    }
                    _ => None,
                };
                let (projection, data_type, typmod) = match resolved {
                    Some((slot, data_type)) => {
                        (Projection::Column(slot), data_type, data_type.typmod)
                    }
                    None => {
                        let data_type = PgType::new(expression_type(expr, RowScope::Bound(scope))?);
                        (Projection::Expression(expr), data_type, PgType::NO_TYPEMOD)
                    }
                };
                projections.push(projection);
                columns.push(ColumnMeta {
                    name: identifier_name(alias),
                    type_oid: data_type.oid(),
                    typmod,
                });
            }
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "SELECT projection is not implemented",
                ));
            }
        }
    }
    Ok((projections, columns))
}

fn row_count(
    expr: &Expr,
    clause: RowCountClause,
    context: &ExecutionContext,
) -> Result<Option<usize>> {
    if matches!(clause, RowCountClause::Limit)
        && matches!(expr, Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("all"))
    {
        return Ok(None);
    }
    let schema = constant_schema();
    let value = evaluate_as(
        expr,
        BaseType::Int8,
        CastContext::Implicit,
        RowScope::Table(&schema),
        &[],
        context,
    )
    .map_err(|error| {
        if error.sqlstate == SqlState::CannotCoerce {
            PgError::new(
                SqlState::DatatypeMismatch,
                match clause {
                    RowCountClause::Limit => "argument of LIMIT must be type bigint",
                    RowCountClause::Offset => "argument of OFFSET must be type bigint",
                },
            )
        } else {
            error
        }
    })?;
    match value {
        Value::Null => Ok(None),
        Value::Int8(value) if value >= 0 => Ok(Some(usize::try_from(value).unwrap_or(usize::MAX))),
        Value::Int8(_) => Err(PgError::new(
            match clause {
                RowCountClause::Limit => SqlState::InvalidRowCountInLimitClause,
                RowCountClause::Offset => SqlState::InvalidRowCountInResultOffsetClause,
            },
            match clause {
                RowCountClause::Limit => "LIMIT must not be negative",
                RowCountClause::Offset => "OFFSET must not be negative",
            },
        )),
        _ => unreachable!("row count was coerced to bigint"),
    }
}
