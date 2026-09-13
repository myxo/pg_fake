use crate::{
    Result,
    executor::{StatementContext, scope::RowScope},
    value::Value,
};
use sqlparser::ast::{self, Spanned as _};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub(crate) struct EvaluationCursor {
    next: usize,
    values: BTreeMap<usize, (usize, Value)>,
}

#[derive(Clone)]
pub(crate) struct PendingEvaluation {
    expression: ast::Expr,
    invocation: Vec<usize>,
    row: Vec<Value>,
    cursor: EvaluationCursor,
}

pub(in crate::executor) fn evaluate(
    expression: &ast::Expr,
    schema: RowScope<'_>,
    row: &[Value],
    context: &StatementContext,
) -> Result<Value> {
    if !context.advisory.enabled && !crate::advisory::contains_advisory_function(expression) {
        return super::evaluate_inner(expression, schema, row, context);
    }
    if let Some(cursor) = &context.evaluation_cursor {
        return evaluate_in_cursor(cursor, || {
            super::evaluate_inner(expression, schema, row, context)
        });
    }
    let mut pending = context
        .pending_evaluations
        .lock()
        .expect("pending evaluation mutex is poisoned");
    let mut cursor = pending
        .iter()
        .position(|cached| {
            cached.expression.span() == expression.span()
                && cached.expression == *expression
                && cached.invocation == context.query_invocation
                && cached.row == row
        })
        .map(|index| pending.remove(index).cursor)
        .unwrap_or_default();
    drop(pending);
    cursor.next = 0;
    let cursor = Arc::new(Mutex::new(cursor));
    let mut invocation = context.clone();
    invocation.advisory.enabled = true;
    invocation.evaluation_cursor = Some(cursor.clone());
    let result = evaluate(expression, schema, row, &invocation);
    if result.as_ref().is_err_and(|error| {
        error.sqlstate == crate::error::SqlState::InternalError
            && error.message == crate::executor::LOCK_PENDING
    }) {
        context
            .pending_evaluations
            .lock()
            .expect("pending evaluation mutex is poisoned")
            .push(PendingEvaluation {
                expression: expression.clone(),
                invocation: context.query_invocation.clone(),
                row: row.to_vec(),
                cursor: cursor
                    .lock()
                    .expect("evaluation cursor mutex is poisoned")
                    .clone(),
            });
    }
    result
}

pub(in crate::executor) fn evaluate_in_cursor(
    cursor: &Arc<Mutex<EvaluationCursor>>,
    operation: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let mut frame = cursor.lock().expect("evaluation cursor mutex is poisoned");
    let position = frame.next;
    if let Some((end, value)) = frame.values.get(&position).cloned() {
        frame.next = end;
        return Ok(value);
    }
    frame.next += 1;
    drop(frame);
    let result = operation()?;
    let mut frame = cursor.lock().expect("evaluation cursor mutex is poisoned");
    let end = frame.next;
    frame.values.insert(position, (end, result.clone()));
    Ok(result)
}

pub(in crate::executor) fn resume_evaluation<T>(
    cursor: &mut EvaluationCursor,
    context: &StatementContext,
    operation: impl FnOnce(&StatementContext) -> Result<T>,
) -> Result<T> {
    cursor.next = 0;
    let shared = Arc::new(Mutex::new(std::mem::take(cursor)));
    let mut invocation = context.clone();
    invocation.advisory.enabled = true;
    invocation.evaluation_cursor = Some(shared.clone());
    let result = operation(&invocation);
    *cursor = shared
        .lock()
        .expect("evaluation cursor mutex is poisoned")
        .clone();
    if result.is_ok() {
        *cursor = EvaluationCursor::default();
    }
    result
}

#[derive(Clone, PartialEq)]
pub(in crate::executor) enum EvaluationOperation {
    InsertRow(Box<ast::Insert>, usize),
    Returning(Vec<ast::SelectItem>, Vec<Value>),
    ConflictUpdate(Box<ast::DoUpdate>, Vec<Value>),
    UpdateRow(Box<ast::Update>, crate::storage::RowId),
}

#[derive(Clone)]
pub(crate) struct PendingOperation {
    operation: EvaluationOperation,
    invocation: Vec<usize>,
    cursor: EvaluationCursor,
}

pub(in crate::executor) fn resume_operation<T>(
    operation: EvaluationOperation,
    context: &StatementContext,
    evaluate: impl FnOnce(&StatementContext) -> Result<T>,
) -> Result<T> {
    let mut pending = context
        .pending_operations
        .lock()
        .expect("pending operation mutex is poisoned");
    let mut cursor = pending
        .iter()
        .position(|entry| {
            entry.operation == operation && entry.invocation == context.query_invocation
        })
        .map(|index| pending.remove(index).cursor)
        .unwrap_or_default();
    drop(pending);
    let result = resume_evaluation(&mut cursor, context, evaluate);
    if result.as_ref().is_err_and(|error| {
        error.sqlstate == crate::error::SqlState::InternalError
            && error.message == crate::executor::LOCK_PENDING
    }) {
        context
            .pending_operations
            .lock()
            .expect("pending operation mutex is poisoned")
            .push(PendingOperation {
                operation,
                invocation: context.query_invocation.clone(),
                cursor,
            });
    }
    result
}
