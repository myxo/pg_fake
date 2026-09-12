//! Parameter analysis and binding for prepared statements.

use std::{borrow::Cow, ops::ControlFlow};

use sqlparser::ast;

use crate::{
    catalog::Catalog,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    value::{BaseType, PgType, Value},
};

mod literals;
mod parameter_types;
mod scopes;
mod subqueries;
mod validation;

pub(crate) use literals::{create_typed_cast, create_typed_literal};
pub(crate) use subqueries::substitute_typed_subqueries;

use parameter_types::{constrain_statement_parameters, finalize_parameter_types};
use validation::validate_statement;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn analyze_prepared_statement_parameters<'a>(
    described: &'a ast::Statement,
    data_modifying_ctes: &[ast::Statement],
    catalog: &Catalog,
    parameter_count: usize,
    supplied_types: &[Option<BaseType>],
) -> Result<(Vec<BaseType>, Cow<'a, ast::Statement>)> {
    let mut types = vec![None; parameter_count];
    types[..supplied_types.len()].copy_from_slice(supplied_types);
    for statement in data_modifying_ctes {
        constrain_statement_parameters(statement, catalog, &mut types)?;
    }
    constrain_statement_parameters(described, catalog, &mut types)?;
    let types = finalize_parameter_types(types);
    let bound = bind_parameters(described, &types, &vec![Value::Null; types.len()])?;
    validate_statement(&bound, catalog)?;
    for statement in data_modifying_ctes {
        let bound = bind_parameters(statement, &types, &vec![Value::Null; types.len()])?;
        validate_statement(&bound, catalog)?;
    }
    Ok((types, bound))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn coerce_parameters(
    parameter_types: &[BaseType],
    values: &[Value],
) -> Result<Vec<Value>> {
    if values.len() != parameter_types.len() {
        return Err(PgError::create(
            SqlState::ProtocolViolation,
            format!(
                "bind message supplies {} parameters, but prepared statement requires {}",
                values.len(),
                parameter_types.len()
            ),
        ));
    }
    values
        .iter()
        .cloned()
        .zip(parameter_types.iter().copied())
        .map(|(value, target)| coerce_parameter(value, target))
        .collect()
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn bind_parameters<'a>(
    statement: &'a ast::Statement,
    parameter_types: &[BaseType],
    values: &[Value],
) -> Result<Cow<'a, ast::Statement>> {
    let values = coerce_parameters(parameter_types, values)?;
    if values.is_empty() {
        return Ok(Cow::Borrowed(statement));
    }
    let mut statement = statement.clone();
    let mut error = None;
    let _ = ast::visit_expressions_mut(&mut statement, |expression| {
        let ast::Expr::Value(value) = expression else {
            return ControlFlow::Continue(());
        };
        let ast::Value::Placeholder(placeholder) = &value.value else {
            return ControlFlow::Continue(());
        };
        let index = match parse_placeholder_index(placeholder) {
            Ok(index) => index,
            Err(bind_error) => {
                error = Some(bind_error);
                return ControlFlow::Break(());
            }
        };
        let target = parameter_types[index];
        *expression = create_typed_literal(values[index].clone(), PgType::create(target));
        ControlFlow::Continue(())
    });
    error.map_or(Ok(Cow::Owned(statement)), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn count_parameters(statement: &ast::Statement) -> Result<usize> {
    let mut maximum = 0;
    let mut error = None;
    let _ = ast::visit_expressions(statement, |expression| {
        let ast::Expr::Value(value) = expression else {
            return ControlFlow::Continue(());
        };
        let ast::Value::Placeholder(placeholder) = &value.value else {
            return ControlFlow::Continue(());
        };
        match parse_placeholder_index(placeholder) {
            Ok(index) => maximum = maximum.max(index + 1),
            Err(parameter_error) => {
                error = Some(parameter_error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(maximum), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn parse_placeholder_index(placeholder: &str) -> Result<usize> {
    let index = placeholder
        .strip_prefix('$')
        .and_then(|index| index.parse::<usize>().ok())
        .filter(|index| *index > 0)
        .ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedParameter,
                format!("there is no parameter {placeholder}"),
            )
        })?;
    Ok(index - 1)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn coerce_parameter(value: Value, target: BaseType) -> Result<Value> {
    if matches!(target, BaseType::Json | BaseType::Jsonb)
        && let Value::Text(text) = value
    {
        return Value::parse(target, &text);
    }
    let Some(source) = value.get_base_type() else {
        return Ok(Value::Null);
    };
    coercion::coerce(value, source, PgType::create(target), CastContext::Implicit)
}
