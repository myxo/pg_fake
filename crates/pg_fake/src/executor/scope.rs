use super::{DatabaseState, identifier_name, name};
use crate::{
    catalog::{Catalog, TableSchema},
    error::{PgError, Result, SqlState},
    value::{BaseType, PgType},
};
use sqlparser::ast::{
    Expr, Ident, Join, JoinConstraint, JoinOperator, SelectItem, SetExpr, TableFactor,
    TableWithJoins,
};

#[derive(Clone)]
pub(super) struct BoundColumn {
    pub(super) name: String,
    pub(super) data_type: PgType,
    pub(super) qualifier: String,
    pub(super) slot: usize,
    pub(super) merged: Option<(usize, usize)>,
    pub(super) unqualified: bool,
    pub(super) wildcard: bool,
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

impl RowScope<'_> {
    pub(super) fn resolve_column(self, identifiers: &[Ident]) -> Result<(usize, PgType)> {
        match self {
            RowScope::Table(schema) => {
                if identifiers.len() != 1 {
                    return Err(PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", identifiers),
                    ));
                }
                let index = schema
                    .columns
                    .iter()
                    .position(|column| column.name == identifier_name(&identifiers[0]))
                    .ok_or_else(|| {
                        PgError::new(
                            SqlState::UndefinedColumn,
                            format!("column {:?} does not exist", identifiers[0].value),
                        )
                    })?;
                Ok((index, schema.columns[index].data_type))
            }
            RowScope::Bound(scope) => {
                let names = identifiers.iter().map(identifier_name).collect::<Vec<_>>();
                let matches = match names.as_slice() {
                    [column] => scope
                        .columns
                        .iter()
                        .filter(|bound| bound.unqualified && bound.name == *column)
                        .collect::<Vec<_>>(),
                    [qualifier, column] => scope
                        .columns
                        .iter()
                        .filter(|bound| bound.qualifier == *qualifier && bound.name == *column)
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                match matches.as_slice() {
                    [] if names.len() == 2
                        && !scope
                            .columns
                            .iter()
                            .any(|column| column.qualifier == names[0]) =>
                    {
                        Err(PgError::new(
                            SqlState::UndefinedTable,
                            format!("missing FROM-clause entry for table {:?}", names[0]),
                        ))
                    }
                    [] => Err(PgError::new(
                        SqlState::UndefinedColumn,
                        format!("column {:?} does not exist", identifiers),
                    )),
                    [column] => Ok((column.slot, column.data_type)),
                    _ => Err(PgError::new(
                        SqlState::AmbiguousColumn,
                        format!("column {:?} is ambiguous", identifiers),
                    )),
                }
            }
        }
    }

    pub(super) fn column_value(
        self,
        identifiers: &[Ident],
        row: &[crate::value::Value],
    ) -> Result<crate::value::Value> {
        match self {
            RowScope::Table(_) => Ok(row[self.resolve_column(identifiers)?.0].clone()),
            RowScope::Bound(scope) => {
                let names = identifiers.iter().map(identifier_name).collect::<Vec<_>>();
                let (_, data_type) = self.resolve_column(identifiers)?;
                let column = scope
                    .columns
                    .iter()
                    .find(|column| match names.as_slice() {
                        [name] => column.unqualified && column.name == *name,
                        [qualifier, name] => column.qualifier == *qualifier && column.name == *name,
                        _ => false,
                    })
                    .expect("resolved column must be present in scope");
                if names.len() == 1
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
                        value.base_type().expect("non-null value has a base type"),
                        data_type,
                        crate::coercion::CastContext::Implicit,
                    );
                }
                Ok(row[column.slot].clone())
            }
        }
    }
}

impl BoundScope {
    pub(super) fn resolve_column(&self, identifiers: &[Ident]) -> Result<(usize, PgType)> {
        RowScope::Bound(self).resolve_column(identifiers)
    }

    fn bind_table(
        schema: &TableSchema,
        alias: Option<&sqlparser::ast::TableAlias>,
        slot: usize,
    ) -> Result<Self> {
        let qualifier = alias
            .map(|alias| identifier_name(&alias.name))
            .unwrap_or_else(|| schema.name.clone());
        if alias.is_some_and(|alias| alias.columns.len() > schema.columns.len()) {
            return Err(PgError::new(
                SqlState::InvalidColumnReference,
                "table has fewer columns than specified in the column alias list",
            ));
        }
        Ok(BoundScope {
            columns: schema
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| BoundColumn {
                    name: alias
                        .and_then(|alias| alias.columns.get(index))
                        .map(|alias| identifier_name(&alias.name))
                        .unwrap_or_else(|| column.name.clone()),
                    data_type: column.data_type,
                    qualifier: qualifier.clone(),
                    slot: slot + index,
                    merged: None,
                    unqualified: true,
                    wildcard: true,
                })
                .collect(),
        })
    }
}

pub(crate) fn bind_query_scope(
    catalog: &Catalog,
    select: &sqlparser::ast::Select,
) -> Result<BoundScope> {
    if select.from.is_empty() {
        return Ok(BoundScope {
            columns: Vec::new(),
        });
    }
    let mut scope = BoundScope {
        columns: Vec::new(),
    };
    for source in &select.from {
        bind_table_with_joins(catalog, source, &mut scope)?;
    }
    Ok(scope)
}

fn bind_table_with_joins(
    catalog: &Catalog,
    table: &TableWithJoins,
    scope: &mut BoundScope,
) -> Result<()> {
    let left_start = scope.columns.len();
    bind_table_factor(catalog, &table.relation, scope)?;
    for join in &table.joins {
        let right_start = scope.columns.len();
        bind_table_factor(catalog, &join.relation, scope)?;
        let constraint = match &join.join_operator {
            JoinOperator::Join(constraint)
            | JoinOperator::Inner(constraint)
            | JoinOperator::CrossJoin(constraint)
            | JoinOperator::Left(constraint)
            | JoinOperator::LeftOuter(constraint)
            | JoinOperator::Right(constraint)
            | JoinOperator::RightOuter(constraint)
            | JoinOperator::FullOuter(constraint) => constraint,
            _ => {
                return Err(PgError::new(
                    SqlState::FeatureNotSupported,
                    "join type is not implemented",
                ));
            }
        };
        bind_join_constraint(scope, join, constraint, left_start, right_start)?;
    }
    Ok(())
}

fn bind_table_factor(
    catalog: &Catalog,
    factor: &TableFactor,
    scope: &mut BoundScope,
) -> Result<()> {
    if let TableFactor::NestedJoin {
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
                return Err(PgError::new(
                    SqlState::InvalidColumnReference,
                    "join has fewer columns than specified in the column alias list",
                ));
            }
            let qualifier = identifier_name(&alias.name);
            let mut output_index = 0;
            for column in &mut scope.columns[start..] {
                if column.wildcard {
                    if let Some(alias) = alias.columns.get(output_index) {
                        column.name = identifier_name(&alias.name);
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
    if let TableFactor::Derived {
        lateral,
        subquery,
        alias,
        ..
    } = factor
    {
        if *lateral {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "LATERAL derived tables are not implemented",
            ));
        }
        let alias = alias.as_ref().ok_or_else(|| {
            PgError::new(SqlState::SyntaxError, "subquery in FROM must have an alias")
        })?;
        let columns = query_columns(catalog, subquery)?;
        if alias.columns.len() > columns.len() {
            return Err(PgError::new(
                SqlState::InvalidColumnReference,
                "derived table has fewer columns than specified in the column alias list",
            ));
        }
        let qualifier = identifier_name(&alias.name);
        let start = scope.columns.len();
        scope
            .columns
            .extend(columns.into_iter().enumerate().map(|(index, column)| {
                BoundColumn {
                    name: alias
                        .columns
                        .get(index)
                        .map(|alias| identifier_name(&alias.name))
                        .unwrap_or(column.name),
                    data_type: column.data_type,
                    qualifier: qualifier.clone(),
                    slot: start + index,
                    merged: None,
                    unqualified: true,
                    wildcard: true,
                }
            }));
        return Ok(());
    }
    let TableFactor::Table {
        name: table_name,
        alias,
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
    let table = BoundScope::bind_table(
        catalog.table(&name(table_name)?)?,
        alias.as_ref(),
        scope.columns.len(),
    )?;
    scope.columns.extend(table.columns);
    Ok(())
}

pub(crate) fn query_output_columns(
    catalog: &Catalog,
    query: &sqlparser::ast::Query,
) -> Result<Vec<(String, PgType)>> {
    query_columns(catalog, query).map(|columns| {
        columns
            .into_iter()
            .map(|column| (column.name, column.data_type))
            .collect()
    })
}

fn query_columns(catalog: &Catalog, query: &sqlparser::ast::Query) -> Result<Vec<BoundColumn>> {
    match query.body.as_ref() {
        SetExpr::Select(select) => {
            let scope = bind_query_scope(catalog, select)?;
            select
                .projection
                .iter()
                .flat_map(|item| match item {
                    SelectItem::Wildcard(_) => scope
                        .columns
                        .iter()
                        .filter(|column| column.wildcard)
                        .map(|column| Ok((column.name.clone(), column.data_type)))
                        .collect::<Vec<_>>(),
                    SelectItem::QualifiedWildcard(
                        sqlparser::ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                        _,
                    ) => {
                        let qualifier = match super::name(name) {
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
                            vec![Err(PgError::new(
                                SqlState::UndefinedTable,
                                format!("missing FROM-clause entry for table {qualifier:?}"),
                            ))]
                        } else {
                            columns
                        }
                    }
                    SelectItem::UnnamedExpr(Expr::Identifier(identifier)) => {
                        vec![
                            scope
                                .resolve_column(std::slice::from_ref(identifier))
                                .map(|(_, data_type)| (identifier_name(identifier), data_type)),
                        ]
                    }
                    SelectItem::UnnamedExpr(Expr::CompoundIdentifier(identifiers)) => {
                        vec![scope.resolve_column(identifiers).map(|(_, data_type)| {
                            (
                                identifier_name(
                                    identifiers
                                        .last()
                                        .expect("compound identifier is non-empty"),
                                ),
                                data_type,
                            )
                        })]
                    }
                    SelectItem::ExprWithAlias { expr, alias } => vec![
                        expression_data_type(catalog, expr, &scope)
                            .map(|data_type| (identifier_name(alias), data_type)),
                    ],
                    SelectItem::UnnamedExpr(expr) => vec![
                        expression_data_type(catalog, expr, &scope)
                            .map(|data_type| ("?column?".into(), data_type)),
                    ],
                    _ => vec![Err(PgError::new(
                        SqlState::FeatureNotSupported,
                        "SELECT projection is not implemented",
                    ))],
                })
                .collect::<Result<Vec<_>>>()
                .map(|columns| {
                    columns
                        .into_iter()
                        .enumerate()
                        .map(|(slot, (name, data_type))| BoundColumn {
                            name,
                            data_type,
                            qualifier: String::new(),
                            slot,
                            merged: None,
                            unqualified: true,
                            wildcard: true,
                        })
                        .collect()
                })
        }
        SetExpr::Values(values) => {
            let width = values.rows.first().map(|row| row.len()).unwrap_or(0);
            if values.rows.iter().any(|row| row.len() != width) {
                return Err(PgError::new(
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
                            !super::null_expression(expr) && super::unknown_string(expr).is_none()
                        })
                        .try_fold(None, |common, expr| {
                            let data_type = super::expression_type(
                                expr,
                                RowScope::Table(&super::constant_schema()),
                            )?;
                            Ok(Some(match common {
                                Some(common) => crate::coercion::common_type(common, data_type)
                                    .ok_or_else(|| {
                                        PgError::new(
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
                        data_type: PgType::new(data_type),
                        qualifier: String::new(),
                        slot,
                        merged: None,
                        unqualified: true,
                        wildcard: true,
                    })
                })
                .collect()
        }
        _ => Err(PgError::new(
            SqlState::FeatureNotSupported,
            "query source is not implemented",
        )),
    }
}

fn expression_data_type(catalog: &Catalog, expr: &Expr, scope: &BoundScope) -> Result<PgType> {
    if let Expr::Subquery(query) = expr {
        let columns = query_columns(catalog, query)?;
        if columns.len() != 1 {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "subquery must return only one column",
            ));
        }
        return Ok(columns[0].data_type);
    }
    Ok(PgType::new(super::expression_type(
        expr,
        RowScope::Bound(scope),
    )?))
}

fn bind_join_constraint(
    scope: &mut BoundScope,
    join: &Join,
    constraint: &JoinConstraint,
    left_start: usize,
    right_start: usize,
) -> Result<()> {
    match constraint {
        JoinConstraint::On(expression) => {
            let data_type = scope.resolve_column_for_join(expression)?;
            if data_type != PgType::new(crate::value::BaseType::Bool)
                && !super::null_expression(expression)
            {
                return Err(PgError::new(
                    SqlState::DatatypeMismatch,
                    "JOIN/ON clause must be type boolean",
                ));
            }
        }
        JoinConstraint::Using(columns) => {
            let columns = columns.iter().map(name).collect::<Result<Vec<_>>>()?;
            bind_join_columns(scope, &columns, left_start, right_start)?;
        }
        JoinConstraint::Natural => {
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
        JoinConstraint::None if matches!(join.join_operator, JoinOperator::CrossJoin(_)) => {}
        JoinConstraint::None => {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "INNER JOIN requires a join condition",
            ));
        }
    }
    Ok(())
}

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
                PgError::new(
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
                PgError::new(
                    SqlState::UndefinedColumn,
                    format!(
                        "column {name:?} specified in USING clause does not exist in right table"
                    ),
                )
            })?;
        let data_type = crate::coercion::common_type(left.data_type.base, right.data_type.base)
            .ok_or_else(|| {
                PgError::new(
                    SqlState::DatatypeMismatch,
                    "JOIN/USING types cannot be matched",
                )
            })?;
        left.data_type = PgType::new(data_type);
        left.merged = Some((left.slot, right.slot));
        right.unqualified = false;
        right.wildcard = false;
    }
    Ok(())
}

trait JoinExpressionType {
    fn resolve_column_for_join(&self, expression: &sqlparser::ast::Expr) -> Result<PgType>;
}

impl JoinExpressionType for BoundScope {
    fn resolve_column_for_join(&self, expression: &sqlparser::ast::Expr) -> Result<PgType> {
        Ok(PgType::new(super::expression_type(
            expression,
            RowScope::Bound(self),
        )?))
    }
}

pub(super) fn bind_select_scope(
    state: &DatabaseState,
    select: &sqlparser::ast::Select,
) -> Result<BoundScope> {
    bind_query_scope(&state.catalog, select)
}
