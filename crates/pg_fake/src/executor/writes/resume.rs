use super::targets::MutationTarget;
use crate::{
    executor::{PreparedUpdateRow, StatementContext},
    value::Value,
};
use sqlparser::tokenizer::Span;

#[derive(Clone)]
pub(crate) struct PreparedWrite {
    occurrence: Span,
    sql: String,
    pub(super) targets: Vec<MutationTarget>,
    pub(super) updates: Option<Vec<PreparedUpdateRow>>,
    pub(super) next: usize,
    pub(super) affected: u64,
    pub(super) returned_rows: Vec<Vec<Value>>,
    pub(super) pending_returning: Option<Vec<Value>>,
}

impl PreparedWrite {
    pub(super) fn create(
        occurrence: Span,
        sql: String,
        targets: Vec<MutationTarget>,
        updates: Option<Vec<PreparedUpdateRow>>,
    ) -> Self {
        Self {
            occurrence,
            sql,
            targets,
            updates,
            next: 0,
            affected: 0,
            returned_rows: Vec::new(),
            pending_returning: None,
        }
    }

    pub(super) fn take(context: &StatementContext, occurrence: Span, sql: &str) -> Option<Self> {
        let mut prepared = context
            .prepared_writes
            .lock()
            .expect("prepared write mutex is poisoned");
        prepared
            .iter()
            .position(|entry| entry.occurrence == occurrence && entry.sql == sql)
            .map(|index| prepared.remove(index))
    }

    pub(super) fn save(self, context: &StatementContext) {
        context
            .prepared_writes
            .lock()
            .expect("prepared write mutex is poisoned")
            .push(self);
    }
}
