use bigdecimal::ToPrimitive;
use rand_chacha::{ChaCha12Rng, rand_core::RngCore};

use crate::{
    api::{ColumnMeta, QueryResult, StatementResult},
    catalog::{
        Catalog, CatalogHistory, ColumnDef, ConstraintId, ForeignKey, ForeignKeyAction,
        IdentityKind, IndexColumnDefinition, IndexSchema, RelationName, ResolvedRelationName,
        SequenceSchema, TEMP_SCHEMA, TableId, TablePersistence, TableSchema,
    },
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    storage::{RowId, Table},
    txn::{
        CommandId, CommitSeq, RelationLockManager, RowLockKey, RowLockManager, RowLockMode,
        Snapshot, TransactionRegistry, TransactionStatus, WaitForGraph, Xid, find_visible_version,
    },
    value::{BaseType, DAYS_PER_MONTH, MICROSECONDS_PER_DAY, PgType, Value},
};
use sqlparser::{
    ast::{self, Spanned as _},
    tokenizer::Span,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::Instant,
};

mod aggregates;
mod alter_table;
mod arithmetic;
mod expressions;
mod foreign_keys;
mod indexes;
mod locks;
mod prepared;
mod procedural;
mod query;
mod scope;
mod sequences;
mod views;
mod writes;

use aggregates::{
    AggregateInput, evaluate_prepared_aggregate_function, infer_aggregate_return_type,
    is_aggregate_function, prepare_aggregate_function_input,
};
use arithmetic::{
    evaluate_boolean_operator, evaluate_distinctness, evaluate_numeric_operator,
    evaluate_temporal_arithmetic, evaluate_unary_operator, infer_interval_arithmetic_type,
};
use expressions::{
    compare_values, evaluate, evaluate_and_coerce, evaluate_assignment_expression,
    evaluate_column_default, evaluate_comparison, extract_number_literal, is_default_expression,
    resolve_operator_type, validate_check_constraint_types, validate_check_constraints,
    validate_column_default, validate_not_null,
};
pub(crate) use expressions::{
    create_constant_expression_schema, evaluate_index_predicate, extract_unknown_string_literal,
    infer_expression_data_type, infer_expression_type, is_null_literal, validate_index_predicate,
};
use foreign_keys::{
    apply_referencing_foreign_key_actions, convert_referential_action,
    resolve_foreign_key_column_indexes, resolve_foreign_key_name, validate_foreign_key_definitions,
    validate_row_foreign_keys,
};
pub(crate) use foreign_keys::{contains_deferred_foreign_keys, validate_deferred_foreign_keys};
use indexes::{execute_alter_index, execute_create_index, execute_drop_indexes};
pub(crate) use locks::{
    collect_required_cte_row_locks, collect_required_row_locks, mutation_locks_cover_targets,
};
pub(crate) use prepared::{PreparedQueryPlan, build_prepared_query_plan, execute_prepared_query};
pub(crate) use procedural::coerce_procedural_value;
pub(crate) use scope::infer_query_output_columns;
use scope::{BoundColumn, bind_select_scope};
pub(crate) use scope::{
    BoundScope, RowScope, bind_from_scope, bind_query_scope, bind_target_scope,
    combine_bound_scopes, create_value_scope, identify_unknown_query_columns,
    substitute_typed_subqueries,
};
pub(crate) use sequences::{
    SequenceExecutionContext, SequenceSessionState, SequenceSessionStorage, SequenceStorage,
    SequenceValueState,
};
use views::{
    execute_alter_trigger, execute_comment_on_view, execute_create_view, execute_drop_views,
};
use writes::{execute_delete, execute_insert, execute_update};

#[derive(Clone)]
pub(crate) struct StatementExecutionContext {
    pub(crate) command_id: CommandId,
    pub(crate) transaction_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) statement_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) clock_timestamp: chrono::DateTime<chrono::Utc>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) rng: Arc<Mutex<ChaCha12Rng>>,
    pub(crate) sequences: SequenceExecutionContext,
    pub(crate) source_state: Option<Arc<DatabaseState>>,
    pub(crate) source_snapshot: Snapshot,
    pub(crate) prepared_trigger_inserts: Arc<Mutex<PreparedTriggerInserts>>,
    pub(crate) prepared_trigger_updates: Arc<Mutex<PreparedTriggerUpdates>>,
    pub(crate) prepared_mutation_targets: Arc<Mutex<PreparedMutationTargets>>,
    pub(crate) prepared_cte_results: Arc<Mutex<Vec<(Span, String, QueryResult)>>>,
    pub(crate) executed_ctes: Arc<Mutex<Vec<(Span, String)>>>,
    pub(crate) pending_cte_mutations: Arc<Mutex<Vec<PendingCteMutation>>>,
    pub(crate) prepared_subquery_results: Arc<Mutex<PreparedSubqueryResults>>,
    pub(crate) prepares_subquery_results: Arc<AtomicBool>,
    pub(crate) trigger_lock_recheck: Arc<AtomicBool>,
    pub(crate) trigger_lock_recheck_locks: Arc<Mutex<Vec<RequiredRowLock>>>,
}

#[derive(Clone, Default)]
pub(crate) struct PreparedTriggerInserts {
    entries: Vec<(PreparedAstKey, ast::Insert, PreparedTriggerInsert)>,
}

#[derive(Clone)]
pub(crate) struct PreparedTriggerInsert {
    pub(crate) source_state: Option<Arc<DatabaseState>>,
    pub(crate) source_snapshot: Option<Snapshot>,
    source_query: Option<query::PreparedQueryStream>,
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
pub(crate) struct PreparedSubqueryResults {
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
pub(crate) struct PreparedMutationTargets {
    entries: Vec<(PreparedAstKey, CommitSeq, Vec<PreparedMutationTarget>)>,
}

#[derive(Clone, Default)]
pub(crate) struct PreparedTriggerUpdates {
    entries: Vec<(PreparedAstKey, ast::Update, Vec<PreparedTriggerUpdate>)>,
}

#[derive(Clone)]
pub(crate) struct PreparedTriggerUpdate {
    pub(crate) row_id: RowId,
    pub(crate) version_xmin: Xid,
    pub(crate) current: Vec<Value>,
    pub(crate) bound_row: Option<Vec<Value>>,
    pub(crate) updated: Option<Vec<Value>>,
}

impl StatementExecutionContext {
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

    pub(super) fn request_trigger_lock_recheck(&self) {
        self.trigger_lock_recheck
            .store(true, AtomicOrdering::Relaxed);
    }

    pub(super) fn request_trigger_lock_recheck_with_locks(&self, locks: Vec<RequiredRowLock>) {
        self.trigger_lock_recheck_locks
            .lock()
            .expect("trigger lock recheck mutex is poisoned")
            .extend(locks);
        self.request_trigger_lock_recheck();
    }

    pub(super) fn requires_trigger_lock_recheck(&self) -> bool {
        self.trigger_lock_recheck.load(AtomicOrdering::Relaxed)
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

    pub(crate) fn take_trigger_lock_recheck(&self) -> bool {
        self.trigger_lock_recheck
            .swap(false, AtomicOrdering::Relaxed)
    }

    pub(super) fn take_trigger_lock_recheck_locks(&self) -> Vec<RequiredRowLock> {
        std::mem::take(
            &mut *self
                .trigger_lock_recheck_locks
                .lock()
                .expect("trigger lock recheck mutex is poisoned"),
        )
    }

    pub(super) fn get_prepared_trigger_insert(
        &self,
        insert: &ast::Insert,
    ) -> Option<PreparedTriggerInsert> {
        let key = PreparedAstKey {
            occurrence: insert.span(),
            sql: insert.to_string(),
        };
        self.prepared_trigger_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned")
            .entries
            .iter()
            .find_map(|(cached, _, prepared)| (cached == &key).then(|| prepared.clone()))
    }

    pub(super) fn get_prior_prepared_trigger_inserts(
        &self,
        insert: &ast::Insert,
    ) -> Vec<(ast::Insert, PreparedTriggerInsert)> {
        let key = PreparedAstKey {
            occurrence: insert.span(),
            sql: insert.to_string(),
        };
        self.prepared_trigger_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned")
            .entries
            .iter()
            .take_while(|(cached, _, _)| cached != &key)
            .map(|(_, insert, prepared)| (insert.clone(), prepared.clone()))
            .collect()
    }

    pub(super) fn set_prepared_trigger_insert(
        &self,
        insert: &ast::Insert,
        value: PreparedTriggerInsert,
    ) {
        let key = PreparedAstKey {
            occurrence: insert.span(),
            sql: insert.to_string(),
        };
        let mut prepared = self
            .prepared_trigger_inserts
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

    pub(super) fn take_prepared_trigger_insert(
        &self,
        insert: &ast::Insert,
    ) -> Option<PreparedTriggerInsert> {
        let mut prepared = self
            .prepared_trigger_inserts
            .lock()
            .expect("prepared trigger INSERT mutex is poisoned");
        let position = prepared
            .entries
            .iter()
            .position(|(cached, _, _)| cached.occurrence == insert.span())?;
        let (_, _, value) = prepared.entries.remove(position);
        value.complete.then_some(value)
    }

    pub(super) fn get_prepared_trigger_update(
        &self,
        update: &ast::Update,
    ) -> (usize, Option<Vec<PreparedTriggerUpdate>>) {
        let prepared = self
            .prepared_trigger_updates
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

    pub(super) fn set_prepared_trigger_update(
        &self,
        index: usize,
        update: ast::Update,
        rows: Vec<PreparedTriggerUpdate>,
    ) {
        let mut prepared = self
            .prepared_trigger_updates
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

    pub(super) fn take_prepared_trigger_update(
        &self,
        update: &ast::Update,
    ) -> Option<Vec<PreparedTriggerUpdate>> {
        let mut prepared = self
            .prepared_trigger_updates
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

#[derive(Clone)]
pub(crate) struct DatabaseState {
    pub(crate) catalog: Catalog,
    pub(crate) catalog_history: CatalogHistory,
    pub(crate) tables: BTreeMap<TableId, Table>,
    pub(crate) transactions: TransactionRegistry,
    pub(crate) row_locks: RowLockManager,
    pub(crate) relation_locks: RelationLockManager,
    pub(crate) wait_for: WaitForGraph,
    pub(crate) sequence_values: SequenceStorage,
    touched_tables: BTreeMap<Xid, Vec<TableId>>,
    reclaimable_tables: Vec<TableId>,
}
#[derive(Clone)]
pub(crate) struct RequiredRowLock {
    pub(crate) key: RowLockKey,
    pub(crate) mode: RowLockMode,
    pub(crate) mutation_candidate: Option<MutationCandidate>,
}
#[derive(Clone)]
pub(crate) struct MutationCandidate {
    pub(crate) version_xmin: Xid,
    pub(crate) row: Option<Vec<Value>>,
}

pub(crate) use query::describe_query_result_columns;
pub(crate) use query::detect_statement_features;
pub(crate) use query::expand_ctes_for_analysis;
pub(crate) use query::materialize_ctes;
pub(crate) use query::materialize_uncorrelated_subqueries;
pub(crate) use query::substitute_procedural_references;
pub(crate) use sequences::normalize_sequence_name;

impl DatabaseState {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn create() -> Self {
        let catalog_history = CatalogHistory::create();
        let transactions = TransactionRegistry::create();
        let catalog =
            catalog_history.materialize(None, Snapshot::create(&transactions), &transactions);
        DatabaseState {
            catalog,
            catalog_history,
            tables: BTreeMap::new(),
            transactions,
            row_locks: RowLockManager::create(),
            relation_locks: RelationLockManager::create(),
            wait_for: WaitForGraph::create(),
            sequence_values: Arc::new(Mutex::new(BTreeMap::new())),
            touched_tables: BTreeMap::new(),
            reclaimable_tables: Vec::new(),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn load_catalog(
        &mut self,
        xid: Option<Xid>,
        snapshot: Snapshot,
        temporary_schema_id: Option<crate::catalog::SchemaId>,
    ) {
        self.catalog = self.catalog_history.materialize_for_session(
            xid,
            snapshot,
            &self.transactions,
            temporary_schema_id,
        );
        let schemas = self.catalog.iterate_tables().cloned().collect::<Vec<_>>();
        for schema in schemas {
            if let Some(table) = self.tables.get_mut(&schema.id) {
                table.replace_schema(schema);
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn record_catalog_changes(
        &mut self,
        previous: &Catalog,
        xid: Xid,
        command_id: CommandId,
    ) {
        self.catalog_history
            .record_changes(previous, &self.catalog, xid, command_id);
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn mark_table_touched(&mut self, xid: Xid, table_id: TableId) {
        let tables = self.touched_tables.entry(xid).or_default();
        if !tables.contains(&table_id) {
            tables.push(table_id);
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn has_touched_tables(&self, xid: Xid) -> bool {
        self.touched_tables
            .get(&xid)
            .is_some_and(|tables| !tables.is_empty())
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn take_touched_tables(&mut self, xid: Xid) -> Vec<TableId> {
        self.touched_tables.remove(&xid).unwrap_or_default()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn collect_touched_tables(&self) -> BTreeSet<TableId> {
        self.touched_tables.values().flatten().copied().collect()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn mark_table_reclaimable(&mut self, table_id: TableId) {
        if !self.reclaimable_tables.contains(&table_id) {
            self.reclaimable_tables.push(table_id);
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn reclaimable_table_ids(&self) -> Vec<TableId> {
        self.reclaimable_tables.clone()
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub(crate) fn clear_table_reclaimable(&mut self, table_id: TableId) {
        self.reclaimable_tables
            .retain(|candidate| *candidate != table_id);
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn execute_statement(
    state: &mut DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementExecutionContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    context.check_timeout()?;
    match statement {
        ast::Statement::CreateTable(create) => {
            if create.query.is_some() {
                return reject_unsupported("CREATE TABLE AS is not implemented");
            }
            if create.like.is_some() || create.clone.is_some() {
                return reject_unsupported("CREATE TABLE LIKE is not implemented");
            }
            if create.inherits.is_some() {
                return reject_unsupported("table inheritance is not implemented");
            }
            if create.partition_by.is_some()
                || create.partition_of.is_some()
                || create.for_values.is_some()
            {
                return reject_unsupported("table partitioning is not implemented");
            }
            if !matches!(create.table_options, ast::CreateTableOptions::None) {
                return reject_unsupported("CREATE TABLE options are not implemented");
            }
            let relation_name = normalize_relation_name(&create.name)?;
            let temporary =
                create.temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
            if create.on_commit.is_some() && !temporary {
                return Err(PgError::create(
                    SqlState::InvalidTableDefinition,
                    "ON COMMIT can only be used on temporary tables",
                ));
            }
            let on_commit_drop = match create.on_commit {
                None | Some(ast::OnCommit::PreserveRows) => false,
                Some(ast::OnCommit::Drop) => true,
                Some(ast::OnCommit::DeleteRows) => {
                    return reject_unsupported("ON COMMIT DELETE ROWS is not implemented");
                }
            };
            let resolved_name = state
                .catalog
                .resolve_creation_name(&relation_name, temporary)?;
            let table_name = resolved_name.name.clone();
            let persistence = if temporary {
                TablePersistence::Temporary { on_commit_drop }
            } else {
                TablePersistence::Permanent
            };
            if create.if_not_exists && state.catalog.has_resolved_relation(&resolved_name) {
                return Ok(StatementResult::Affected(0));
            }
            let mut columns = Vec::new();
            let mut constraints = Vec::new();
            let mut sequence_schemas = Vec::new();
            for column in &create.columns {
                let column_name = normalize_identifier(&column.name);
                let serial_type = match column.data_type.to_string().to_ascii_lowercase().as_str() {
                    "smallserial" | "serial2" => Some(BaseType::Int2),
                    "serial" | "serial4" => Some(BaseType::Int4),
                    "bigserial" | "serial8" => Some(BaseType::Int8),
                    _ => None,
                };
                let data_type = match serial_type {
                    Some(base) => PgType::create(base),
                    None => coercion::convert_ast_data_type(&column.data_type)?,
                };
                let mut nullable = true;
                let mut default = None;
                let mut default_sequence = None;
                let mut identity = None;
                for option in &column.options {
                    match &option.option {
                        ast::ColumnOption::Null => nullable = true,
                        ast::ColumnOption::NotNull => nullable = false,
                        ast::ColumnOption::Default(expr) => {
                            if serial_type.is_some() || identity.is_some() {
                                return Err(PgError::create(
                                    SqlState::SyntaxError,
                                    "multiple default values specified for column",
                                ));
                            }
                            default = Some(expr.clone());
                            default_sequence =
                                resolve_default_sequence(&state.catalog, expr, persistence)?;
                        }
                        ast::ColumnOption::PrimaryKey(_) => {
                            let columns = vec![column_name.clone()];
                            constraints.push(crate::catalog::Constraint::PrimaryKey {
                                id: ConstraintId(0),
                                name: option
                                    .name
                                    .as_ref()
                                    .map(normalize_identifier)
                                    .unwrap_or_else(|| format!("{table_name}_pkey")),
                                columns,
                            });
                        }
                        ast::ColumnOption::Unique(_) => {
                            let columns = vec![column_name.clone()];
                            constraints.push(crate::catalog::Constraint::Unique {
                                id: ConstraintId(0),
                                name: option
                                    .name
                                    .as_ref()
                                    .map(normalize_identifier)
                                    .unwrap_or_else(|| format!("{table_name}_{column_name}_key")),
                                columns,
                            });
                        }
                        ast::ColumnOption::Check(check) => {
                            constraints.push(crate::catalog::Constraint::Check {
                                id: ConstraintId(0),
                                name: option
                                    .name
                                    .as_ref()
                                    .map(normalize_identifier)
                                    .unwrap_or_else(|| {
                                        generate_constraint_name(
                                            format!("{table_name}_{column_name}_check"),
                                            &constraints,
                                        )
                                    }),
                                expression: check.expr.clone(),
                                validated: true,
                            })
                        }
                        ast::ColumnOption::ForeignKey(foreign_key) => {
                            let name = resolve_foreign_key_name(
                                option.name.as_ref(),
                                format!("{}_{}_fkey", table_name, column_name),
                            );
                            constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                                id: ConstraintId(0),
                                name,
                                columns: vec![column_name.clone()],
                                foreign_table: crate::executor::normalize_relation_name(
                                    &foreign_key.foreign_table,
                                )?,
                                foreign_table_id: TableId(0),
                                referred_columns: foreign_key
                                    .referred_columns
                                    .iter()
                                    .map(normalize_identifier)
                                    .collect(),
                                on_delete: convert_referential_action(foreign_key.on_delete),
                                on_update: convert_referential_action(foreign_key.on_update),
                                deferrable: foreign_key.characteristics.is_some_and(
                                    |characteristics| characteristics.deferrable.unwrap_or(false),
                                ),
                                initially_deferred: foreign_key.characteristics.is_some_and(
                                    |characteristics| {
                                        characteristics.initially
                                            == Some(ast::DeferrableInitial::Deferred)
                                    },
                                ),
                                match_kind: foreign_key.match_kind,
                                validated: true,
                            }))
                        }
                        ast::ColumnOption::Generated {
                            generated_as,
                            sequence_options,
                            generation_expr,
                            generation_expr_mode,
                            generated_keyword,
                        } => {
                            if serial_type.is_some()
                                || default.is_some()
                                || identity.is_some()
                                || generation_expr.is_some()
                                || generation_expr_mode.is_some()
                                || !generated_keyword
                            {
                                return Err(PgError::create(
                                    SqlState::SyntaxError,
                                    "invalid identity column declaration",
                                ));
                            }
                            let kind = match generated_as {
                                ast::GeneratedAs::Always => IdentityKind::Always,
                                ast::GeneratedAs::ByDefault => IdentityKind::ByDefault,
                                ast::GeneratedAs::ExpStored => {
                                    return Err(PgError::create(
                                        SqlState::SyntaxError,
                                        "invalid identity column declaration",
                                    ));
                                }
                            };
                            if !matches!(
                                data_type.base,
                                BaseType::Int2 | BaseType::Int4 | BaseType::Int8
                            ) {
                                return Err(PgError::create(
                                    SqlState::DatatypeMismatch,
                                    "identity column type must be smallint, integer, or bigint",
                                ));
                            }
                            let sequence_name = create_generated_sequence_name(
                                &state.catalog,
                                &sequence_schemas,
                                &resolved_name,
                                &column_name,
                            );
                            let sequence = sequences::create_sequence_schema_for_type(
                                sequence_name.clone(),
                                data_type.base,
                                sequence_options.as_deref().unwrap_or(&[]),
                            )?;
                            sequence_schemas.push(sequence);
                            nullable = false;
                            default_sequence = Some(ResolvedRelationName {
                                schema_id: resolved_name.schema_id,
                                name: sequence_name,
                            });
                            identity = Some(kind);
                        }
                        option => {
                            return reject_unsupported(format!(
                                "column option is not implemented: {option}"
                            ));
                        }
                    }
                }
                if let Some(base) = serial_type {
                    let sequence_name = create_generated_sequence_name(
                        &state.catalog,
                        &sequence_schemas,
                        &resolved_name,
                        &column_name,
                    );
                    let sequence = sequences::create_sequence_schema_for_type(
                        sequence_name.clone(),
                        base,
                        &[],
                    )?;
                    sequence_schemas.push(sequence);
                    nullable = false;
                    default_sequence = Some(ResolvedRelationName {
                        schema_id: resolved_name.schema_id,
                        name: sequence_name,
                    });
                }
                let column = ColumnDef {
                    name: column_name,
                    data_type,
                    nullable,
                    default,
                    default_sequence,
                    identity,
                };
                validate_column_default(&column)?;
                columns.push(column);
            }
            for constraint in &create.constraints {
                match constraint {
                    ast::TableConstraint::PrimaryKey(primary_key) => {
                        let columns = primary_key
                            .columns
                            .iter()
                            .map(resolve_index_column_name)
                            .collect::<Result<Vec<_>>>()?;
                        constraints.push(crate::catalog::Constraint::PrimaryKey {
                            id: ConstraintId(0),
                            name: primary_key
                                .name
                                .as_ref()
                                .map(normalize_identifier)
                                .unwrap_or_else(|| format!("{table_name}_pkey")),
                            columns,
                        })
                    }
                    ast::TableConstraint::Unique(unique) => {
                        let columns = unique
                            .columns
                            .iter()
                            .map(resolve_index_column_name)
                            .collect::<Result<Vec<_>>>()?;
                        let default_name = format!("{table_name}_{}_key", columns.join("_"));
                        constraints.push(crate::catalog::Constraint::Unique {
                            id: ConstraintId(0),
                            name: unique
                                .name
                                .as_ref()
                                .map(normalize_identifier)
                                .unwrap_or(default_name),
                            columns,
                        })
                    }
                    ast::TableConstraint::Check(check) => {
                        let base = find_first_referenced_column(&check.expr, &columns).map_or_else(
                            || format!("{table_name}_check"),
                            |column| format!("{table_name}_{column}_check"),
                        );
                        constraints.push(crate::catalog::Constraint::Check {
                            id: ConstraintId(0),
                            name: check
                                .name
                                .as_ref()
                                .map(normalize_identifier)
                                .unwrap_or_else(|| generate_constraint_name(base, &constraints)),
                            expression: check.expr.clone(),
                            validated: true,
                        })
                    }
                    ast::TableConstraint::ForeignKey(foreign_key) => {
                        let foreign_key_columns = foreign_key
                            .columns
                            .iter()
                            .map(normalize_identifier)
                            .collect::<Vec<_>>();
                        let name = resolve_foreign_key_name(
                            foreign_key.name.as_ref(),
                            format!("{}_{}_fkey", table_name, foreign_key_columns.join("_")),
                        );
                        constraints.push(crate::catalog::Constraint::ForeignKey(ForeignKey {
                            id: ConstraintId(0),
                            name,
                            columns: foreign_key_columns,
                            foreign_table: crate::executor::normalize_relation_name(
                                &foreign_key.foreign_table,
                            )?,
                            foreign_table_id: TableId(0),
                            referred_columns: foreign_key
                                .referred_columns
                                .iter()
                                .map(normalize_identifier)
                                .collect(),
                            on_delete: convert_referential_action(foreign_key.on_delete),
                            on_update: convert_referential_action(foreign_key.on_update),
                            deferrable: foreign_key.characteristics.is_some_and(
                                |characteristics| characteristics.deferrable.unwrap_or(false),
                            ),
                            initially_deferred: foreign_key.characteristics.is_some_and(
                                |characteristics| {
                                    characteristics.initially
                                        == Some(ast::DeferrableInitial::Deferred)
                                },
                            ),
                            match_kind: foreign_key.match_kind,
                            validated: true,
                        }))
                    }
                    constraint => {
                        return reject_unsupported(format!(
                            "table constraint is not implemented: {constraint}"
                        ));
                    }
                }
            }
            for constraint in &constraints {
                let (constraint_columns, primary_key) = match constraint {
                    crate::catalog::Constraint::PrimaryKey { columns, .. } => (columns, true),
                    crate::catalog::Constraint::Unique { columns, .. } => (columns, false),
                    crate::catalog::Constraint::Check { .. }
                    | crate::catalog::Constraint::ForeignKey(_) => continue,
                };
                for name in constraint_columns {
                    let column = columns
                        .iter_mut()
                        .find(|column| column.name == *name)
                        .ok_or_else(|| {
                            PgError::create(
                                SqlState::UndefinedColumn,
                                format!("column {name:?} does not exist"),
                            )
                        })?;
                    if primary_key {
                        column.nullable = false;
                    }
                }
            }
            validate_check_constraint_types(&TableSchema {
                id: TableId(0),
                schema_id: resolved_name.schema_id,
                name: table_name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
                indexes: Vec::new(),
                triggers: Vec::new(),
                persistence,
            })?;
            let proposed = TableSchema {
                id: TableId(0),
                schema_id: resolved_name.schema_id,
                name: table_name.clone(),
                columns: columns.clone(),
                constraints: constraints.clone(),
                indexes: Vec::new(),
                triggers: Vec::new(),
                persistence,
            };
            validate_foreign_key_definitions(&state.catalog, &proposed)?;
            let id = state.catalog.create_named_table(
                resolved_name.clone(),
                columns,
                constraints,
                proposed.persistence,
            )?;
            let table = state
                .catalog
                .require_table_by_id(id)
                .expect("created table must exist")
                .clone();
            state.tables.insert(id, Table::create(table.clone()));
            for mut sequence in sequence_schemas {
                let column = table
                    .columns
                    .iter()
                    .find(|column| {
                        column.default_sequence.as_ref().is_some_and(|name| {
                            name.schema_id == resolved_name.schema_id && name.name == sequence.name
                        })
                    })
                    .expect("generated sequence must belong to a table column");
                sequence.owned_by = Some((id, column.name.clone()));
                let initial = SequenceValueState {
                    last_value: sequence.start_value,
                    is_called: false,
                };
                let id = state.catalog.create_named_sequence(
                    ResolvedRelationName {
                        schema_id: resolved_name.schema_id,
                        name: sequence.name.clone(),
                    },
                    sequence,
                )?;
                state
                    .sequence_values
                    .lock()
                    .expect("sequence storage is poisoned")
                    .insert(id, initial);
            }
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::CreateSequence {
            temporary,
            if_not_exists,
            name,
            data_type,
            sequence_options,
            owned_by,
        } => {
            let relation_name = normalize_relation_name(name)?;
            let temporary = *temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
            let resolved_name = state
                .catalog
                .resolve_creation_name(&relation_name, temporary)?;
            if *if_not_exists && state.catalog.has_resolved_relation(&resolved_name) {
                return Ok(StatementResult::Affected(0));
            }
            let owned_by = match owned_by {
                None => None,
                Some(owned_by)
                    if owned_by.0.len() == 1
                        && owned_by.0[0]
                            .as_ident()
                            .is_some_and(|name| name.value.eq_ignore_ascii_case("none")) =>
                {
                    None
                }
                Some(owned_by) if matches!(owned_by.0.len(), 2 | 3) => {
                    let Some(column) = owned_by.0.last().and_then(|part| part.as_ident()) else {
                        return reject_unsupported("sequence ownership is not implemented");
                    };
                    let column_name = normalize_identifier(column);
                    let table_name = normalize_relation_name(&ast::ObjectName(
                        owned_by.0[..owned_by.0.len() - 1].to_vec(),
                    ))?;
                    let table = state.catalog.require_named_table(&table_name)?;
                    if table.schema_id != resolved_name.schema_id {
                        return Err(PgError::create(
                            SqlState::ObjectNotInPrerequisiteState,
                            "sequence must be in the same schema as its owned table",
                        ));
                    }
                    if !table
                        .columns
                        .iter()
                        .any(|column| column.name == column_name)
                    {
                        return Err(PgError::create(
                            SqlState::UndefinedColumn,
                            format!(
                                "column {column_name:?} of relation {:?} does not exist",
                                table.name
                            ),
                        ));
                    }
                    Some((table.id, column_name))
                }
                Some(_) => return reject_unsupported("sequence ownership is not implemented"),
            };
            let mut sequence = sequences::create_sequence_schema(
                resolved_name.name.clone(),
                data_type.as_ref(),
                sequence_options,
            )?;
            sequence.owned_by = owned_by;
            let initial = SequenceValueState {
                last_value: sequence.start_value,
                is_called: false,
            };
            let id = state
                .catalog
                .create_named_sequence(resolved_name, sequence)?;
            state
                .sequence_values
                .lock()
                .expect("sequence storage is poisoned")
                .insert(id, initial);
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::CreateView(create) => execute_create_view(state, create),
        ast::Statement::CreateFunction(create) => {
            procedural::execute_create_function(state, create)
        }
        ast::Statement::DropFunction(drop) => {
            procedural::execute_drop_function(state, drop, xid, snapshot)
        }
        ast::Statement::CreateTrigger(create) => procedural::execute_create_trigger(state, create),
        ast::Statement::DropTrigger(drop) => procedural::execute_drop_trigger(state, drop),
        ast::Statement::AlterTrigger {
            name,
            table_name,
            new_name,
        } => execute_alter_trigger(state, name, table_name, new_name),
        ast::Statement::Comment {
            object_type: ast::CommentObject::View,
            object_name,
            comment,
            if_exists: false,
        } => execute_comment_on_view(state, object_name, comment),
        ast::Statement::AlterTable(alter) => alter_table::execute_alter_table(
            state,
            alter,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        ),
        ast::Statement::CreateIndex(create) => {
            execute_create_index(state, create, xid, snapshot, context)
        }
        ast::Statement::AlterIndex {
            if_exists,
            name,
            operation,
        } => execute_alter_index(state, *if_exists, name, operation),
        ast::Statement::Drop {
            object_type: ast::ObjectType::View,
            names,
            if_exists,
            cascade,
            ..
        } => execute_drop_views(state, names, *if_exists, *cascade),
        ast::Statement::Drop {
            object_type: ast::ObjectType::Index,
            names,
            if_exists,
            cascade,
            restrict,
            ..
        } => execute_drop_indexes(state, names, *if_exists, *cascade, *restrict),
        ast::Statement::Drop {
            object_type: ast::ObjectType::Table,
            names,
            if_exists,
            cascade,
            restrict,
            ..
        } => {
            if *cascade || *restrict {
                return reject_unsupported(
                    "DROP TABLE with CASCADE or RESTRICT is not implemented",
                );
            }
            let mut table_names = Vec::new();
            let mut seen = BTreeSet::new();
            for object in names {
                let table_name = normalize_relation_name(object)?;
                match state.catalog.require_named_table(&table_name) {
                    Ok(table) if seen.insert(table.id) => table_names.push(table_name),
                    Ok(_) => {}
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedTable => {}
                    Err(error) => return Err(error),
                }
            }
            for schema in state.catalog.drop_named_tables(&table_names)? {
                state.catalog.drop_owned_sequences(schema.id);
            }
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::Drop {
            object_type: ast::ObjectType::Sequence,
            names,
            if_exists,
            cascade,
            restrict,
            ..
        } => {
            if *cascade || *restrict {
                return reject_unsupported(
                    "DROP SEQUENCE with CASCADE or RESTRICT is not implemented",
                );
            }
            for object in names {
                let name = normalize_relation_name(object)?;
                if let Ok(sequence) = state.catalog.require_named_sequence(&name)
                    && state.catalog.has_dependent_views_for_sequence(sequence.id)
                {
                    return Err(PgError::create(
                        SqlState::DependentObjectsStillExist,
                        "cannot drop sequence because a view depends on it",
                    ));
                }
                match state.catalog.drop_named_sequence(&name) {
                    Ok(_) => {}
                    Err(error) if *if_exists && error.sqlstate == SqlState::UndefinedTable => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(StatementResult::Affected(0))
        }
        ast::Statement::Insert(insert) => execute_insert(
            state,
            insert,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
        ),
        ast::Statement::Update(update) => {
            if update.or.is_some() {
                return reject_unsupported("UPDATE feature is not implemented");
            }
            execute_update(
                state,
                update,
                xid,
                snapshot,
                deferred_constraints,
                defer_all,
                context,
                mutation_targets,
            )
        }
        ast::Statement::Delete(delete) => execute_delete(
            state,
            delete,
            xid,
            snapshot,
            deferred_constraints,
            defer_all,
            context,
            mutation_targets,
        ),
        ast::Statement::Query(query) => query::execute_query(state, query, xid, snapshot, context),
        ast::Statement::Lock(_) => Ok(StatementResult::Affected(0)),
        _ => reject_unsupported("statement is not implemented"),
    }
}

fn find_first_referenced_column(expression: &ast::Expr, columns: &[ColumnDef]) -> Option<String> {
    let mut found = None;
    let _ = ast::visit_expressions(expression, |nested| {
        let name = match nested {
            ast::Expr::Identifier(identifier) => Some(normalize_identifier(identifier)),
            ast::Expr::CompoundIdentifier(identifiers) => {
                identifiers.last().map(normalize_identifier)
            }
            _ => None,
        };
        if let Some(name) = name
            && columns.iter().any(|column| column.name == name)
        {
            found = Some(name);
            return std::ops::ControlFlow::Break(());
        }
        std::ops::ControlFlow::Continue(())
    });
    found
}

fn generate_constraint_name(base: String, constraints: &[crate::catalog::Constraint]) -> String {
    let mut suffix = 0;
    loop {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}{suffix}")
        };
        if !constraints
            .iter()
            .any(|constraint| constraint.get_name() == Some(&name))
        {
            return name;
        }
        suffix += 1;
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn create_generated_sequence_name(
    catalog: &Catalog,
    sequences: &[SequenceSchema],
    table_name: &ResolvedRelationName,
    column_name: &str,
) -> String {
    let base = format!("{}_{column_name}_seq", table_name.name);
    let mut number = 0;
    loop {
        let name = if number == 0 {
            base.clone()
        } else {
            format!("{base}{number}")
        };
        if !catalog.has_resolved_relation(&ResolvedRelationName {
            schema_id: table_name.schema_id,
            name: name.clone(),
        }) && !sequences.iter().any(|sequence| sequence.name == name)
        {
            return name;
        }
        number += 1;
    }
}

fn extract_default_sequence_name(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Nested(expr) => extract_default_sequence_name(expr),
        ast::Expr::Function(function)
            if normalize_unqualified_object_name(&function.name).as_deref() == Ok("nextval") =>
        {
            let ast::FunctionArguments::List(arguments) = &function.args else {
                return None;
            };
            let [ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))] =
                arguments.args.as_slice()
            else {
                return None;
            };
            extract_default_sequence_literal(argument)
        }
        _ => None,
    }
}

fn extract_default_sequence_literal(expression: &ast::Expr) -> Option<&str> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => {
            extract_default_sequence_literal(expr)
        }
        ast::Expr::Value(value) => match &value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_default_sequence(
    catalog: &Catalog,
    expression: &ast::Expr,
    persistence: TablePersistence,
) -> Result<Option<ResolvedRelationName>> {
    let Some(name) = extract_default_sequence_name(expression) else {
        let mut contains_sequence_call = false;
        let _ = ast::visit_expressions(expression, |nested| {
            let ast::Expr::Function(function) = nested else {
                return std::ops::ControlFlow::Continue(());
            };
            if normalize_unqualified_object_name(&function.name)
                .is_ok_and(|name| matches!(name.as_str(), "nextval" | "currval" | "setval"))
            {
                contains_sequence_call = true;
                return std::ops::ControlFlow::Break(());
            }
            std::ops::ControlFlow::Continue(())
        });
        if contains_sequence_call {
            return reject_unsupported("compound sequence defaults are not implemented");
        }
        return Ok(None);
    };
    let name = sequences::normalize_sequence_name(name)?;
    let sequence = catalog.require_named_sequence(&name).map_err(|error| {
        if error.sqlstate == SqlState::WrongObjectType {
            PgError::create(
                SqlState::FeatureNotSupported,
                "sequence defaults bound to non-sequence relations are not implemented",
            )
        } else {
            error
        }
    })?;
    let temporary_table = matches!(persistence, TablePersistence::Temporary { .. });
    let temporary_sequence = catalog.get_schema_name(sequence.schema_id) == TEMP_SCHEMA;
    if temporary_table != temporary_sequence {
        return reject_unsupported("cross-persistence sequence defaults are not implemented");
    }
    Ok(Some(ResolvedRelationName {
        schema_id: sequence.schema_id,
        name: sequence.name.clone(),
    }))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn normalize_unqualified_object_name(name: &ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return reject_unsupported("schemas are not implemented");
    }
    let Some(identifier) = name.0[0].as_ident() else {
        return reject_unsupported("dynamic object names are not implemented");
    };
    Ok(normalize_identifier(identifier))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn normalize_relation_name(name: &ast::ObjectName) -> Result<RelationName> {
    match name.0.as_slice() {
        [name] => {
            let Some(name) = name.as_ident() else {
                return reject_unsupported("dynamic object names are not implemented");
            };
            Ok(RelationName::create_unqualified(normalize_identifier(name)))
        }
        [schema, name] => {
            let Some(schema) = schema.as_ident() else {
                return reject_unsupported("dynamic object names are not implemented");
            };
            let Some(name) = name.as_ident() else {
                return reject_unsupported("dynamic object names are not implemented");
            };
            Ok(RelationName::create(
                Some(normalize_identifier(schema)),
                normalize_identifier(name),
            ))
        }
        _ => reject_unsupported("database-qualified relation names are not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn resolve_insert_table_name(table: &ast::TableObject) -> Result<RelationName> {
    let ast::TableObject::TableName(table_name) = table else {
        return reject_unsupported("insert target is not a table");
    };
    normalize_relation_name(table_name)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn normalize_identifier(identifier: &ast::Ident) -> String {
    if identifier.quote_style.is_some() {
        identifier.value.clone()
    } else {
        identifier.value.to_ascii_lowercase()
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_order_ascending(options: &ast::OrderByOptions) -> Result<bool> {
    match &options.sort {
        None | Some(ast::OrderBySort::Asc) => Ok(true),
        Some(ast::OrderBySort::Desc) => Ok(false),
        Some(ast::OrderBySort::Using(_)) => reject_unsupported("ORDER BY USING is not implemented"),
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn resolve_index_column_name(column: &ast::IndexColumn) -> Result<String> {
    let ast::Expr::Identifier(identifier) = &column.column.expr else {
        return reject_unsupported("index expressions are not implemented");
    };
    Ok(normalize_identifier(identifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn compares_all_phase_one_value_types() {
        let pairs = [
            (Value::Bool(false), Value::Bool(true)),
            (Value::Int2(1), Value::Int2(2)),
            (Value::Int4(1), Value::Int4(2)),
            (Value::Int8(1), Value::Int8(2)),
            (Value::Float4(1.0), Value::Float4(2.0)),
            (Value::Float8(1.0), Value::Float8(2.0)),
            (
                Value::Numeric("1".parse().unwrap()),
                Value::Numeric("2".parse().unwrap()),
            ),
            (Value::Text("a".into()), Value::Text("b".into())),
            (Value::Bytea(vec![1]), Value::Bytea(vec![2])),
        ];

        for (lower, higher) in pairs {
            assert_eq!(compare_values(&lower, &higher).unwrap(), Ordering::Less);
            assert_eq!(compare_values(&higher, &lower).unwrap(), Ordering::Greater);
        }

        assert_eq!(
            compare_values(&Value::Float4(f32::NAN), &Value::Float4(1.0)).unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            compare_values(&Value::Float8(f64::NAN), &Value::Float8(1.0)).unwrap(),
            Ordering::Greater
        );
    }
}
