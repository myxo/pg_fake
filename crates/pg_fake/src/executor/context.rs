use super::{DatabaseState, RequiredRowLock, query, sequences::SequenceExecutionContext};
use crate::{
    QueryResult,
    error::{PgError, Result, SqlState},
    storage::RowId,
    txn::{CommandId, CommitSeq, Snapshot, Xid},
    value::Value,
};
use rand_chacha::ChaCha12Rng;
use sqlparser::{
    ast::{self, Spanned as _},
    tokenizer::Span,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::Instant,
};

#[derive(Clone)]
pub(crate) struct StatementContext {
    pub(crate) command_id: CommandId,
    pub(crate) transaction_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) statement_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) clock_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) timezone: String,
    pub(crate) deadline: Option<Instant>,
    pub(crate) rng: Arc<Mutex<ChaCha12Rng>>,
    pub(crate) sequences: SequenceExecutionContext,
    pub(crate) source_state: Option<Arc<DatabaseState>>,
    pub(crate) source_snapshot: Snapshot,
    pub(crate) prepared_inserts: Arc<Mutex<PreparedInsertCache>>,
    pub(crate) prepared_updates: Arc<Mutex<PreparedUpdateCache>>,
    pub(crate) prepared_mutation_targets: Arc<Mutex<MutationTargetCache>>,
    pub(crate) prepared_cte_results: Arc<Mutex<Vec<(Span, String, QueryResult)>>>,
    pub(crate) executed_ctes: Arc<Mutex<Vec<(Span, String)>>>,
    pub(crate) pending_cte_mutations: Arc<Mutex<Vec<PendingCteMutation>>>,
    pub(crate) prepared_subquery_results: Arc<Mutex<SubqueryResultCache>>,
    pub(crate) lateral_initplans: Arc<Mutex<super::lateral::InitplanCache>>,
    pub(crate) lateral_invocation: bool,
    pub(crate) prepares_subquery_results: Arc<AtomicBool>,
    pub(crate) row_lock_recheck: Arc<AtomicBool>,
    pub(crate) row_lock_recheck_locks: Arc<Mutex<Vec<RequiredRowLock>>>,
}

#[derive(Clone, Default)]
pub(crate) struct PreparedInsertCache {
    entries: Vec<(PreparedAstKey, ast::Insert, PreparedInsert)>,
}

#[derive(Clone)]
pub(crate) struct PreparedInsert {
    pub(crate) source_state: Option<Arc<DatabaseState>>,
    pub(crate) source_snapshot: Option<Snapshot>,
    pub(super) source_query: Option<query::QueryStreamState>,
    pub(crate) source_rows: Vec<Option<Vec<Value>>>,
    pub(crate) rows: Vec<Vec<Value>>,
    pub(crate) conflicts: Vec<Option<PreparedConflictUpdate>>,
    pub(crate) returned_rows: Option<Vec<Option<Vec<Value>>>>,
    pub(crate) error: Option<PgError>,
    pub(crate) complete: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct PreparedAstKey {
    occurrence: Span,
    sql: String,
}

#[derive(Clone)]
pub(crate) struct PendingCteMutation {
    pub(crate) occurrence: Span,
    pub(crate) name: String,
    pub(crate) statement: ast::Statement,
}

#[derive(Clone, Default)]
pub(crate) struct SubqueryResultCache {
    entries: Vec<(Span, String, QueryResult)>,
}

#[derive(Clone)]
pub(crate) struct PreparedConflictUpdate {
    pub(crate) row_id: RowId,
    pub(crate) version_xmin: Xid,
    pub(crate) current: Vec<Value>,
    pub(crate) updated: Option<Vec<Value>>,
}

#[derive(Clone)]
pub(crate) struct PreparedMutationTarget {
    pub(crate) row_id: RowId,
    pub(crate) version_xmin: Xid,
    pub(crate) current: Vec<Value>,
    pub(crate) bound_row: Option<Vec<Value>>,
}

#[derive(Clone, Default)]
pub(crate) struct MutationTargetCache {
    entries: Vec<(PreparedAstKey, CommitSeq, Vec<PreparedMutationTarget>)>,
}

#[derive(Clone, Default)]
pub(crate) struct PreparedUpdateCache {
    entries: Vec<(PreparedAstKey, ast::Update, Vec<PreparedUpdateRow>)>,
}

#[derive(Clone)]
pub(crate) struct PreparedUpdateRow {
    pub(crate) row_id: RowId,
    pub(crate) version_xmin: Xid,
    pub(crate) current: Vec<Value>,
    pub(crate) bound_row: Option<Vec<Value>>,
    pub(crate) updated: Option<Vec<Value>>,
}

impl StatementContext {
    pub(crate) fn check_timeout(&self) -> Result<()> {
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(PgError::create(
                SqlState::QueryCanceled,
                "canceling statement due to statement timeout",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn request_row_lock_recheck(&self) {
        self.row_lock_recheck.store(true, AtomicOrdering::Relaxed);
    }

    pub(super) fn request_row_lock_recheck_with_locks(&self, locks: Vec<RequiredRowLock>) {
        self.row_lock_recheck_locks
            .lock()
            .expect("trigger lock recheck mutex is poisoned")
            .extend(locks);
        self.request_row_lock_recheck();
    }

    pub(super) fn requires_row_lock_recheck(&self) -> bool {
        self.row_lock_recheck.load(AtomicOrdering::Relaxed)
    }

    pub(super) fn get_prepared_cte_result(
        &self,
        occurrence: Span,
        name: &str,
    ) -> Option<QueryResult> {
        self.prepared_cte_results
            .lock()
            .expect("prepared CTE results mutex is poisoned")
            .iter()
            .find_map(|(cached, cached_name, result)| {
                (*cached == occurrence && cached_name == name).then(|| result.clone())
            })
    }

    pub(super) fn set_prepared_cte_result(
        &self,
        occurrence: Span,
        name: String,
        result: QueryResult,
    ) {
        let mut prepared = self
            .prepared_cte_results
            .lock()
            .expect("prepared CTE results mutex is poisoned");
        assert!(
            prepared
                .iter()
                .all(|(cached, cached_name, _)| *cached != occurrence || cached_name != &name)
        );
        prepared.push((occurrence, name, result));
    }

    pub(super) fn get_executed_cte_result(
        &self,
        occurrence: Span,
        name: &str,
    ) -> Option<QueryResult> {
        if !self
            .executed_ctes
            .lock()
            .expect("executed CTE mutex is poisoned")
            .iter()
            .any(|(cached, cached_name)| *cached == occurrence && cached_name == name)
        {
            return None;
        }
        self.get_prepared_cte_result(occurrence, name)
    }

    pub(super) fn defer_cte_mutation(
        &self,
        occurrence: Span,
        name: String,
        statement: ast::Statement,
    ) {
        let mut pending = self
            .pending_cte_mutations
            .lock()
            .expect("pending CTE mutations mutex is poisoned");
        if pending
            .iter()
            .any(|cached| cached.occurrence == occurrence && cached.name == name)
        {
            return;
        }
        pending.push(PendingCteMutation {
            occurrence,
            name,
            statement,
        });
    }

    pub(super) fn get_pending_cte_mutation(&self) -> Option<PendingCteMutation> {
        self.pending_cte_mutations
            .lock()
            .expect("pending CTE mutations mutex is poisoned")
            .first()
            .cloned()
    }

    pub(crate) fn take_pending_cte_mutation(&self) -> Option<PendingCteMutation> {
        let mut pending = self
            .pending_cte_mutations
            .lock()
            .expect("pending CTE mutations mutex is poisoned");
        (!pending.is_empty()).then(|| pending.remove(0))
    }

    pub(crate) fn set_executed_cte_result(
        &self,
        occurrence: Span,
        name: String,
        result: QueryResult,
    ) {
        self.set_prepared_cte_result(occurrence, name.clone(), result);
        let mut executed = self
            .executed_ctes
            .lock()
            .expect("executed CTE mutex is poisoned");
        assert!(
            executed
                .iter()
                .all(|(cached, cached_name)| *cached != occurrence || cached_name != &name)
        );
        executed.push((occurrence, name));
    }

    pub(super) fn get_prepared_subquery_result(&self, query: &ast::Query) -> Option<QueryResult> {
        let occurrence = query.span();
        let sql = query.to_string();
        self.prepared_subquery_results
            .lock()
            .expect("prepared subquery results mutex is poisoned")
            .entries
            .iter()
            .find_map(|(cached, cached_sql, result)| {
                (*cached == occurrence && cached_sql == &sql).then(|| result.clone())
            })
    }

    pub(super) fn set_prepared_subquery_result(&self, query: &ast::Query, result: QueryResult) {
        let occurrence = query.span();
        let sql = query.to_string();
        let mut prepared = self
            .prepared_subquery_results
            .lock()
            .expect("prepared subquery results mutex is poisoned");
        assert!(
            prepared
                .entries
                .iter()
                .all(|(cached, cached_sql, _)| { *cached != occurrence || cached_sql != &sql })
        );
        prepared.entries.push((occurrence, sql, result));
    }

    pub(super) fn set_prepares_subquery_results(&self, enabled: bool) {
        self.prepares_subquery_results
            .store(enabled, AtomicOrdering::Relaxed);
    }

    pub(super) fn prepares_subquery_results(&self) -> bool {
        self.prepares_subquery_results.load(AtomicOrdering::Relaxed)
    }

    pub(crate) fn take_row_lock_recheck(&self) -> bool {
        self.row_lock_recheck.swap(false, AtomicOrdering::Relaxed)
    }

    pub(super) fn take_row_lock_recheck_locks(&self) -> Vec<RequiredRowLock> {
        std::mem::take(
            &mut *self
                .row_lock_recheck_locks
                .lock()
                .expect("trigger lock recheck mutex is poisoned"),
        )
    }

    pub(super) fn get_prepared_insert(&self, insert: &ast::Insert) -> Option<PreparedInsert> {
        let key = PreparedAstKey {
            occurrence: insert.span(),
            sql: insert.to_string(),
        };
        self.prepared_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned")
            .entries
            .iter()
            .find_map(|(cached, _, prepared)| (cached == &key).then(|| prepared.clone()))
    }

    pub(super) fn get_prior_prepared_inserts(
        &self,
        insert: &ast::Insert,
    ) -> Vec<(ast::Insert, PreparedInsert)> {
        let key = PreparedAstKey {
            occurrence: insert.span(),
            sql: insert.to_string(),
        };
        self.prepared_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned")
            .entries
            .iter()
            .take_while(|(cached, _, _)| cached != &key)
            .map(|(_, insert, prepared)| (insert.clone(), prepared.clone()))
            .collect()
    }

    pub(super) fn set_prepared_insert(&self, insert: &ast::Insert, value: PreparedInsert) {
        let key = PreparedAstKey {
            occurrence: insert.span(),
            sql: insert.to_string(),
        };
        let mut prepared = self
            .prepared_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned");
        match prepared
            .entries
            .iter_mut()
            .find(|(cached, _, _)| cached == &key)
        {
            Some((_, _, cached)) => *cached = value,
            None => prepared.entries.push((key, insert.clone(), value)),
        }
    }

    pub(super) fn take_prepared_insert(&self, insert: &ast::Insert) -> Option<PreparedInsert> {
        let mut prepared = self
            .prepared_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned");
        let position = prepared
            .entries
            .iter()
            .position(|(cached, _, _)| cached.occurrence == insert.span())?;
        let (_, _, value) = prepared.entries.remove(position);
        value.complete.then_some(value)
    }

    pub(super) fn get_prepared_update(
        &self,
        update: &ast::Update,
    ) -> (usize, Option<Vec<PreparedUpdateRow>>) {
        let prepared = self
            .prepared_updates
            .lock()
            .expect("prepared trigger rows mutex is poisoned");
        let key = PreparedAstKey {
            occurrence: update.span(),
            sql: update.to_string(),
        };
        let index = prepared
            .entries
            .iter()
            .position(|(cached, _, _)| cached == &key)
            .unwrap_or(prepared.entries.len());
        let rows = prepared.entries.get(index).map(|(_, _, rows)| rows.clone());
        (index, rows)
    }

    pub(super) fn get_prepared_mutation_targets(
        &self,
        occurrence: Span,
        snapshot: CommitSeq,
    ) -> Option<Vec<PreparedMutationTarget>> {
        let mut prepared = self
            .prepared_mutation_targets
            .lock()
            .expect("prepared mutation targets mutex is poisoned");
        let index = prepared
            .entries
            .iter()
            .position(|(key, _, _)| key.occurrence == occurrence)?;
        if prepared.entries[index].1 != snapshot {
            prepared.entries.remove(index);
            return None;
        }
        Some(prepared.entries[index].2.clone())
    }

    pub(super) fn set_prepared_mutation_targets(
        &self,
        occurrence: Span,
        sql: String,
        snapshot: CommitSeq,
        targets: Vec<PreparedMutationTarget>,
    ) {
        let mut prepared = self
            .prepared_mutation_targets
            .lock()
            .expect("prepared mutation targets mutex is poisoned");
        assert!(
            prepared
                .entries
                .iter()
                .all(|(key, _, _)| key.occurrence != occurrence)
        );
        prepared
            .entries
            .push((PreparedAstKey { occurrence, sql }, snapshot, targets));
    }

    pub(super) fn take_prepared_mutation_targets(
        &self,
        occurrence: Span,
        snapshot: CommitSeq,
    ) -> Option<Vec<PreparedMutationTarget>> {
        let mut prepared = self
            .prepared_mutation_targets
            .lock()
            .expect("prepared mutation targets mutex is poisoned");
        let index = prepared
            .entries
            .iter()
            .position(|(key, _, _)| key.occurrence == occurrence)?;
        let (_, cached_snapshot, targets) = prepared.entries.remove(index);
        (cached_snapshot == snapshot).then_some(targets)
    }

    pub(super) fn set_prepared_update(
        &self,
        index: usize,
        update: ast::Update,
        rows: Vec<PreparedUpdateRow>,
    ) {
        let mut prepared = self
            .prepared_updates
            .lock()
            .expect("prepared trigger rows mutex is poisoned");
        assert_eq!(index, prepared.entries.len());
        prepared.entries.push((
            PreparedAstKey {
                occurrence: update.span(),
                sql: update.to_string(),
            },
            update,
            rows,
        ));
    }

    pub(super) fn take_prepared_update(
        &self,
        update: &ast::Update,
    ) -> Option<Vec<PreparedUpdateRow>> {
        let mut prepared = self
            .prepared_updates
            .lock()
            .expect("prepared trigger rows mutex is poisoned");
        if prepared.entries.is_empty() {
            return None;
        }
        let position = prepared
            .entries
            .iter()
            .position(|(cached, _, _)| cached.occurrence == update.span())?;
        Some(prepared.entries.remove(position).2)
    }
}
