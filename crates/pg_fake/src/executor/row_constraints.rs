use super::{
    StatementContext,
    expressions::{
        evaluate_and_coerce, extract_unknown_string_literal, infer_expression_type, is_null_literal,
    },
    scope::RowScope,
};
use crate::{
    catalog::TableSchema,
    coercion::CastContext,
    error::{PgError, Result, SqlState},
    value::{BaseType, Value},
};

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_not_null(schema: &TableSchema, row: &[Value]) -> Result<()> {
    if let Some(column) = schema
        .columns
        .iter()
        .zip(row)
        .find_map(|(column, value)| (!column.nullable && value.is_null()).then_some(column))
    {
        return Err(PgError::create(
            SqlState::NotNullViolation,
            format!(
                "null value in column {:?} of relation {:?} violates not-null constraint",
                column.name, schema.name
            ),
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_check_constraint_types(schema: &TableSchema) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check { expression, .. } = constraint else {
            continue;
        };
        let base = infer_expression_type(expression, RowScope::Table(schema))?;
        if base != BaseType::Bool
            && !is_null_literal(expression)
            && extract_unknown_string_literal(expression).is_none()
        {
            return Err(PgError::create(
                SqlState::DatatypeMismatch,
                "CHECK constraint must be a boolean expression",
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_check_constraints(
    schema: &TableSchema,
    row: &[Value],
    context: &StatementContext,
) -> Result<()> {
    for constraint in &schema.constraints {
        let crate::catalog::Constraint::Check { expression, .. } = constraint else {
            continue;
        };
        match evaluate_and_coerce(
            expression,
            BaseType::Bool,
            CastContext::Implicit,
            RowScope::Table(schema),
            row,
            context,
        )? {
            Value::Bool(true) | Value::Null => {}
            Value::Bool(false) => {
                return Err(PgError::create(
                    SqlState::CheckViolation,
                    format!(
                        "new row for relation {:?} violates check constraint",
                        schema.name
                    ),
                ));
            }
            _ => unreachable!("CHECK expression was type-checked"),
        }
    }
    Ok(())
}
