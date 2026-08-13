use super::*;
use sqlparser::ast::{VisitMut, VisitorMut};

struct SubqueryMaterializer<'a> {
    state: &'a DatabaseState,
    xid: Xid,
    snapshot: &'a Snapshot,
    context: &'a ExecutionContext,
    error: Option<PgError>,
    defer_unresolved: bool,
    scopes: Vec<BoundScope>,
}

impl SubqueryMaterializer<'_> {
    fn execute(&self, query: &sqlparser::ast::Query) -> Result<QueryResult> {
        let query = materialize_scalar_subqueries(
            self.state,
            &Statement::Query(Box::new(query.clone())),
            self.xid,
            self.snapshot,
            self.context,
        )?;
        let Statement::Query(query) = query else {
            unreachable!("subquery statement remains a query");
        };
        let StatementResult::Query(result) =
            select_rows(self.state, &query, self.xid, self.snapshot, self.context)?
        else {
            unreachable!("subquery execution returns query rows");
        };
        Ok(result)
    }
}

impl VisitorMut for SubqueryMaterializer<'_> {
    type Break = ();

    fn pre_visit_query(
        &mut self,
        query: &mut sqlparser::ast::Query,
    ) -> std::ops::ControlFlow<Self::Break> {
        let scope = match query.body.as_ref() {
            SetExpr::Select(select) => bind_query_scope(&self.state.catalog, select),
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

    fn post_visit_query(
        &mut self,
        _query: &mut sqlparser::ast::Query,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &mut Expr) -> std::ops::ControlFlow<Self::Break> {
        let original = expr.clone();
        let correlation_candidate = original.clone();
        let result = (|| match original {
            Expr::AnyOp {
                left,
                compare_op,
                right,
                is_some,
            } => {
                let Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::new(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::with_typmod(
                    BaseType::from_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(Expr::AnyOp {
                    left,
                    compare_op,
                    right: Box::new(Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| crate::analyzer::typed_literal(row[0].clone(), data_type))
                            .collect(),
                    )),
                    is_some,
                }))
            }
            Expr::AllOp {
                left,
                compare_op,
                right,
            } => {
                let Expr::Subquery(subquery) = right.as_ref() else {
                    return Ok(None);
                };
                let result = self.execute(subquery)?;
                if result.columns.len() != 1 {
                    return Err(PgError::new(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let data_type = PgType::with_typmod(
                    BaseType::from_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(Expr::AllOp {
                    left,
                    compare_op,
                    right: Box::new(Expr::Tuple(
                        result
                            .rows
                            .into_iter()
                            .map(|row| crate::analyzer::typed_literal(row[0].clone(), data_type))
                            .collect(),
                    )),
                }))
            }
            Expr::Subquery(query) => {
                let result = self.execute(&query)?;
                if result.columns.len() != 1 {
                    return Err(PgError::new(
                        SqlState::SyntaxError,
                        "subquery must return only one column",
                    ));
                }
                if result.rows.len() > 1 {
                    return Err(PgError::new(
                        SqlState::CardinalityViolation,
                        "more than one row returned by a subquery used as an expression",
                    ));
                }
                let data_type = PgType::with_typmod(
                    BaseType::from_oid(result.columns[0].type_oid)
                        .expect("query result type OID is supported"),
                    result.columns[0].typmod,
                );
                Ok(Some(crate::analyzer::typed_literal(
                    result
                        .rows
                        .into_iter()
                        .next()
                        .map(|row| row[0].clone())
                        .unwrap_or(Value::Null),
                    data_type,
                )))
            }
            Expr::Exists { subquery, negated } => Ok(Some(crate::analyzer::typed_literal(
                Value::Bool(self.execute(&subquery)?.rows.is_empty() == negated),
                PgType::new(BaseType::Bool),
            ))),
            Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let result = self.execute(&subquery)?;
                let left_width = match expr.as_ref() {
                    Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if result.columns.len() != left_width {
                    return Err(PgError::new(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let types = result
                    .columns
                    .iter()
                    .map(|column| {
                        PgType::with_typmod(
                            BaseType::from_oid(column.type_oid)
                                .expect("query result type OID is supported"),
                            column.typmod,
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(Some(Expr::InList {
                    expr,
                    list: result
                        .rows
                        .into_iter()
                        .map(|row| {
                            let fields = row
                                .into_iter()
                                .zip(&types)
                                .map(|(value, data_type)| {
                                    crate::analyzer::typed_literal(value, *data_type)
                                })
                                .collect::<Vec<_>>();
                            if fields.len() == 1 {
                                fields.into_iter().next().expect("row has one field")
                            } else {
                                Expr::Tuple(fields)
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
                    expression_references_outer(&self.state.catalog, &correlation_candidate, outer)
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

pub(crate) fn materialize_scalar_subqueries(
    state: &DatabaseState,
    statement: &Statement,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
) -> Result<Statement> {
    let mut statement = statement.clone();
    materialize_subqueries(state, &mut statement, xid, snapshot, context, true)?;
    Ok(statement)
}

fn materialize_subqueries<V: VisitMut>(
    state: &DatabaseState,
    value: &mut V,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
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

impl VisitorMut for OuterReferenceSubstituter<'_> {
    type Break = ();

    fn pre_visit_query(
        &mut self,
        query: &mut sqlparser::ast::Query,
    ) -> std::ops::ControlFlow<Self::Break> {
        let SetExpr::Select(select) = query.body.as_ref() else {
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

    fn post_visit_query(
        &mut self,
        _query: &mut sqlparser::ast::Query,
    ) -> std::ops::ControlFlow<Self::Break> {
        self.scopes.pop().expect("visited query pushed a scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut Expr) -> std::ops::ControlFlow<Self::Break> {
        let identifiers = match expression {
            Expr::Identifier(identifier) => std::slice::from_ref(identifier),
            Expr::CompoundIdentifier(identifiers) => identifiers.as_slice(),
            _ => return std::ops::ControlFlow::Continue(()),
        };
        for scope in self.scopes.iter().rev() {
            if identifiers.len() == 2 {
                let qualifier = identifier_name(&identifiers[0]);
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
                match RowScope::Bound(self.outer_scope).column_value(identifiers, self.outer_row) {
                    Ok(value) => {
                        *expression = crate::analyzer::typed_literal(value, data_type);
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

fn expression_references_outer(
    catalog: &Catalog,
    expression: &Expr,
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
    expression: &Expr,
    scope: &BoundScope,
    row: &[Value],
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
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

pub(crate) fn query_columns(
    state: &DatabaseState,
    statement: &Statement,
) -> Result<Vec<ColumnMeta>> {
    let Statement::Query(query) = statement else {
        return Ok(Vec::new());
    };
    match query.body.as_ref() {
        SetExpr::Select(select) => bind_select_scope(state, select).and_then(|scope| {
            projections_and_columns(state, &select.projection, &scope).map(|(_, columns)| columns)
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
    Merged(usize, usize, PgType),
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
                merged: None,
                unqualified: true,
                wildcard: true,
                depth: 0,
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
        let base = expression_data_type(state, selection, &scope)?.base;
        if base != BaseType::Bool && !null_expression(selection) {
            return Err(PgError::new(
                SqlState::DatatypeMismatch,
                "WHERE requires a boolean expression",
            ));
        }
    }
    let (projections, columns) = projections_and_columns(state, &select.projection, &scope)?;
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
                        expression_data_type(state, &order.expr, &scope)?;
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
    let mut rows = Vec::new();
    visit_source_rows(
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
                    Projection::Column(index) => Ok(row[*index].clone()),
                    Projection::Merged(left, right, data_type) => {
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
                                value.base_type().expect("non-null value has a base type"),
                                *data_type,
                                CastContext::Implicit,
                            )
                        }
                    }
                    Projection::Expression(expr) => {
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
    }
    let rows = rows
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(|row| row.values)
        .collect();
    Ok(StatementResult::Query(QueryResult { columns, rows }))
}

fn projections_and_columns<'a>(
    state: &DatabaseState,
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
                        projections.push(match column.merged {
                            Some((left, right)) => {
                                Projection::Merged(left, right, column.data_type)
                            }
                            None => Projection::Column(column.slot),
                        });
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
            SelectItem::UnnamedExpr(expression @ Expr::Identifier(column)) => {
                let (_, data_type) = scope.resolve_column(std::slice::from_ref(column))?;
                projections.push(Projection::Expression(expression));
                columns.push(ColumnMeta {
                    name: column.value.clone(),
                    type_oid: data_type.oid(),
                    typmod: data_type.typmod,
                });
            }
            SelectItem::UnnamedExpr(expression @ Expr::CompoundIdentifier(identifiers)) => {
                let (_, data_type) = scope.resolve_column(identifiers)?;
                projections.push(Projection::Expression(expression));
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
                let data_type = expression_data_type(state, expr, scope)?;
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
                    Some((_, data_type)) => {
                        (Projection::Expression(expr), data_type, data_type.typmod)
                    }
                    None => {
                        let data_type = expression_data_type(state, expr, scope)?;
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

fn expression_data_type(state: &DatabaseState, expr: &Expr, scope: &BoundScope) -> Result<PgType> {
    super::scope::expression_data_type(&state.catalog, expr, scope)
}

fn source_rows(
    state: &DatabaseState,
    select: &sqlparser::ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
) -> Result<Vec<Vec<Value>>> {
    if select.from.is_empty() {
        return Ok(vec![Vec::new()]);
    }
    let mut next_slot = 0;
    let mut rows = vec![vec![Value::Null; scope.columns.len()]];
    for table in &select.from {
        let source = table_rows(
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

fn visit_source_rows(
    state: &DatabaseState,
    select: &sqlparser::ast::Select,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    if let [table] = select.from.as_slice()
        && streams_inner_rows(table)
    {
        return visit_inner_rows(
            state, table, scope, xid, snapshot, context, selection, visit,
        );
    }
    for row in source_rows(state, select, scope, xid, snapshot, context, selection)? {
        visit(&row)?;
    }
    Ok(())
}

fn streams_inner_rows(table: &sqlparser::ast::TableWithJoins) -> bool {
    matches!(table.relation, TableFactor::Table { .. })
        && table.joins.iter().all(|join| {
            matches!(join.relation, TableFactor::Table { .. })
                && matches!(
                    join.join_operator,
                    sqlparser::ast::JoinOperator::Join(_)
                        | sqlparser::ast::JoinOperator::Inner(_)
                        | sqlparser::ast::JoinOperator::CrossJoin(_)
                )
        })
}

fn visit_inner_rows(
    state: &DatabaseState,
    table: &sqlparser::ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut starts = Vec::with_capacity(table.joins.len() + 1);
    let mut next_slot = 0;
    for factor in
        std::iter::once(&table.relation).chain(table.joins.iter().map(|join| &join.relation))
    {
        let TableFactor::Table {
            name: table_name, ..
        } = factor
        else {
            unreachable!("streamable sources are tables");
        };
        starts.push(next_slot);
        next_slot += state.catalog.table(&name(table_name)?)?.columns.len();
    }
    if table.joins.len() == 1
        && let Some((left_slot, right_slot)) =
            hash_join_slots(&table.joins[0].join_operator, scope, starts[0], starts[1])
    {
        return visit_hash_inner_rows(
            state, table, scope, xid, snapshot, context, selection, starts[0], starts[1],
            left_slot, right_slot, visit,
        );
    }
    visit_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        starts[0],
        &mut |row| {
            visit_inner_join_rows(
                state, table, scope, xid, snapshot, context, selection, &starts, 0, row, visit,
            )
        },
    )
}

fn hash_join_slots(
    operator: &sqlparser::ast::JoinOperator,
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
) -> Option<(usize, usize)> {
    let (sqlparser::ast::JoinOperator::Join(sqlparser::ast::JoinConstraint::On(expression))
    | sqlparser::ast::JoinOperator::Inner(sqlparser::ast::JoinConstraint::On(expression))) =
        operator
    else {
        return None;
    };
    let Expr::BinaryOp {
        left,
        op: BinaryOperator::Eq,
        right,
    } = expression
    else {
        return None;
    };
    let (left_slot, left_type) = hash_expression_slot(left, scope)?;
    let (right_slot, right_type) = hash_expression_slot(right, scope)?;
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

fn hash_expression_slot(expression: &Expr, scope: &BoundScope) -> Option<(usize, PgType)> {
    match expression {
        Expr::Identifier(identifier) => scope.resolve_column(std::slice::from_ref(identifier)).ok(),
        Expr::CompoundIdentifier(identifiers) => scope.resolve_column(identifiers).ok(),
        _ => None,
    }
}

fn visit_hash_inner_rows(
    state: &DatabaseState,
    table: &sqlparser::ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
    left_start: usize,
    right_start: usize,
    left_slot: usize,
    right_slot: usize,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let mut right_rows = std::collections::HashMap::<JoinKey, Vec<Vec<Value>>>::new();
    visit_factor_rows(
        state,
        &table.joins[0].relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        right_start,
        &mut |row| {
            if let Some(key) = hash_key(&row[right_slot]) {
                right_rows.entry(key).or_default().push(row.to_vec());
            }
            Ok(())
        },
    )?;
    visit_factor_rows(
        state,
        &table.relation,
        scope,
        xid,
        snapshot,
        context,
        selection,
        left_start,
        &mut |left| {
            let Some(key) = hash_key(&left[left_slot]) else {
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

fn hash_key(value: &Value) -> Option<JoinKey> {
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

fn visit_inner_join_rows(
    state: &DatabaseState,
    table: &sqlparser::ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
    starts: &[usize],
    index: usize,
    left: &[Value],
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let Some(join) = table.joins.get(index) else {
        return visit(left);
    };
    visit_factor_rows(
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
            if join_matches(
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
                visit_inner_join_rows(
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

fn visit_factor_rows(
    state: &DatabaseState,
    factor: &TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
    start: usize,
    visit: &mut dyn FnMut(&[Value]) -> Result<()>,
) -> Result<()> {
    let TableFactor::Table {
        name: table_name,
        args,
        ..
    } = factor
    else {
        unreachable!("streamable source is a table");
    };
    if args.is_some() {
        return Err(PgError::new(
            SqlState::FeatureNotSupported,
            "table functions are not implemented",
        ));
    }
    let schema = state.catalog.table(&name(table_name)?)?;
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        push_filters(
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
        .rows()
    {
        let Some(version) = visible_version(chain, snapshot, xid, &state.transactions) else {
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

fn table_rows(
    state: &DatabaseState,
    table: &sqlparser::ast::TableWithJoins,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
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
        selection,
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
                if join_matches(
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
                    sqlparser::ast::JoinOperator::Left(_)
                        | sqlparser::ast::JoinOperator::LeftOuter(_)
                        | sqlparser::ast::JoinOperator::FullOuter(_)
                )
            {
                joined.push(left.clone());
            }
        }
        if matches!(
            join.join_operator,
            sqlparser::ast::JoinOperator::Right(_)
                | sqlparser::ast::JoinOperator::RightOuter(_)
                | sqlparser::ast::JoinOperator::FullOuter(_)
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

fn factor_rows(
    state: &DatabaseState,
    factor: &TableFactor,
    scope: &BoundScope,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
    selection: Option<&Expr>,
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
            selection,
            next_slot,
        );
    }
    if let TableFactor::Derived {
        lateral,
        subquery,
        alias: Some(_),
        ..
    } = factor
    {
        if *lateral {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "LATERAL derived tables are not implemented",
            ));
        }
        let StatementResult::Query(result) = select_rows(state, subquery, xid, snapshot, context)?
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
    let mut filters = Vec::new();
    if let Some(selection) = selection {
        push_filters(
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
        .rows()
        .filter_map(|(_, chain)| visible_version(chain, snapshot, xid, &state.transactions))
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

fn push_filters<'a>(
    expr: &'a Expr,
    scope: &BoundScope,
    start: usize,
    end: usize,
    filters: &mut Vec<&'a Expr>,
) {
    if let Expr::BinaryOp {
        left,
        op: BinaryOperator::And,
        right,
    } = expr
    {
        push_filters(left, scope, start, end, filters);
        push_filters(right, scope, start, end, filters);
        return;
    }
    let Expr::BinaryOp { left, right, .. } = expr else {
        return;
    };
    let column = match (left.as_ref(), right.as_ref()) {
        (Expr::Identifier(column), Expr::Value(_)) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (Expr::CompoundIdentifier(columns), Expr::Value(_)) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        (Expr::Value(_), Expr::Identifier(column)) => scope
            .resolve_column(std::slice::from_ref(column))
            .ok()
            .map(|(slot, _)| slot),
        (Expr::Value(_), Expr::CompoundIdentifier(columns)) => {
            scope.resolve_column(columns).ok().map(|(slot, _)| slot)
        }
        _ => None,
    };
    if column.is_some_and(|slot| (start..end).contains(&slot)) {
        filters.push(expr);
    }
}

fn join_matches(
    state: &DatabaseState,
    operator: &sqlparser::ast::JoinOperator,
    row: &[Value],
    scope: &BoundScope,
    left_start: usize,
    right_start: usize,
    xid: Xid,
    snapshot: &Snapshot,
    context: &ExecutionContext,
) -> Result<bool> {
    let constraint = match operator {
        sqlparser::ast::JoinOperator::Join(constraint)
        | sqlparser::ast::JoinOperator::Inner(constraint)
        | sqlparser::ast::JoinOperator::CrossJoin(constraint)
        | sqlparser::ast::JoinOperator::Left(constraint)
        | sqlparser::ast::JoinOperator::LeftOuter(constraint)
        | sqlparser::ast::JoinOperator::Right(constraint)
        | sqlparser::ast::JoinOperator::RightOuter(constraint)
        | sqlparser::ast::JoinOperator::FullOuter(constraint) => constraint,
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
            evaluate_query_expression(state, expression, scope, row, xid, snapshot, context,)?,
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
