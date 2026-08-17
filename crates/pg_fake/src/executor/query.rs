use super::*;
use ast::VisitMut as _;
use sqlparser::ast;

struct SubqueryMaterializer<'a> {
    state: &'a DatabaseState,
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a StatementExecutionContext,
    error: Option<PgError>,
    defer_unresolved: bool,
    scopes: Vec<BoundScope>,
}

impl SubqueryMaterializer<'_> {
    fn execute(&self, query: &ast::Query) -> Result<QueryResult> {
        let query = materialize_uncorrelated_subqueries(
            self.state,
            &ast::Statement::Query(Box::new(query.clone())),
            self.xid,
            self.snapshot,
            self.context,
        )?;
        let ast::Statement::Query(query) = query else {
            unreachable!("subquery statement remains a query");
        };
        let StatementResult::Query(result) =
            execute_query(self.state, &query, self.xid, self.snapshot, self.context)?
        else {
            unreachable!("subquery execution returns query rows");
        };
        Ok(result)
    }
}

impl ast::VisitorMut for SubqueryMaterializer<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let scope = match query.body.as_ref() {
            ast::SetExpr::Select(select) => bind_query_scope(&self.state.catalog, select),
            _ => Ok(BoundScope {
                columns: Vec::new(),
            }),
        }
        .unwrap_or(BoundScope {
            columns: Vec::new(),
        });
        self.scopes.push(scope);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let original = expr.clone();
        let correlation_candidate = original.clone();
        let result = (|| match original {
            ast::Expr::AnyOp {
                left,
                compare_op,
                right,
                is_some,
            } => {
                let ast::Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(ast::Expr::AnyOp {
                    left,
                    compare_op,
                    right: Box::new(ast::Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| {
                                crate::analyzer::create_typed_literal(row[0].clone(), data_type)
                            })
                            .collect(),
                    )),
                    is_some,
                }))
            }
            ast::Expr::AllOp {
                left,
                compare_op,
                right,
            } => {
                let ast::Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(ast::Expr::AllOp {
                    left,
                    compare_op,
                    right: Box::new(ast::Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| {
                                crate::analyzer::create_typed_literal(row[0].clone(), data_type)
                            })
                            .collect(),
                    )),
                }))
            }
            ast::Expr::Subquery(query) => {
                let result = self.execute(&query)?;
                if result.columns.len() != 1 {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery must return only one column",
                    ));
                }
                if result.rows.len() > 1 {
                    return Err(PgError::create(
                        SqlState::CardinalityViolation,
                        "more than one row returned by a subquery used as an expression",
                    ));
                }
                let data_type = PgType::create_with_typmod(
                    BaseType::resolve_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(crate::analyzer::create_typed_literal(
                    result
                        .rows
                        .into_iter()
                        .next()
                        .map(|row| row[0].clone())
                        .unwrap_or(Value::Null),
                    data_type,
                )))
            }
            ast::Expr::Exists { subquery, negated } => {
                Ok(Some(crate::analyzer::create_typed_literal(
                    Value::Bool(self.execute(&subquery)?.rows.is_empty() == negated),
                    PgType::create(BaseType::Bool),
                )))
            }
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let result = self.execute(&subquery)?;
                let left_width = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if result.columns.len() != left_width {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let types = result
                    .columns
                    .iter()
                    .map(|column| {
                        PgType::create_with_typmod(
                            BaseType::resolve_oid(column.type_oid)
                                .expect("query result type OID is supported"),
                            column.typmod,
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(Some(ast::Expr::InList {
                    expr,
                    list: result
                        .rows
                        .into_iter()
                        .map(|row| {
                            let fields = row
                                .into_iter()
                                .zip(&types)
                                .map(|(value, data_type)| {
                                    crate::analyzer::create_typed_literal(value, *data_type)
                                })
                                .collect::<Vec<_>>();
                            if fields.len() == 1 {
                                fields.into_iter().next().expect("row has one field")
                            } else {
                                ast::Expr::Tuple(fields)
                            }
                        })
                        .collect(),
                    negated,
                }))
            }
            _ => Ok(None),
        })();
        match result {
            Ok(Some(value)) => *expr = value,
            Ok(None) => {}
            Err(error)
                if self.defer_unresolved
                    && matches!(
                        error.sqlstate,
                        SqlState::UndefinedColumn | SqlState::UndefinedTable
                    ) =>
            {
                match self.scopes.last().map(|outer| {
                    references_outer_scope(&self.state.catalog, &correlation_candidate, outer)
                }) {
                    Some(Ok(true)) => {}
                    Some(Err(scope_error)) => self.error = Some(scope_error),
                    _ => self.error = Some(error),
                }
            }
            Err(error) => self.error = Some(error),
        }
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn materialize_uncorrelated_subqueries(
    state: &DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<ast::Statement> {
    let mut statement = statement.clone();
    materialize_subqueries(state, &mut statement, xid, snapshot, context, true)?;
    Ok(statement)
}

fn materialize_subqueries<V: ast::VisitMut>(
    state: &DatabaseState,
    value: &mut V,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    defer_unresolved: bool,
) -> Result<()> {
    let mut materializer = SubqueryMaterializer {
        state,
        xid,
        snapshot,
        context,
        error: None,
        defer_unresolved,
        scopes: Vec::new(),
    };
    let _ = value.visit(&mut materializer);
    if let Some(error) = materializer.error {
        return Err(error);
    }
    Ok(())
}

struct OuterReferenceSubstituter<'a> {
    catalog: &'a Catalog,
    outer_scope: &'a BoundScope,
    outer_row: &'a [Value],
    scopes: Vec<BoundScope>,
    error: Option<PgError>,
    substituted: bool,
}

impl ast::VisitorMut for OuterReferenceSubstituter<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        let ast::SetExpr::Select(select) = query.body.as_ref() else {
            self.scopes.push(BoundScope {
                columns: Vec::new(),
            });
            return std::ops::ControlFlow::Continue(());
        };
        match bind_query_scope(self.catalog, select) {
            Ok(scope) => self.scopes.push(scope),
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let identifiers = match expression {
            ast::Expr::Identifier(identifier) => std::slice::from_ref(identifier),
            ast::Expr::CompoundIdentifier(identifiers) => identifiers.as_slice(),
            _ => return std::ops::ControlFlow::Continue(()),
        };
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
                Ok(_) => return std::ops::ControlFlow::Continue(()),
                Err(error)
                    if identifiers.len() == 1 && error.sqlstate == SqlState::UndefinedColumn => {}
                Err(error) => {
                    self.error = Some(error);
                    return std::ops::ControlFlow::Break(());
                }
            }
        }
        match self.outer_scope.resolve_column(identifiers) {
            Ok((_, data_type)) => {
                match RowScope::Bound(self.outer_scope)
                    .resolve_column_value(identifiers, self.outer_row)
                {
                    Ok(value) => {
                        *expression = crate::analyzer::create_typed_literal(value, data_type);
                        self.substituted = true;
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
}

fn references_outer_scope(
    catalog: &Catalog,
    expression: &ast::Expr,
    outer_scope: &BoundScope,
) -> Result<bool> {
    let mut expression = expression.clone();
    let outer_row = vec![Value::Null; outer_scope.columns.len()];
    let mut substituter = OuterReferenceSubstituter {
        catalog,
        outer_scope,
        outer_row: &outer_row,
        scopes: Vec::new(),
        error: None,
        substituted: false,
    };
    let _ = expression.visit(&mut substituter);
    substituter.error.map_or(Ok(substituter.substituted), Err)
}

fn evaluate_query_expression(
    state: &DatabaseState,
    expression: &ast::Expr,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<Value> {
    let mut expression = expression.clone();
    let mut substituter = OuterReferenceSubstituter {
        catalog: &state.catalog,
        outer_scope: scope,
        outer_row: row,
        scopes: Vec::new(),
        error: None,
        substituted: false,
    };
    let _ = expression.visit(&mut substituter);
    if let Some(error) = substituter.error {
        return Err(error);
    }
    materialize_subqueries(state, &mut expression, xid, snapshot, context, false)?;
    evaluate(&expression, RowScope::Bound(scope), row, context)
}

pub(crate) fn describe_query_result_columns(
    state: &DatabaseState,
    statement: &ast::Statement,
) -> Result<Vec<ColumnMeta>> {
    let ast::Statement::Query(query) = statement else {
        return Ok(Vec::new());
    };
    match query.body.as_ref() {
        ast::SetExpr::Select(select) => bind_select_scope(state, select).and_then(|scope| {
            build_projection_plan(state, &select.projection, &scope).map(|(_, columns)| columns)
        }),
        ast::SetExpr::Values(values) => bind_values_scope(values).map(|scope| {
            scope
                .columns
                .iter()
                .map(|column| ColumnMeta {
                    name: column.name.clone(),
                    type_oid: column.data_type.map_to_oid(),
                    typmod: column.data_type.typmod,
                })
                .collect()
        }),
        _ => Ok(Vec::new()),
    }
}
enum ProjectionSource<'a> {
    Column(usize),
    Merged(usize, usize, PgType),
    Expression(&'a ast::Expr),
}
enum OrderKey<'a> {
    Output(usize),
    Expression(&'a ast::Expr),
}
enum RowCountClause {
    Limit,
    Offset,
}
struct RowOrderSpec<'a> {
    key: OrderKey<'a>,
    ascending: bool,
    nulls_first: bool,
}
struct OrderedRow {
    values: Vec<Value>,
    keys: Vec<Value>,
}
#[derive(Eq, Hash, PartialEq)]
enum JoinKey {
    Bool(bool),
    Int2(i16),
    Int4(i32),
    Int8(i64),
    Text(String),
    Bytea(Vec<u8>),
    Uuid(uuid::Uuid),
}
pub(super) fn resolve_select_lock_mode(query: &ast::Query) -> Result<Option<RowLockMode>> {
    if query.locks.len() > 1 {
        return reject_unsupported("multiple row-lock clauses are not implemented");
    }
    let Some(lock) = query.locks.first() else {
        return Ok(None);
    };
    if lock.of.is_some() || lock.nonblock.is_some() {
        return reject_unsupported("row-lock clause variant is not implemented");
    }
    Ok(Some(match lock.lock_type {
        ast::LockType::Share => RowLockMode::Share,
        ast::LockType::Update => RowLockMode::Update,
    }))
}

fn bind_values_scope(values: &ast::Values) -> Result<BoundScope> {
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
                .try_fold(None, |common, expression| {
                    let data_type = infer_expression_type(expression, RowScope::Table(&constants))?;
                    Ok(Some(match common {
                        Some(common) => coercion::resolve_common_type(common, data_type)
                            .ok_or_else(|| {
                                PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "VALUES types cannot be matched",
                                )
                            })?,
                        None => data_type,
                    }))
                })?
                .unwrap_or(BaseType::Text);
            Ok(BoundColumn {
                name: format!("column{}", slot + 1),
                data_type: PgType::create(data_type),
                qualifier: String::new(),
                slot,
                merged: None,
                unqualified: true,
                wildcard: true,
                depth: 0,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(BoundScope { columns })
}

fn execute_values_query(
    query: &ast::Query,
    values: &ast::Values,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
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
    let constants = create_constant_expression_schema();
    let mut rows = values
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .zip(&scope.columns)
                .map(|(expression, column)| {
                    evaluate_and_coerce(
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
        let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
            return reject_unsupported("ORDER BY ALL is not implemented");
        };
        let orders = orders
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
    let (limit, offset) = match &query.limit_clause {
        None => (None, 0),
        Some(ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) if limit_by.is_empty() => (
            limit
                .as_ref()
                .map(|limit| evaluate_row_count(limit, RowCountClause::Limit, context))
                .transpose()?
                .flatten(),
            offset
                .as_ref()
                .map(|offset| evaluate_row_count(&offset.value, RowCountClause::Offset, context))
                .transpose()?
                .flatten()
                .unwrap_or(0),
        ),
        _ => {
            return reject_unsupported("LIMIT clause is not implemented");
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

pub(super) fn execute_query(
    state: &DatabaseState,
    query: &ast::Query,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<StatementResult> {
    if query.with.is_some() || query.fetch.is_some() {
        return reject_unsupported("query clause is not implemented");
    }
    resolve_select_lock_mode(query)?;
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        if let ast::SetExpr::Values(values) = query.body.as_ref() {
            return execute_values_query(query, values, context);
        }
        return reject_unsupported("query source is not implemented");
    };
    let ast::GroupByExpr::Expressions(group_by, modifiers) = &select.group_by else {
        return reject_unsupported("GROUP BY is not implemented");
    };
    if select.distinct.is_some()
        || select.into.is_some()
        || !group_by.is_empty()
        || !modifiers.is_empty()
        || select.having.is_some()
    {
        return reject_unsupported("SELECT feature is not implemented");
    }
    let scope = bind_select_scope(state, select)?;
    let (limit, offset) = match &query.limit_clause {
        None => (None, 0),
        Some(ast::LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) => {
            if !limit_by.is_empty() {
                return reject_unsupported("LIMIT BY is not implemented");
            }
            let limit = limit
                .as_ref()
                .map(|limit| evaluate_row_count(limit, RowCountClause::Limit, context))
                .transpose()?
                .flatten();
            let offset = offset
                .as_ref()
                .map(|offset| evaluate_row_count(&offset.value, RowCountClause::Offset, context))
                .transpose()?
                .flatten()
                .unwrap_or(0);
            (limit, offset)
        }
        Some(ast::LimitClause::OffsetCommaLimit { .. }) => {
            return reject_unsupported("LIMIT clause is not implemented");
        }
    };
    if let Some(selection) = &select.selection {
        let base = infer_query_expression_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !is_null_literal(selection) {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (projections, columns) = build_projection_plan(state, &select.projection, &scope)?;
    let order_specs = query
        .order_by
        .as_ref()
        .map(|order_by| {
            if order_by.interpolate.is_some() {
                return reject_unsupported("ORDER BY INTERPOLATE is not implemented");
            }
            let ast::OrderByKind::Expressions(orders) = &order_by.kind else {
                return reject_unsupported("ORDER BY ALL is not implemented");
            };
            orders
                .iter()
                .map(|order| {
                    if order.with_fill.is_some() {
                        return reject_unsupported("ORDER BY WITH FILL is not implemented");
                    }
                    let key = if let Some(position) = extract_number_literal(&order.expr)
                        && !position.contains(['.', 'e', 'E'])
                    {
                        let position = position.parse::<usize>().map_err(|_| {
                            PgError::create(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            )
                        })?;
                        if position == 0 || position > projections.len() {
                            return Err(PgError::create(
                                SqlState::InvalidColumnReference,
                                "ORDER BY position is not in select list",
                            ));
                        }
                        OrderKey::Output(position - 1)
                    } else if let ast::Expr::Identifier(identifier) = &order.expr
                        && let Some(index) = columns
                            .iter()
                            .position(|column| column.name == normalize_identifier(identifier))
                    {
                        OrderKey::Output(index)
                    } else {
                        infer_query_expression_type(state, &order.expr, &scope)?;
                        OrderKey::Expression(&order.expr)
                    };
                    let ascending = order.options.asc.unwrap_or(true);
                    Ok(RowOrderSpec {
                        key,
                        ascending,
                        nulls_first: order.options.nulls_first.unwrap_or(!ascending),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut rows = Vec::new();
    visit_query_source_rows(
        state,
        select,
        &scope,
        xid,
        snapshot,
        context,
        select.selection.as_ref(),
        &mut |row| {
            if let Some(selection) = &select.selection {
                match evaluate_query_expression(
                    state, selection, &scope, row, xid, snapshot, context,
                )? {
                    Value::Bool(true) => {}
                    Value::Bool(false) | Value::Null => return Ok(()),
                    _ => unreachable!("WHERE expression was type-checked"),
                }
            }
            let values = projections
                .iter()
                .map(|projection| match projection {
                    ProjectionSource::Column(index) => Ok(row[*index].clone()),
                    ProjectionSource::Merged(left, right, data_type) => {
                        let value = if row[*left].is_null() {
                            row[*right].clone()
                        } else {
                            row[*left].clone()
                        };
                        if value.is_null() {
                            Ok(value)
                        } else {
                            coercion::coerce(
                                value.clone(),
                                value
                                    .get_base_type()
                                    .expect("non-null value has a base type"),
                                *data_type,
                                CastContext::Implicit,
                            )
                        }
                    }
                    ProjectionSource::Expression(expr) => {
                        evaluate_query_expression(state, expr, &scope, row, xid, snapshot, context)
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            let keys = order_specs
                .iter()
                .map(|order| match order.key {
                    OrderKey::Output(index) => Ok(values[index].clone()),
                    OrderKey::Expression(expression) => evaluate_query_expression(
                        state, expression, &scope, row, xid, snapshot, context,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            rows.push(OrderedRow { values, keys });
            Ok(())
        },
    )?;
    if !order_specs.is_empty() {
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
                            let ordering = compare_values(left, right)
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
    }
    let rows = rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| row.values)
        .collect();
    Ok(StatementResult::Query(QueryResult { columns, rows }))
}

fn build_projection_plan<'a>(
    state: &DatabaseState,
    projection: &'a [ast::SelectItem],
    scope: &BoundScope,
) -> Result<(Vec<ProjectionSource<'a>>, Vec<ColumnMeta>)> {
    let mut projections = Vec::new();
    let mut columns = Vec::new();
    for item in projection {
        match item {
            ast::SelectItem::Wildcard(_) => {
                for column in &scope.columns {
                    if column.wildcard {
                        projections.push(match column.merged {
                            Some((left, right)) => {
                                ProjectionSource::Merged(left, right, column.data_type)
                            }
                            None => ProjectionSource::Column(column.slot),
                        });
                        columns.push(ColumnMeta {
                            name: column.name.clone(),
                            type_oid: column.data_type.map_to_oid(),
                            typmod: column.data_type.typmod,
                        });
                    }
                }
            }
            ast::SelectItem::QualifiedWildcard(
                ast::SelectItemQualifiedWildcardKind::ObjectName(object_name),
                _,
            ) => {
                let qualifier = normalize_unqualified_object_name(object_name)?;
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
                    return Err(PgError::create(
                        SqlState::UndefinedTable,
                        format!("missing FROM-clause entry for table {qualifier:?}"),
                    ));
                }
                for column in matching {
                    projections.push(ProjectionSource::Column(column.slot));
                    columns.push(ColumnMeta {
                        name: column.name.clone(),
                        type_oid: column.data_type.map_to_oid(),
                        typmod: column.data_type.typmod,
                    });
                }
            }
            ast::SelectItem::UnnamedExpr(expression @ ast::Expr::Identifier(column)) => {
                let (_, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                projections.push(ProjectionSource::Expression(expression));
                columns.push(ColumnMeta {
                    name: column.value.clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(
                expression @ ast::Expr::CompoundIdentifier(identifiers),
            ) => {
                let (_, data_type) = scope.resolve_column(identifiers)?;
                projections.push(ProjectionSource::Expression(expression));
                columns.push(ColumnMeta {
                    name: identifiers
                        .last()
                        .expect("compound identifier is non-empty")
                        .value
                        .clone(),
                    type_oid: data_type.map_to_oid(),
                    typmod: data_type.typmod,
                });
            }
            ast::SelectItem::UnnamedExpr(expr) => {
                let data_type = infer_query_expression_type(state, expr, scope)?;
                projections.push(ProjectionSource::Expression(expr));
                columns.push(ColumnMeta {
                    name: "?column?".into(),
                    type_oid: data_type.map_to_oid(),
                    typmod: PgType::NO_TYPEMOD,
                });
            }
            ast::SelectItem::ExprWithAlias { expr, alias } => {
                let resolved = match expr {
                    ast::Expr::Identifier(column) => {
                        Some(scope.resolve_column(std::slice::from_ref(column))?)
                    }
                    ast::Expr::CompoundIdentifier(identifiers) => {
                        Some(scope.resolve_column(identifiers)?)
                    }
                    _ => None,
                };
                let (projection, data_type, typmod) = match resolved {
                    Some((_, data_type)) => (
                        ProjectionSource::Expression(expr),
                        data_type,
                        data_type.typmod,
                    ),
                    None => {
                        let data_type = infer_query_expression_type(state, expr, scope)?;
                        (
                            ProjectionSource::Expression(expr),
                            data_type,
                            PgType::NO_TYPEMOD,
                        )
                    }
                };
                projections.push(projection);
                columns.push(ColumnMeta {
                    name: normalize_identifier(alias),
                    type_oid: data_type.map_to_oid(),
                    typmod,
                });
            }
            _ => {
                return reject_unsupported("SELECT projection is not implemented");
            }
        }
    }
    Ok((projections, columns))
}

fn infer_query_expression_type(
    state: &DatabaseState,
    expr: &ast::Expr,
    scope: &BoundScope,
) -> Result<PgType> {
    super::scope::infer_expression_data_type(&state.catalog, expr, scope)
}

fn materialize_source_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
) -> Result<Vec<Vec<Value>>> {
    if select.from.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    let mut next_slot = 0;
    let mut rows = vec![vec![Value::Null; scope.columns.len()]];
    for table in &select.from {
        let source = materialize_table_with_joins_rows(
            state,
            table,
            scope,
            xid,
            snapshot,
            context,
            selection,
            &mut next_slot,
        )?;
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

fn visit_query_source_rows(
    state: &DatabaseState,
    select: &ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    if let [table] = select.from.as_slice()
        && can_stream_inner_join(table)
    {
        return visit_streamed_inner_join_rows(
            state, table, scope, xid, snapshot, context, selection, visit,
        );
    }
    for row in materialize_source_rows(state, select, scope, xid, snapshot, context, selection)? {
        visit(&row)?;
    }
    Ok(())
}

fn can_stream_inner_join(table: &ast::TableWithJoins) -> bool {
    matches!(table.relation, ast::TableFactor::Table { .. })
        && table.joins.iter().all(|join| {
            matches!(join.relation, ast::TableFactor::Table { .. })
                && matches!(
                    join.join_operator,
                    ast::JoinOperator::Join(_)
                        | ast::JoinOperator::Inner(_)
                        | ast::JoinOperator::CrossJoin(_)
                )
        })
}

fn visit_streamed_inner_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut starts = Vec::with_capacity(table.joins.len() + 1);
    let mut next_slot = 0;
    for factor in
        std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    {
        let ast::TableFactor::Table {
            name: table_name, ..
        } = factor
        else {
            unreachable!("streamable sources are tables");
        };
        starts.push(next_slot);
        next_slot += state
            .catalog
            .require_table(&normalize_unqualified_object_name(table_name)?)?
            .columns
            .len();
    }
    if table.joins.len() == 1
        && let Some((left_slot, right_slot)) =
            resolve_hash_join_slots(&table.joins[0].join_operator, scope, starts[0], starts[1])
    {
        return visit_hash_join_rows(
            state, table, scope, xid, snapshot, context, selection, starts[0], starts[1],
            left_slot, right_slot, visit,
        );
    }
    visit_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[0],
        &mut |row| {
            visit_nested_loop_join_rows(
                state, table, scope, xid, snapshot, context, selection, &starts, 0, row, visit,
            )
        },
    )
}

fn resolve_hash_join_slots(
    operator: &ast::JoinOperator,
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Option<(usize, usize)> {
    let (ast::JoinOperator::Join(ast::JoinConstraint::On(expression))
    | ast::JoinOperator::Inner(ast::JoinConstraint::On(expression))) = operator
    else {
        return None;
    };
    let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::Eq,
        right,
    } = expression
    else {
        return None;
    };
    let (left_slot, left_type) = resolve_hash_expression_slot(left, scope)?;
    let (right_slot, right_type) = resolve_hash_expression_slot(right, scope)?;
    if left_type.base != right_type.base
        || !matches!(
            left_type.base,
            BaseType::Bool
                | BaseType::Int2
                | BaseType::Int4
                | BaseType::Int8
                | BaseType::Text
                | BaseType::Varchar
                | BaseType::Bpchar
                | BaseType::Bytea
                | BaseType::Uuid
        )
    {
        return None;
    }
    if (left_start..right_start).contains(&right_slot)
        && (right_start..scope.columns.len()).contains(&left_slot)
    {
        return Some((right_slot, left_slot));
    }
    ((left_start..right_start).contains(&left_slot)
        && (right_start..scope.columns.len()).contains(&right_slot))
    .then_some((left_slot, right_slot))
}

fn resolve_hash_expression_slot(
    expression: &ast::Expr,
    scope: &BoundScope,
) -> Option<(usize, PgType)> {
    match expression {
        ast::Expr::Identifier(identifier) => {
            scope.resolve_column(std::slice::from_ref(identifier)).ok()
        }
        ast::Expr::CompoundIdentifier(identifiers) => scope.resolve_column(identifiers).ok(),
        _ => None,
    }
}

fn visit_hash_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    left_start: usize,
    right_start: usize,
    left_slot: usize,
    right_slot: usize,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut right_rows = std::collections::HashMap::<JoinKey, Vec<Vec<Value>>>::new();
    visit_table_factor_rows(
        state,
        &table.joins[0].relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        right_start,
        &mut |row| {
            if let Some(key) = create_hash_join_key(&row[right_slot]) {
                right_rows.entry(key).or_default().push(row.to_vec());
            }
            Ok(())
        },
    )?;
    visit_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        left_start,
        &mut |left| {
            let Some(key) = create_hash_join_key(&left[left_slot]) else {
                return Ok(());
            };
            let Some(matches) = right_rows.get(&key) else {
                return Ok(());
            };
            for right in matches {
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
                visit(&row)?;
            }
            Ok(())
        },
    )
}

fn create_hash_join_key(value: &Value) -> Option<JoinKey> {
    match value {
        Value::Null => None,
        Value::Bool(value) => Some(JoinKey::Bool(*value)),
        Value::Int2(value) => Some(JoinKey::Int2(*value)),
        Value::Int4(value) => Some(JoinKey::Int4(*value)),
        Value::Int8(value) => Some(JoinKey::Int8(*value)),
        Value::Text(value) => Some(JoinKey::Text(value.clone())),
        Value::Bytea(value) => Some(JoinKey::Bytea(value.clone())),
        Value::Uuid(value) => Some(JoinKey::Uuid(*value)),
        _ => None,
    }
}

fn visit_nested_loop_join_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    starts: &[usize],
    index: usize,
    left: &[Value],
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let Some(join) = table.joins.get(index) else {
        return visit(left);
    };
    visit_table_factor_rows(
        state,
        &join.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[index + 1],
        &mut |right| {
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
            if evaluate_join_condition(
                state,
                &join.join_operator,
                &row,
                scope,
                starts[0],
                starts[index + 1],
                xid,
                snapshot,
                context,
            )? {
                visit_nested_loop_join_rows(
                    state,
                    table,
                    scope,
                    xid,
                    snapshot,
                    context,
                    selection,
                    starts,
                    index + 1,
                    &row,
                    visit,
                )?;
            }
            Ok(())
        },
    )
}

fn visit_table_factor_rows(
    state: &DatabaseState,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    start: usize,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let ast::TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        unreachable!("streamable source is a table");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let schema = state
        .catalog
        .require_table(&normalize_unqualified_object_name(table_name)?)?;
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        collect_pushdown_filters(
            selection,
            scope,
            start,
            start + schema.columns.len(),
            &mut filters,
        );
    }
    for (_, chain) in state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
    {
        let Some(version) = find_visible_version(chain, snapshot, xid, &state.transactions) else {
            continue;
        };
        let mut row = vec![Value::Null; scope.columns.len()];
        row[start..start + version.row.len()].clone_from_slice(&version.row);
        let passes = filters.iter().try_fold(true, |passes, filter| {
            if !passes {
                return Ok(false);
            }
            Ok(matches!(
                evaluate(filter, RowScope::Bound(scope), &row, context)?,
                Value::Bool(true)
            ))
        })?;
        if passes {
            visit(&row)?;
        }
    }
    Ok(())
}

fn materialize_table_with_joins_rows(
    state: &DatabaseState,
    table: &ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    next_slot: &mut usize,
) -> Result<Vec<Vec<Value>>> {
    let left_start = *next_slot;
    let mut rows = materialize_table_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        next_slot,
    )?;
    for join in &table.joins {
        let right_start = *next_slot;
        let right_rows = materialize_table_factor_rows(
            state,
            &join.relation,
            scope,
            xid,
            snapshot,
            context,
            selection,
            next_slot,
        )?;
        let mut joined = Vec::new();
        let mut matched_right = vec![false; right_rows.len()];
        for left in &rows {
            let mut matched_left = false;
            for (index, right) in right_rows.iter().enumerate() {
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
                if evaluate_join_condition(
                    state,
                    &join.join_operator,
                    &row,
                    scope,
                    left_start,
                    right_start,
                    xid,
                    snapshot,
                    context,
                )? {
                    matched_left = true;
                    matched_right[index] = true;
                    joined.push(row);
                }
            }
            if !matched_left
                && matches!(
                    join.join_operator,
                    ast::JoinOperator::Left(_)
                        | ast::JoinOperator::LeftOuter(_)
                        | ast::JoinOperator::FullOuter(_)
                )
            {
                joined.push(left.clone());
            }
        }
        if matches!(
            join.join_operator,
            ast::JoinOperator::Right(_)
                | ast::JoinOperator::RightOuter(_)
                | ast::JoinOperator::FullOuter(_)
        ) {
            joined.extend(
                right_rows
                    .iter()
                    .zip(matched_right)
                    .filter_map(|(row, matched)| (!matched).then_some(row.clone())),
            );
        }
        rows = joined;
    }
    Ok(rows)
}

fn materialize_table_factor_rows(
    state: &DatabaseState,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
    selection: Option<&ast::Expr>,
    next_slot: &mut usize,
) -> Result<Vec<Vec<Value>>> {
    if let ast::TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        return materialize_table_with_joins_rows(
            state,
            table_with_joins,
            scope,
            xid,
            snapshot,
            context,
            selection,
            next_slot,
        );
    }
    if let ast::TableFactor::Derived {
        lateral,
        subquery,
        alias: Some(_),
        ..
    } = factor
    {
        if *lateral {
            return reject_unsupported("LATERAL derived tables are not implemented");
        }
        let StatementResult::Query(result) =
            execute_query(state, subquery, xid, snapshot, context)?
        else {
            unreachable!("derived query execution returns query rows");
        };
        let start = *next_slot;
        *next_slot += result.columns.len();
        return Ok(result
            .rows
            .into_iter()
            .map(|values| {
                let mut row = vec![Value::Null; scope.columns.len()];
                row[start..start + values.len()].clone_from_slice(&values);
                row
            })
            .collect());
    }
    let ast::TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        return reject_unsupported("FROM source is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let schema = state
        .catalog
        .require_table(&normalize_unqualified_object_name(table_name)?)?;
    let start = *next_slot;
    *next_slot += schema.columns.len();
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        collect_pushdown_filters(
            selection,
            scope,
            start,
            start + schema.columns.len(),
            &mut filters,
        );
    }
    state
        .tables
        .get(&schema.id)
        .expect("catalog table must have storage")
        .iterate_version_chains()
        .filter_map(|(_, chain)| find_visible_version(chain, snapshot, xid, &state.transactions))
        .map(|version| {
            let mut row = vec![Value::Null; scope.columns.len()];
            row[start..start + version.row.len()].clone_from_slice(&version.row);
            let passes = filters.iter().try_fold(true, |passes, filter| {
                if !passes {
                    return Ok(false);
                }
                Ok(matches!(
                    evaluate(filter, RowScope::Bound(scope), &row, context)?,
                    Value::Bool(true)
                ))
            })?;
            Ok(passes.then_some(row))
        })
        .collect::<Result<Vec<_>>>()
        .map(|rows| rows.into_iter().flatten().collect())
}

fn collect_pushdown_filters<'a>(
    expr: &'a ast::Expr,
    scope: &BoundScope,
    start: usize,
    end: usize,
    filters: &mut Vec<&'a ast::Expr>,
) {
    if let ast::Expr::BinaryOp {
        left,
        op: ast::BinaryOperator::And,
        right,
    } = expr
    {
        collect_pushdown_filters(left, scope, start, end, filters);
        collect_pushdown_filters(right, scope, start, end, filters);
        return;
    }
    let ast::Expr::BinaryOp { left, right, .. } = expr else {
        return;
    };
    let column = match (left.as_ref(), right.as_ref()) {
        (ast::Expr::Identifier(column), ast::Expr::Value(_)) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (ast::Expr::CompoundIdentifier(columns), ast::Expr::Value(_)) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        (ast::Expr::Value(_), ast::Expr::Identifier(column)) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (ast::Expr::Value(_), ast::Expr::CompoundIdentifier(columns)) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        _ => None,
    };
    if column.is_some_and(|slot| (start..end).contains(&slot)) {
        filters.push(expr);
    }
}

fn evaluate_join_condition(
    state: &DatabaseState,
    operator: &ast::JoinOperator,
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &StatementExecutionContext,
) -> Result<bool> {
    let constraint = match operator {
        ast::JoinOperator::Join(constraint)
        | ast::JoinOperator::Inner(constraint)
        | ast::JoinOperator::CrossJoin(constraint)
        | ast::JoinOperator::Left(constraint)
        | ast::JoinOperator::LeftOuter(constraint)
        | ast::JoinOperator::Right(constraint)
        | ast::JoinOperator::RightOuter(constraint)
        | ast::JoinOperator::FullOuter(constraint) => constraint,
        _ => {
            return reject_unsupported("join type is not implemented");
        }
    };
    match constraint {
        ast::JoinConstraint::None => Ok(matches!(operator, ast::JoinOperator::CrossJoin(_))),
        ast::JoinConstraint::On(expression) => Ok(matches!(
            evaluate_query_expression(state, expression, scope, row, xid, snapshot, context,)?,
            Value::Bool(true)
        )),
        ast::JoinConstraint::Using(names) => evaluate_using_join_condition(
            names
                .iter()
                .map(normalize_unqualified_object_name)
                .collect::<Result<Vec<_>>>()?
                .as_slice(),
            row,
            scope,
            left_start,
            right_start,
        ),
        ast::JoinConstraint::Natural => {
            let names = scope.columns[left_start..right_start]
                .iter()
                .filter(|left| {
                    scope.columns[right_start..]
                        .iter()
                        .any(|right| right.name == left.name)
                })
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            evaluate_using_join_condition(&names, row, scope, left_start, right_start)
        }
    }
}

fn evaluate_using_join_condition(
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
        let data_type = coercion::resolve_common_type(left.data_type.base, right.data_type.base)
            .expect("bound USING columns must have a common type");
        let left = coercion::coerce(
            row[left.slot].clone(),
            left.data_type.base,
            PgType::create(data_type),
            CastContext::Implicit,
        )?;
        let right = coercion::coerce(
            row[right.slot].clone(),
            right.data_type.base,
            PgType::create(data_type),
            CastContext::Implicit,
        )?;
        if left.is_null() || right.is_null() || left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn evaluate_row_count(
    expr: &ast::Expr,
    clause: RowCountClause,
    context: &StatementExecutionContext,
) -> Result<Option<usize>> {
    if matches!(clause, RowCountClause::Limit)
        && matches!(expr, ast::Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("all"))
    {
        return Ok(None);
    }
    let schema = create_constant_expression_schema();
    let value = evaluate_and_coerce(
        expr,
        BaseType::Int8,
        CastContext::Implicit,
        RowScope::Table(&schema),
        &[],
        context,
    )
    .map_err(|error| {
        if error.sqlstate == SqlState::CannotCoerce {
            PgError::create(
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
        Value::Int8(_) => Err(PgError::create(
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
