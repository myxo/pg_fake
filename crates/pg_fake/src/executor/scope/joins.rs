use super::{BoundScope, sources::bind_table_factor, subqueries::infer_expression_data_type};
use crate::executor::{
    expressions::{is_null_literal, validate_equality_type},
    json, normalize_unqualified_object_name, outer_references,
};
use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState, reject_unsupported},
    value::PgType,
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn bind_table_with_joins(
    catalog: &Catalog,
    table: &ast::TableWithJoins,
    scope: &mut BoundScope,
) -> Result<()> {
    let left_start = scope.columns.len();
    bind_table_factor(catalog, &table.relation, scope)?;
    for join in &table.joins {
        bind_join(catalog, join, scope, left_start)?;
    }
    Ok(())
}

pub(crate) fn bind_join(
    catalog: &Catalog,
    join: &ast::Join,
    scope: &mut BoundScope,
    left_start: usize,
) -> Result<()> {
    let right_start = scope.columns.len();
    if matches!(
        join.join_operator,
        ast::JoinOperator::Right(_)
            | ast::JoinOperator::RightOuter(_)
            | ast::JoinOperator::FullOuter(_)
    ) {
        validate_json_join_references(catalog, &join.relation, scope, left_start..right_start)?;
    }
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
    Ok(())
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
                && !is_null_literal(expression)
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
        validate_equality_type(data_type)?;
        left.data_type = PgType::create(data_type);
        let slots = left.merged.get_or_insert_with(|| vec![left.slot]);
        slots.extend(right.merged.clone().unwrap_or_else(|| vec![right.slot]));
        right.unqualified = false;
        right.wildcard = false;
    }
    let mut output = (left_start..scope.columns.len())
        .filter(|i| scope.columns[*i].wildcard)
        .collect::<Vec<_>>();
    output.sort_by_key(|i| {
        let column = &scope.columns[*i];
        (
            names
                .iter()
                .position(|name| column.name == *name)
                .unwrap_or(names.len()),
            column.output_order,
        )
    });
    for (index, slot) in output.into_iter().enumerate() {
        scope.columns[slot].output_order = left_start + index;
    }
    Ok(())
}

fn validate_json_join_references(
    catalog: &Catalog,
    factor: &ast::TableFactor,
    scope: &BoundScope,
    forbidden: std::ops::Range<usize>,
) -> Result<()> {
    if let Some(json::JsonTableFunction { argument, .. }) =
        json::extract_json_table_function(factor)?
    {
        let referenced = outer_references::collect_outer_reference_slots(catalog, argument, scope)?
            .iter()
            .any(|slot| forbidden.contains(slot));
        if referenced {
            return Err(PgError::create(
                SqlState::InvalidColumnReference,
                "invalid lateral reference in RIGHT or FULL JOIN",
            ));
        }
    }
    if let ast::TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        let mut visible = scope.clone();
        for source in std::iter::once(&table_with_joins.relation)
            .chain(table_with_joins.joins.iter().map(|j| &j.relation))
        {
            validate_json_join_references(catalog, source, &visible, forbidden.clone())?;
            bind_table_factor(catalog, source, &mut visible)?;
        }
    }
    Ok(())
}
