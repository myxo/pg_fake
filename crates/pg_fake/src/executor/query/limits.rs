use crate::{
    coercion::CastContext,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{
        StatementContext,
        expressions::{create_constant_expression_schema, evaluate_and_coerce},
        scope::RowScope,
    },
    value::{BaseType, Value},
};
use sqlparser::ast::{self, Spanned as _};

#[derive(Clone)]
pub(crate) struct PreparedLimit {
    occurrence: sqlparser::tokenizer::Span,
    sql: String,
    result: Option<(Option<usize>, usize)>,
    cursor: super::super::expressions::EvaluationCursor,
}

pub(super) enum RowCountClause {
    Limit,
    Offset,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn has_zero_limit(query: &ast::Query) -> bool {
    matches!(
        &query.limit_clause,
        Some(ast::LimitClause::LimitOffset {
            limit: Some(ast::Expr::Value(value)),
            ..
        }) if matches!(&value.value, ast::Value::Number(number, _) if number == "0")
    )
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn resolve_select_limit(
    query: &ast::Query,
    context: &StatementContext,
) -> Result<(Option<usize>, usize)> {
    if query.limit_clause.is_none() {
        return Ok((None, 0));
    }
    if !context.capture_lock_queries {
        return evaluate_select_limit(query, context);
    }
    let sql = format!("{:?} {query}", context.query_invocation);
    let mut cached = context
        .prepared_limits
        .lock()
        .expect("prepared limit mutex is poisoned");
    let mut prepared = cached
        .iter()
        .position(|entry| entry.occurrence == query.span() && entry.sql == sql)
        .map(|index| cached.remove(index))
        .unwrap_or_else(|| PreparedLimit {
            occurrence: query.span(),
            sql,
            result: None,
            cursor: Default::default(),
        });
    drop(cached);
    let result = if let Some(result) = prepared.result {
        Ok(result)
    } else {
        super::super::expressions::resume_evaluation(&mut prepared.cursor, context, |context| {
            evaluate_select_limit(query, context)
        })
    };
    prepared.result = result.as_ref().ok().copied();
    context
        .prepared_limits
        .lock()
        .expect("prepared limit mutex is poisoned")
        .push(prepared);
    result
}

fn evaluate_select_limit(
    query: &ast::Query,
    context: &StatementContext,
) -> Result<(Option<usize>, usize)> {
    match &query.limit_clause {
        None => Ok((None, 0)),
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
            Ok((limit, offset))
        }
        Some(ast::LimitClause::OffsetCommaLimit { .. }) => {
            reject_unsupported("LIMIT clause is not implemented")
        }
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn evaluate_row_count(
    expr: &ast::Expr,
    clause: RowCountClause,
    context: &StatementContext,
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
