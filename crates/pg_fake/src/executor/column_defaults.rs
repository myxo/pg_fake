use super::{
    StatementContext,
    expressions::{
        create_constant_expression_schema, evaluate_assignment_expression,
        extract_unknown_string_literal, infer_expression_type,
    },
};
use crate::{
    catalog::ColumnDef,
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState},
    value::{BaseType, Value},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_column_default(
    column: &ColumnDef,
    context: &StatementContext,
) -> Result<Value> {
    let evaluate_default = || {
        if let Some(sequence) = &column.default_sequence {
            let value = context.sequences.get_next_resolved_value(sequence)?;
            return coercion::coerce(
                Value::Int8(value),
                BaseType::Int8,
                column.data_type,
                CastContext::Assignment,
                &context.timezone,
            );
        }
        let Some(expr) = &column.default else {
            return Ok(Value::Null);
        };
        evaluate_assignment_expression(
            expr,
            column.data_type,
            &create_constant_expression_schema(),
            &[],
            context,
        )
        .map_err(|error| {
            if error.sqlstate == SqlState::UndefinedColumn {
                PgError::create(
                    SqlState::FeatureNotSupported,
                    "cannot use column reference in DEFAULT expression",
                )
            } else {
                error
            }
        })
    };
    if let Some(cursor) = &context.evaluation_cursor {
        super::expressions::evaluate_in_cursor(cursor, evaluate_default)
    } else {
        evaluate_default()
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn validate_column_default(column: &ColumnDef) -> Result<()> {
    let Some(expression) = &column.default else {
        return Ok(());
    };
    if let Some(text) = extract_unknown_string_literal(expression) {
        coercion::coerce_unknown(text, column.data_type, CastContext::Assignment, "UTC")?;
        return Ok(());
    }
    let source = infer_expression_type(
        expression,
        super::scope::RowScope::Table(&create_constant_expression_schema()),
    )
    .map_err(|error| {
        if error.sqlstate == SqlState::UndefinedColumn {
            PgError::create(
                SqlState::FeatureNotSupported,
                "cannot use column reference in DEFAULT expression",
            )
        } else {
            error
        }
    })?;
    if coercion::can_cast(source, column.data_type.base, CastContext::Assignment) {
        Ok(())
    } else {
        Err(PgError::create(
            SqlState::DatatypeMismatch,
            "default expression has incompatible type",
        ))
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn is_default_expression(expr: &ast::Expr) -> bool {
    matches!(expr, ast::Expr::Identifier(identifier) if identifier.quote_style.is_none() && identifier.value.eq_ignore_ascii_case("default"))
}
