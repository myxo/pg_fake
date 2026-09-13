use sqlparser::{
    ast::{self, Spanned as _},
    tokenizer::Span,
};

use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor::{normalize_identifier, normalize_relation_name},
    txn::RowLockMode,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::executor) enum LockPolicy {
    Block,
    SkipLocked,
    NoWait,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SelectLock {
    pub(in crate::executor) mode: RowLockMode,
    pub(in crate::executor) policy: LockPolicy,
}

pub(in crate::executor) fn resolve_select_lock_mode(query: &ast::Query) -> Option<RowLockMode> {
    query
        .locks
        .iter()
        .map(|lock| match lock.lock_type {
            ast::LockType::KeyShare => RowLockMode::KeyShare,
            ast::LockType::Share => RowLockMode::Share,
            ast::LockType::NoKeyUpdate => RowLockMode::NoKeyUpdate,
            ast::LockType::Update => RowLockMode::Update,
        })
        .max()
}

pub(in crate::executor) fn resolve_query_lock_targets(
    catalog: &Catalog,
    query: &ast::Query,
    inherited: Option<SelectLock>,
    cte_names: &[String],
    cte_queries: &[ast::Query],
) -> Result<Vec<(Span, SelectLock)>> {
    let mut cte_names = cte_names.to_vec();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            resolve_query_lock_targets(catalog, &cte.query, None, &cte_names, cte_queries)?;
            cte_names.push(normalize_identifier(&cte.alias.name));
        }
    }
    let locking = inherited.is_some() || !query.locks.is_empty();
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        if let ast::SetExpr::Query(nested) = query.body.as_ref() {
            let inherited = query.locks.iter().fold(inherited, |current, lock| {
                Some(merge_select_locks(current, resolve_lock_clause(lock)))
            });
            return resolve_query_lock_targets(catalog, nested, inherited, &cte_names, cte_queries);
        }
        if matches!(query.body.as_ref(), ast::SetExpr::Values(_)) && query.locks.is_empty() {
            return Ok(Vec::new());
        }
        if locking {
            return reject_unsupported("row locking is not allowed with this query");
        }
        if let ast::SetExpr::SetOperation { left, right, .. } = query.body.as_ref() {
            for operand in [left, right] {
                let mut expression = operand.as_ref();
                while let ast::SetExpr::Query(query) = expression {
                    if !query.locks.is_empty() {
                        return reject_unsupported(
                            "row locking is not allowed in set-operation operands",
                        );
                    }
                    expression = &query.body;
                }
                let operand = super::set_operations::create_set_operand_query(query, operand);
                resolve_query_lock_targets(catalog, &operand, None, &cte_names, cte_queries)?;
            }
        }
        return Ok(Vec::new());
    };
    if locking
        && (select.distinct.is_some()
            || select.having.is_some()
            || !matches!(&select.group_by, ast::GroupByExpr::Expressions(expressions, modifiers) if expressions.is_empty() && modifiers.is_empty())
            || super::contains_query_aggregate(query))
    {
        return reject_unsupported(
            "row locking is not allowed with grouping, aggregates, or DISTINCT",
        );
    }
    struct WindowDetector {
        depth: usize,
        found: bool,
    }
    impl ast::Visitor for WindowDetector {
        type Break = ();
        fn pre_visit_query(&mut self, _: &ast::Query) -> std::ops::ControlFlow<()> {
            self.depth += 1;
            std::ops::ControlFlow::Continue(())
        }
        fn post_visit_query(&mut self, _: &ast::Query) -> std::ops::ControlFlow<()> {
            self.depth -= 1;
            std::ops::ControlFlow::Continue(())
        }
        fn pre_visit_expr(&mut self, expression: &ast::Expr) -> std::ops::ControlFlow<()> {
            self.found |= self.depth == 1
                && matches!(expression, ast::Expr::Function(function) if function.over.is_some());
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut detector = WindowDetector {
        depth: 0,
        found: false,
    };
    let _ = ast::Visit::visit(query, &mut detector);
    if locking && detector.found {
        return reject_unsupported("row locking is not allowed with window functions");
    }
    let mut sources = Vec::new();
    for table in &select.from {
        collect_lock_sources(table, false, &mut sources);
    }
    for lock in &query.locks {
        for name in &lock.of {
            if name.0.len() != 1 {
                return Err(PgError::create(
                    SqlState::SyntaxError,
                    "row-lock relation name must be unqualified",
                ));
            }
            let name = normalize_relation_name(name)?.name;
            if !sources
                .iter()
                .any(|(factor, _)| resolve_source_name(factor).as_ref() == Some(&name))
            {
                return Err(PgError::create(
                    SqlState::UndefinedTable,
                    format!("relation {name} in FOR clause not found in FROM clause"),
                ));
            }
        }
    }
    let mut targets = Vec::new();
    for (source, nullable) in sources {
        let name = resolve_source_name(source);
        let mut selected = inherited;
        let mut explicit = false;
        for lock in &query.locks {
            if lock.of.is_empty()
                || lock.of.iter().any(|target| {
                    normalize_relation_name(target).ok().map(|name| name.name) == name
                })
            {
                selected = Some(merge_select_locks(selected, resolve_lock_clause(lock)));
                explicit |= !lock.of.is_empty();
            }
        }
        let is_cte = matches!(source, ast::TableFactor::Table { name, .. }
            if name.0.len() == 1 && normalize_relation_name(name).is_ok_and(|name| cte_names.contains(&name.name)));
        if selected.is_some() && is_cte {
            if explicit {
                return reject_unsupported("row locking cannot be applied to a WITH query");
            }
            continue;
        }
        if selected.is_some() && nullable {
            return reject_unsupported(
                "row locking cannot be applied to the nullable side of an outer join",
            );
        }
        match source {
            ast::TableFactor::Derived { subquery, .. } => {
                if cte_queries.contains(subquery.as_ref()) {
                    continue;
                }
                resolve_query_lock_targets(catalog, subquery, selected, &cte_names, cte_queries)?;
            }
            ast::TableFactor::Table {
                name, args: None, ..
            } if !is_cte => {
                if let Ok(view) = catalog.require_named_view(&normalize_relation_name(name)?) {
                    resolve_query_lock_targets(catalog, &view.query, selected, &[], cte_queries)?;
                }
            }
            _ if selected.is_some() => {
                if explicit {
                    return reject_unsupported("row locking cannot be applied to this FROM source");
                }
                continue;
            }
            _ => {}
        }
        if let Some(selected) = selected {
            targets.push((source.span(), selected));
        }
    }
    Ok(targets)
}

fn resolve_source_name(factor: &ast::TableFactor) -> Option<String> {
    match factor {
        ast::TableFactor::Table { name, alias, .. } => alias
            .as_ref()
            .map(|alias| normalize_identifier(&alias.name))
            .or_else(|| normalize_relation_name(name).ok().map(|name| name.name)),
        ast::TableFactor::Derived { alias, .. } | ast::TableFactor::NestedJoin { alias, .. } => {
            alias
                .as_ref()
                .map(|alias| normalize_identifier(&alias.name))
        }
        _ => None,
    }
}

fn collect_lock_sources<'a>(
    table: &'a ast::TableWithJoins,
    nullable: bool,
    sources: &mut Vec<(&'a ast::TableFactor, bool)>,
) {
    let start = sources.len();
    match &table.relation {
        ast::TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            sources.push((&table.relation, nullable));
            collect_lock_sources(table_with_joins, nullable, sources);
        }
        source => sources.push((source, nullable)),
    }
    for join in &table.joins {
        if matches!(
            join.join_operator,
            ast::JoinOperator::Right(_)
                | ast::JoinOperator::RightOuter(_)
                | ast::JoinOperator::FullOuter(_)
        ) {
            for (_, nullable) in &mut sources[start..] {
                *nullable = true;
            }
        }
        let nullable = nullable
            || matches!(
                join.join_operator,
                ast::JoinOperator::Left(_)
                    | ast::JoinOperator::LeftOuter(_)
                    | ast::JoinOperator::FullOuter(_)
            );
        match &join.relation {
            ast::TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                sources.push((&join.relation, nullable));
                collect_lock_sources(table_with_joins, nullable, sources);
            }
            source => sources.push((source, nullable)),
        }
    }
}

fn resolve_lock_clause(lock: &ast::LockClause) -> SelectLock {
    SelectLock {
        mode: match lock.lock_type {
            ast::LockType::KeyShare => RowLockMode::KeyShare,
            ast::LockType::Share => RowLockMode::Share,
            ast::LockType::NoKeyUpdate => RowLockMode::NoKeyUpdate,
            ast::LockType::Update => RowLockMode::Update,
        },
        policy: match lock.nonblock {
            Some(ast::NonBlock::Nowait) => LockPolicy::NoWait,
            Some(ast::NonBlock::SkipLocked) => LockPolicy::SkipLocked,
            None => LockPolicy::Block,
        },
    }
}

fn merge_select_locks(current: Option<SelectLock>, requested: SelectLock) -> SelectLock {
    current.map_or(requested, |current| SelectLock {
        mode: current.mode.max(requested.mode),
        policy: current.policy.max(requested.policy),
    })
}

pub(in crate::executor) fn contains_row_locks(value: &impl ast::Visit) -> bool {
    struct LockDetector(bool);
    impl ast::Visitor for LockDetector {
        type Break = ();
        fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<()> {
            self.0 |= !query.locks.is_empty();
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut detector = LockDetector(false);
    let _ = value.visit(&mut detector);
    detector.0
}

pub(in crate::executor) fn requires_nested_locking(query: &ast::Query) -> bool {
    struct BarrierDetector(bool);
    impl ast::Visitor for BarrierDetector {
        type Break = ();
        fn pre_visit_query(&mut self, query: &ast::Query) -> std::ops::ControlFlow<()> {
            self.0 |=
                query.limit_clause.is_some() || !query.locks.is_empty() || query.with.is_some();
            std::ops::ControlFlow::Continue(())
        }
    }
    let mut detector = BarrierDetector(false);
    let _ = ast::Visit::visit(query, &mut detector);
    detector.0
}

pub(super) fn refresh_locked_source(
    targets: Option<&[(Span, SelectLock)]>,
    state: &crate::executor::DatabaseState,
    row: &mut crate::executor::from::SourceRow,
    xid: crate::txn::Xid,
    snapshot: &crate::txn::Snapshot,
    context: &crate::executor::StatementContext,
) -> Result<Option<bool>> {
    let mut changed = false;
    for origin in &mut row.origins {
        if targets
            .is_some_and(|targets| !targets.iter().any(|(source, _)| *source == origin.source))
        {
            continue;
        }
        if !state
            .row_locks
            .is_held(origin.key, xid, RowLockMode::KeyShare)
        {
            continue;
        }
        let table = state
            .tables
            .get(&origin.key.table_id)
            .expect("row origin table exists");
        let version = table
            .iterate_version_chains()
            .find(|(row_id, _)| *row_id == origin.key.row_id)
            .and_then(|(_, chain)| {
                crate::txn::find_visible_version(chain, snapshot, xid, &state.transactions)
            });
        let Some(version) = version else {
            return Ok(None);
        };
        if version.xmin == origin.version_xmin {
            continue;
        }
        let values = if let Some(projection) = &origin.projection {
            let mut source = projection.source.clone();
            if refresh_locked_source(None, state, &mut source, xid, snapshot, context)?.is_none() {
                return Ok(None);
            }
            let scope = crate::executor::scope::bind_select_scope(state, &projection.select)?;
            if !crate::executor::from::recheck_join_conditions(
                state,
                &projection.select,
                &scope,
                &mut source,
                xid,
                snapshot,
                context,
            )? {
                return Ok(None);
            }
            if !super::select::evaluate_where_clause(
                state,
                projection.select.selection.as_ref(),
                &scope,
                &source.values,
                xid,
                snapshot,
                context,
            )? {
                return Ok(None);
            }
            let (projections, _) =
                super::build_projection_plan(state, &projection.select.projection, &scope)?;
            super::evaluate_projection_values(
                state,
                &projections,
                &scope,
                &source.values,
                None,
                xid,
                snapshot,
                context,
            )?
        } else {
            version.row.clone()
        };
        let start = origin.start.expect("row source has a bound slot");
        row.values[start..start + values.len()].clone_from_slice(&values);
        origin.version_xmin = version.xmin;
        changed = true;
    }
    Ok(Some(changed))
}
