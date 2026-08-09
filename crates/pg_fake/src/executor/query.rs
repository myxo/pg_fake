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
                qualifier: String::new(),
                slot,
                unqualified: true,
                wildcard: true,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(BoundScope { columns })
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
    let input_rows = source_rows(state, select, &scope, xid, snapshot, context)?;
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
                    if column.wildcard {
                        projections.push(Projection::Column(column.slot));
                        columns.push(ColumnMeta {
                            name: column.name.clone(),
                            type_oid: column.data_type.oid(),
                            typmod: column.data_type.typmod,
                        });
                    }
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
                    .filter(|column| column.qualifier == qualifier && column.wildcard)
                    .collect::<Vec<_>>();
                if matching.is_empty()
                    && !scope
                        .columns
                        .iter()
                        .any(|column| column.qualifier == qualifier)
                {
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

fn source_rows(
    state: &DatabaseState,
    select: &sqlparser::ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
) -> Result<Vec<Vec<Value>>> {
    if select.from.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    let mut next_slot = 0;
    let mut rows = vec![vec![Value::Null; scope.columns.len()]];
    for table in &select.from {
        let source = table_rows(state, table, scope, xid, snapshot, context, &mut next_slot)?;
        rows = rows
            .into_iter()
            .flat_map(|left| {
                source.iter().map(move |right| {
                    left.iter()
                        .zip(right)
                        .map(|(left, right)| {
                            if left.is_null() {
                                right.clone()
                            } else {
                                left.clone()
                            }
                        })
                        .collect()
                })
            })
            .collect();
    }
    Ok(rows)
}

fn table_rows(
    state: &DatabaseState,
    table: &sqlparser::ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    next_slot: &mut usize,
) -> Result<Vec<Vec<Value>>> {
    let left_start = *next_slot;
    let mut rows = factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        next_slot,
    )?;
    for join in &table.joins {
        let right_start = *next_slot;
        let right_rows = factor_rows(
            state,
            &join.relation,
            scope,
            xid,
            snapshot,
            context,
            next_slot,
        )?;
        let mut joined = Vec::new();
        for left in &rows {
            for right in &right_rows {
                let row = left
                    .iter()
                    .zip(right)
                    .map(|(left, right)| {
                        if left.is_null() {
                            right.clone()
                        } else {
                            left.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                if join_matches(
                    &join.join_operator,
                    &row,
                    scope,
                    left_start,
                    right_start,
                    context,
                )? {
                    joined.push(row);
                }
            }
        }
        rows = joined;
    }
    Ok(rows)
}

fn factor_rows(
    state: &DatabaseState,
    factor: &TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    next_slot: &mut usize,
) -> Result<Vec<Vec<Value>>> {
    if let TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        return table_rows(
            state,
            table_with_joins,
            scope,
            xid,
            snapshot,
            context,
            next_slot,
        );
    }
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "FROM source is not implemented",
        ));
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?;
    let start = *next_slot;
    *next_slot += schema.columns.len();
    Ok(state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .rows()
        .filter_map(|(_, chain)| visible_version(chain, snapshot, xid, &state.transactions))
        .map(|version| {
            let mut row = vec![Value::Null; scope.columns.len()];
            row[start..start + version.row.len()].clone_from_slice(&version.row);
            row
        })
        .collect())
}

fn join_matches(
    operator: &sqlparser::ast::JoinOperator,
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
    context: &ExecutionContext,
) -> Result<bool> {
    let constraint = match operator {
        sqlparser::ast::JoinOperator::Join(constraint)
        | sqlparser::ast::JoinOperator::Inner(constraint)
        | sqlparser::ast::JoinOperator::CrossJoin(constraint) => constraint,
        _ => {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "join type is not implemented",
            ));
        }
    };
    match constraint {
        sqlparser::ast::JoinConstraint::None => Ok(matches!(
            operator,
            sqlparser::ast::JoinOperator::CrossJoin(_)
        )),
        sqlparser::ast::JoinConstraint::On(expression) => Ok(matches!(
            evaluate(expression, RowScope::Bound(scope), row, context)?,
            Value::Bool(true)
        )),
        sqlparser::ast::JoinConstraint::Using(names) => join_using_matches(
            names
                .iter()
                .map(name)
                .collect::<Result<Vec<_>>>()?
                .as_slice(),
            row,
            scope,
            left_start,
            right_start,
        ),
        sqlparser::ast::JoinConstraint::Natural => {
            let names = scope.columns[left_start..right_start]
                .iter()
                .filter(|left| {
                    scope.columns[right_start..]
                        .iter()
                        .any(|right| right.name == left.name)
                })
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            join_using_matches(&names, row, scope, left_start, right_start)
        }
    }
}

fn join_using_matches(
    names: &[String],
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Result<bool> {
    for name in names {
        let left = scope.columns[left_start..right_start]
            .iter()
            .find(|column| column.unqualified && column.name == *name)
            .expect("bound USING column must exist in left source");
        let right = scope.columns[right_start..]
            .iter()
            .find(|column| !column.unqualified && column.name == *name)
            .expect("bound USING column must exist in right source");
        let data_type = coercion::common_type(left.data_type.base, right.data_type.base)
            .expect("bound USING columns must have a common type");
        let left = coercion::coerce(
            row[left.slot].clone(),
            left.data_type.base,
            PgType::new(data_type),
            CastContext::Implicit,
        )?;
        let right = coercion::coerce(
            row[right.slot].clone(),
            right.data_type.base,
            PgType::new(data_type),
            CastContext::Implicit,
        )?;
        if left.is_null() || right.is_null() || left != right {
            return Ok(false);
        }
    }
    Ok(true)
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
