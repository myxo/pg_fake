use super::{DatabaseState, normalize_identifier, normalize_unqualified_object_name};
use crate::{
    catalog::{Catalog, TableId, TableSchema},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType},
};
use ast::VisitMut as _;
use sqlparser::ast;
use std::ops::ControlFlow;

#[derive(Clone)]
pub(super) struct BoundColumn {
    pub(super) name: String,
    pub(super) data_type: PgType,
    pub(super) qualifier: String,
    pub(super) slot: usize,
    pub(super) merged: Option<(usize, usize)>,
    pub(super) unqualified: bool,
    pub(super) wildcard: bool,
    pub(super) depth: usize,
    pub(super) table_id: Option<TableId>,
    pub(super) source_name: String,
}

#[derive(Clone)]
pub(crate) struct BoundScope {
    pub(super) columns: Vec<BoundColumn>,
}

#[derive(Clone, Copy)]
pub(crate) enum RowScope<'a> {
    Table(&'a TableSchema),
    Bound(&'a BoundScope),
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn matches_identifier(name: &str, identifier: &ast::Ident) -> bool {
    if identifier.quote_style.is_some() {
        name == identifier.value
    } else {
        name.len() == identifier.value.len()
            && name
                .bytes()
                .zip(identifier.value.bytes())
                .all(|(name, identifier)| name == identifier.to_ascii_lowercase())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_bound_column<'a>(
    scope: &'a BoundScope,
    identifiers: &[ast::Ident],
) -> Result<&'a BoundColumn> {
    let mut selected = None;
    let mut ambiguous = false;
    match identifiers {
        [identifier] => {
            for column in &scope.columns {
                if !column.unqualified || !matches_identifier(&column.name, identifier) {
                    continue;
                }
                match selected {
                    None => selected = Some(column),
                    Some(current) if column.depth < current.depth => {
                        selected = Some(column);
                        ambiguous = false;
                    }
                    Some(current) if column.depth == current.depth => ambiguous = true,
                    Some(_) => {}
                }
            }
        }
        [qualifier, identifier] => {
            let depth = scope
                .columns
                .iter()
                .filter(|column| matches_identifier(&column.qualifier, qualifier))
                .map(|column| column.depth)
                .min()
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::UndefinedTable,
                        format!(
                            "missing FROM-clause entry for table {:?}",
                            normalize_identifier(qualifier)
                        ),
                    )
                })?;
            for column in &scope.columns {
                if column.depth != depth
                    || !matches_identifier(&column.qualifier, qualifier)
                    || !matches_identifier(&column.name, identifier)
                {
                    continue;
                }
                if selected.is_some() {
                    ambiguous = true;
                } else {
                    selected = Some(column);
                }
            }
        }
        _ => {}
    }
    if ambiguous {
        return Err(PgError::create(
            SqlState::AmbiguousColumn,
            format!("column {:?} is ambiguous", identifiers),
        ));
    }
    selected.ok_or_else(|| {
        PgError::create(
            SqlState::UndefinedColumn,
            format!("column {:?} does not exist", identifiers),
        )
    })
}

impl RowScope<'_> {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn resolve_column(self, identifiers: &[ast::Ident]) -> Result<(usize, PgType)> {
        match self {
            RowScope::Table(schema) => {
                if identifiers.len() != 1 {
                    return Err(PgError::create(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", identifiers),
                    ));
                }
                let index = schema
                    .columns
                    .iter()
                    .position(|column| matches_identifier(&column.name, &identifiers[0]))
                    .ok_or_else(|| {
                        PgError::create(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", identifiers[0].value),
                        )
                    })?;
                Ok((index, schema.columns[index].data_type))
            }
            RowScope::Bound(scope) => {
                let column = resolve_bound_column(scope, identifiers)?;
                Ok((column.slot, column.data_type))
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn resolve_column_value(
        self,
        identifiers: &[ast::Ident],
        row: &[crate::value::Value],
    ) -> Result<crate::value::Value> {
        match self {
            RowScope::Table(_) => Ok(row[self.resolve_column(identifiers)?.0].clone()),
            RowScope::Bound(scope) => {
                let column = resolve_bound_column(scope, identifiers)?;
                if identifiers.len() == 1
                    && let Some((left, right)) = column.merged
                {
                    let value = if row[left].is_null() {
                        row[right].clone()
                    } else {
                        row[left].clone()
                    };
                    if value.is_null() {
                        return Ok(value);
                    }
                    return crate::coercion::coerce(
                        value.clone(),
                        value
                            .get_base_type()
                            .expect("non-null value has a base type"),
                        column.data_type,
                        crate::coercion::CastContext::Implicit,
                    );
                }
                Ok(row[column.slot].clone())
            }
        }
    }
}

impl BoundScope {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(super) fn resolve_column(&self, identifiers: &[ast::Ident]) -> Result<(usize, PgType)> {
        RowScope::Bound(self).resolve_column(identifiers)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn bind_table(
        schema: &TableSchema,
        alias: Option<&ast::TableAlias>,
        slot: usize,
    ) -> Result<Self> {
        if alias.is_some_and(|alias| alias.columns.len() > schema.columns.len()) {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "table has fewer columns than specified in the column alias list",
            ));
        }
        let mut scope = bind_target_scope(schema, alias.map(|alias| &alias.name));
        for (index, column) in scope.columns.iter_mut().enumerate() {
            column.slot += slot;
            if let Some(alias) = alias.and_then(|alias| alias.columns.get(index)) {
                column.name = normalize_identifier(&alias.name);
            }
        }
        Ok(scope)
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_target_scope(schema: &TableSchema, alias: Option<&ast::Ident>) -> BoundScope {
    let qualifier = alias
        .map(normalize_identifier)
        .unwrap_or_else(|| schema.name.clone());
    BoundScope {
        columns: schema
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| BoundColumn {
                name: column.name.clone(),
                data_type: column.data_type,
                qualifier: qualifier.clone(),
                slot: index,
                merged: None,
                unqualified: true,
                wildcard: true,
                depth: 0,
                table_id: Some(schema.id),
                source_name: column.name.clone(),
            })
            .collect(),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_query_scope(catalog: &Catalog, select: &ast::Select) -> Result<BoundScope> {
    bind_from_scope(catalog, &select.from)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_from_scope(
    catalog: &Catalog,
    from: &[ast::TableWithJoins],
) -> Result<BoundScope> {
    let mut scope = BoundScope {
        columns: Vec::new(),
    };
    for source in from {
        bind_table_with_joins(catalog, source, &mut scope)?;
    }
    Ok(scope)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn combine_bound_scopes(mut target: BoundScope, mut source: BoundScope) -> BoundScope {
    let start = target.columns.len();
    for column in &mut source.columns {
        column.slot += start;
    }
    target.columns.extend(source.columns);
    target
}

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
            } => super::extract_unknown_string_literal(expression).is_some(),
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
pub(crate) fn bind_query_scope_with_outer(
    catalog: &Catalog,
    select: &ast::Select,
    outer: &BoundScope,
) -> Result<BoundScope> {
    let mut scope = bind_query_scope(catalog, select)?;
    let start = scope.columns.len();
    scope.columns.extend(outer.columns.iter().map(|column| {
        let mut column = column.clone();
        column.slot += start;
        column.depth += 1;
        column.unqualified = true;
        column.wildcard = false;
        column
    }));
    Ok(scope)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_table_with_joins(
    catalog: &Catalog,
    table: &ast::TableWithJoins,
    scope: &mut BoundScope,
) -> Result<()> {
    let left_start = scope.columns.len();
    bind_table_factor(catalog, &table.relation, scope)?;
    for join in &table.joins {
        let right_start = scope.columns.len();
        bind_table_factor(catalog, &join.relation, scope)?;
        let constraint = match &join.join_operator {
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
        bind_join_constraint(catalog, scope, join, constraint, left_start, right_start)?;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_table_factor(
    catalog: &Catalog,
    factor: &ast::TableFactor,
    scope: &mut BoundScope,
) -> Result<()> {
    if let ast::TableFactor::NestedJoin {
        table_with_joins,
        alias,
    } = factor
    {
        let start = scope.columns.len();
        bind_table_with_joins(catalog, table_with_joins, scope)?;
        if let Some(alias) = alias {
            let output_count = scope.columns[start..]
                .iter()
                .filter(|column| column.wildcard)
                .count();
            if alias.columns.len() > output_count {
                return Err(PgError::create(
                    SqlState::InvalidColumnReference,
                    "join has fewer columns than specified in the column alias list",
                ));
            }
            let qualifier = normalize_identifier(&alias.name);
            let mut output_index = 0;
            for column in &mut scope.columns[start..] {
                if column.wildcard {
                    if let Some(alias) = alias.columns.get(output_index) {
                        column.name = normalize_identifier(&alias.name);
                    }
                    column.qualifier = qualifier.clone();
                    output_index += 1;
                } else {
                    column.qualifier.clear();
                }
            }
        }
        return Ok(());
    }
    if let ast::TableFactor::Derived {
        lateral,
        subquery,
        alias,
        ..
    } = factor
    {
        if *lateral {
            return reject_unsupported("LATERAL derived tables are not implemented");
        }
        let alias = alias.as_ref().ok_or_else(|| {
            PgError::create(SqlState::SyntaxError, "subquery in FROM must have an alias")
        })?;
        let columns = describe_bound_query_columns(catalog, subquery, None)?;
        if alias.columns.len() > columns.len() {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "derived table has fewer columns than specified in the column alias list",
            ));
        }
        let qualifier = normalize_identifier(&alias.name);
        let start = scope.columns.len();
        scope
            .columns
            .extend(columns.into_iter().enumerate().map(|(index, column)| {
                let source_name = column.name.clone();
                BoundColumn {
                    name: alias
                        .columns
                        .get(index)
                        .map(|alias| normalize_identifier(&alias.name))
                        .unwrap_or(column.name),
                    data_type: column.data_type,
                    qualifier: qualifier.clone(),
                    slot: start + index,
                    merged: None,
                    unqualified: true,
                    wildcard: true,
                    depth: 0,
                    table_id: None,
                    source_name,
                }
            }));
        return Ok(());
    }
    let ast::TableFactor::Table {
        name: table_name,
        alias,
        args,
        ..
    } = factor
    else {
        return reject_unsupported("FROM source is not implemented");
    };
    if args.is_some() {
        return reject_unsupported("table functions are not implemented");
    }
    let table = BoundScope::bind_table(
        catalog.require_table(&normalize_unqualified_object_name(table_name)?)?,
        alias.as_ref(),
        scope.columns.len(),
    )?;
    scope.columns.extend(table.columns);
    Ok(())
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
fn describe_bound_query_columns(
    catalog: &Catalog,
    query: &ast::Query,
    outer: Option<&BoundScope>,
) -> Result<Vec<BoundColumn>> {
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
                        .columns
                        .iter()
                        .filter(|column| column.wildcard)
                        .map(|column| Ok((column.name.clone(), column.data_type)))
                        .collect::<Vec<_>>(),
                    ast::SelectItem::QualifiedWildcard(
                        ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                        _,
                    ) => {
                        let qualifier = match super::normalize_unqualified_object_name(name) {
                            Ok(qualifier) => qualifier,
                            Err(error) => return vec![Err(error)],
                        };
                        let columns = scope
                            .columns
                            .iter()
                            .filter(|column| column.qualifier == qualifier && column.wildcard)
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
                    ast::SelectItem::UnnamedExpr(expr) => vec![
                        infer_expression_data_type(catalog, expr, &scope)
                            .map(|data_type| ("?column?".into(), data_type)),
                    ],
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
                            !super::is_null_literal(expr)
                                && super::extract_unknown_string_literal(expr).is_none()
                        })
                        .try_fold(None, |common, expr| {
                            let data_type = super::infer_expression_type(
                                expr,
                                RowScope::Table(&super::create_constant_expression_schema()),
                            )?;
                            Ok(Some(match common {
                                Some(common) => {
                                    crate::coercion::resolve_common_type(common, data_type)
                                        .ok_or_else(|| {
                                            PgError::create(
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
                        data_type: PgType::create(data_type),
                        qualifier: String::new(),
                        slot,
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
        ast::SetExpr::SetOperation { left, right, .. } => {
            let left = describe_bound_set_expression_columns(catalog, left, outer)?;
            let right = describe_bound_set_expression_columns(catalog, right, outer)?;
            if left.len() != right.len() {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "each set-operation query must have the same number of columns",
                ));
            }
            left.into_iter()
                .zip(right)
                .map(|(mut left, right)| {
                    left.data_type = PgType::create(
                        crate::coercion::resolve_common_type(
                            left.data_type.base,
                            right.data_type.base,
                        )
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::DatatypeMismatch,
                                "set-operation types cannot be matched",
                            )
                        })?,
                    );
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn infer_expression_data_type(
    catalog: &Catalog,
    expr: &ast::Expr,
    scope: &BoundScope,
) -> Result<PgType> {
    if matches!(expr, ast::Expr::Value(value) if matches!(value.value, ast::Value::Placeholder(_)))
    {
        return Ok(PgType::create(BaseType::Text));
    }
    if let ast::Expr::Subquery(query) = expr {
        let columns = describe_bound_query_columns(catalog, query, Some(scope))?;
        if columns.len() != 1 {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "subquery must return only one column",
            ));
        }
        return Ok(columns[0].data_type);
    }
    let mut expression = expr.clone();
    let mut describer = TypedSubquerySubstituter {
        catalog,
        outer: scope,
        error: None,
    };
    let _ = expression.visit(&mut describer);
    if let Some(error) = describer.error {
        return Err(error);
    }
    Ok(PgType::create(super::infer_expression_type(
        &expression,
        RowScope::Bound(scope),
    )?))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn substitute_typed_subqueries(
    catalog: &Catalog,
    expression: &ast::Expr,
    outer: &BoundScope,
) -> Result<ast::Expr> {
    let mut expression = expression.clone();
    let mut describer = TypedSubquerySubstituter {
        catalog,
        outer,
        error: None,
    };
    let _ = expression.visit(&mut describer);
    describer.error.map_or(Ok(expression), Err)
}

struct TypedSubquerySubstituter<'a> {
    catalog: &'a Catalog,
    outer: &'a BoundScope,
    error: Option<PgError>,
}

impl ast::VisitorMut for TypedSubquerySubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> ControlFlow<Self::Break> {
        if self.error.is_some() {
            return ControlFlow::Break(());
        }
        let result = match expression {
            ast::Expr::Subquery(query) => {
                describe_bound_query_columns(self.catalog, query, Some(self.outer)).and_then(
                    |columns| {
                        if columns.len() != 1 {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "subquery must return only one column",
                            ));
                        }
                        Ok(crate::analyzer::create_typed_literal(
                            crate::value::Value::Null,
                            columns[0].data_type,
                        ))
                    },
                )
            }
            ast::Expr::Exists { .. } => Ok(crate::analyzer::create_typed_literal(
                crate::value::Value::Bool(false),
                PgType::create(BaseType::Bool),
            )),
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => describe_bound_query_columns(self.catalog, subquery, Some(self.outer)).and_then(
                |columns| {
                    let left_width = match expr.as_ref() {
                        ast::Expr::Tuple(fields) => fields.len(),
                        _ => 1,
                    };
                    if columns.len() != left_width {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "subquery has too many columns",
                        ));
                    }
                    let mut fields = columns.into_iter().map(|column| {
                        crate::analyzer::create_typed_literal(
                            crate::value::Value::Null,
                            column.data_type,
                        )
                    });
                    let candidate = if left_width == 1 {
                        fields.next().expect("subquery has one column")
                    } else {
                        ast::Expr::Tuple(fields.collect())
                    };
                    Ok(ast::Expr::InList {
                        expr: expr.clone(),
                        list: vec![candidate],
                        negated: *negated,
                    })
                },
            ),
            ast::Expr::AnyOp {
                left,
                compare_op,
                right,
                is_some,
            } if matches!(right.as_ref(), ast::Expr::Subquery(_)) => {
                let ast::Expr::Subquery(query) = right.as_ref() else {
                    unreachable!("quantified right side was checked")
                };
                describe_bound_query_columns(self.catalog, query, Some(self.outer)).and_then(
                    |columns| {
                        if columns.len() != 1 {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "subquery has too many columns",
                            ));
                        }
                        Ok(ast::Expr::AnyOp {
                            left: left.clone(),
                            compare_op: compare_op.clone(),
                            right: Box::new(ast::Expr::Tuple(vec![
                                crate::analyzer::create_typed_literal(
                                    crate::value::Value::Null,
                                    columns[0].data_type,
                                ),
                            ])),
                            is_some: *is_some,
                        })
                    },
                )
            }
            ast::Expr::AllOp {
                left,
                compare_op,
                right,
            } if matches!(right.as_ref(), ast::Expr::Subquery(_)) => {
                let ast::Expr::Subquery(query) = right.as_ref() else {
                    unreachable!("quantified right side was checked")
                };
                describe_bound_query_columns(self.catalog, query, Some(self.outer)).and_then(
                    |columns| {
                        if columns.len() != 1 {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "subquery has too many columns",
                            ));
                        }
                        Ok(ast::Expr::AllOp {
                            left: left.clone(),
                            compare_op: compare_op.clone(),
                            right: Box::new(ast::Expr::Tuple(vec![
                                crate::analyzer::create_typed_literal(
                                    crate::value::Value::Null,
                                    columns[0].data_type,
                                ),
                            ])),
                        })
                    },
                )
            }
            _ => return ControlFlow::Continue(()),
        };
        match result {
            Ok(replacement) => *expression = replacement,
            Err(error) => self.error = Some(error),
        }
        ControlFlow::Continue(())
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_join_constraint(
    catalog: &Catalog,
    scope: &mut BoundScope,
    join: &ast::Join,
    constraint: &ast::JoinConstraint,
    left_start: usize,
    right_start: usize,
) -> Result<()> {
    match constraint {
        ast::JoinConstraint::On(expression) => {
            let data_type = infer_expression_data_type(catalog, expression, scope)?;
            if data_type != PgType::create(crate::value::BaseType::Bool)
                && !super::is_null_literal(expression)
            {
                return Err(PgError::create(
                    SqlState::DatatypeMismatch,
                    "JOIN/ON clause must be type boolean",
                ));
            }
        }
        ast::JoinConstraint::Using(columns) => {
            let columns = columns
                .iter()
                .map(normalize_unqualified_object_name)
                .collect::<Result<Vec<_>>>()?;
            bind_join_columns(scope, &columns, left_start, right_start)?;
        }
        ast::JoinConstraint::Natural => {
            let columns = scope.columns[left_start..right_start]
                .iter()
                .filter(|left| {
                    scope.columns[right_start..]
                        .iter()
                        .any(|right| right.name == left.name)
                })
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            bind_join_columns(scope, &columns, left_start, right_start)?;
        }
        ast::JoinConstraint::None
            if matches!(join.join_operator, ast::JoinOperator::CrossJoin(_)) => {}
        ast::JoinConstraint::None => {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "INNER JOIN requires a join condition",
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn bind_join_columns(
    scope: &mut BoundScope,
    names: &[String],
    left_start: usize,
    right_start: usize,
) -> Result<()> {
    for name in names {
        let (left_columns, right_columns) = scope.columns.split_at_mut(right_start);
        let left = left_columns[left_start..]
            .iter_mut()
            .find(|column| column.unqualified && column.name == *name)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedColumn,
                    format!(
                        "column {name:?} specified in USING clause does not exist in left table"
                    ),
                )
            })?;
        let right = right_columns
            .iter_mut()
            .find(|column| column.unqualified && column.name == *name)
            .ok_or_else(|| {
                PgError::create(
                    SqlState::UndefinedColumn,
                    format!(
                        "column {name:?} specified in USING clause does not exist in right table"
                    ),
                )
            })?;
        let data_type =
            crate::coercion::resolve_common_type(left.data_type.base, right.data_type.base)
                .ok_or_else(|| {
                    PgError::create(
                        SqlState::DatatypeMismatch,
                        "JOIN/USING types cannot be matched",
                    )
                })?;
        left.data_type = PgType::create(data_type);
        left.merged = Some((left.slot, right.slot));
        right.unqualified = false;
        right.wildcard = false;
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn bind_select_scope(state: &DatabaseState, select: &ast::Select) -> Result<BoundScope> {
    bind_query_scope(&state.catalog, select)
}
