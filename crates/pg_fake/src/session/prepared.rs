use sqlparser::ast;

use crate::{
    analyzer,
    error::{PgError, Result, SqlState, reject_unsupported},
    executor, parser,
    txn::{RelationLockMode, Snapshot},
    value::Value,
};

use super::{
    ColumnMeta, QueryResult, Session, SessionTransactionState, StatementResult,
    catalog_dependencies::{CatalogDependency, collect_catalog_dependencies},
    contains_sequence_function,
    locking::collect_relation_locks,
};

#[derive(Debug, Clone)]
pub struct PreparedStatement {
    pub(super) statement: ast::Statement,
    search_path: Vec<String>,
    source_sql: String,
    pub(super) literal_timezone: String,
    replanned: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<PreparedStatement>>>>,
    pub(super) parameter_types: Vec<crate::value::BaseType>,
    pub(super) columns: Vec<ColumnMeta>,
    pub(super) query_plan: Option<executor::PreparedQueryPlan>,
    pub(super) catalog_dependencies: Vec<CatalogDependency>,
    pub(super) catalog_identity: crate::catalog::CatalogIdentity,
    pub(super) relation_locks: Option<Vec<(String, RelationLockMode)>>,
}

impl Session {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        let statement = self.prepare(sql)?;
        self.execute_prepared(&statement, params)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let statement = self.prepare(sql)?;
        self.query_prepared(&statement, params)
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn prepare(&mut self, sql: &str) -> Result<PreparedStatement> {
        self.prepare_with_parameter_types(sql, &[])
    }

    pub fn prepare_with_parameter_types(
        &mut self,
        sql: &str,
        parameter_types: &[Option<crate::value::BaseType>],
    ) -> Result<PreparedStatement> {
        let mut statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => {
                return self.abort_with_error(error);
            }
        };
        if statements.len() != 1 {
            return self.abort_with_error(PgError::create(
                SqlState::SyntaxError,
                "prepared statements require exactly one statement",
            ));
        }
        let mut statement = statements.pop().expect("statement count was checked");
        if let Err(error) = executor::resolve_statement_windows(&mut statement) {
            return self.abort_with_error(error);
        }
        let parameter_count = match analyzer::count_parameters(&statement) {
            Ok(count) => count,
            Err(error) => return self.abort_with_error(error),
        };
        let _ = ast::visit_expressions_mut(&mut statement, |expression| {
            if let ast::Expr::Value(value) = expression
                && let ast::Value::Placeholder(placeholder) = &value.value
            {
                let index = analyzer::parse_placeholder_index(placeholder)
                    .expect("parameter indices were validated");
                if let Some(Some(base)) = parameter_types.get(index) {
                    *expression = analyzer::create_typed_cast(
                        expression.clone(),
                        crate::value::PgType::create(*base),
                    );
                }
            }
            std::ops::ControlFlow::<()>::Continue(())
        });
        if matches!(statement, ast::Statement::CreateView(_)) && parameter_count != 0 {
            return self.abort_with_error(PgError::create(
                SqlState::UndefinedParameter,
                "there is no parameter in CREATE VIEW",
            ));
        }
        if matches!(
            self.transaction,
            Some(SessionTransactionState::Aborted { .. })
        ) && !matches!(
            &statement,
            ast::Statement::Commit { .. } | ast::Statement::Rollback { .. }
        ) {
            return Err(PgError::create(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        let frozen = ast::visit_expressions_mut(&mut statement, |expression| {
            let literal = match &*expression {
                ast::Expr::TypedString(typed) if !typed.uses_odbc_syntax => {
                    Some((typed.data_type.clone(), typed.value.value.clone()))
                }
                ast::Expr::Cast {
                    expr, data_type, ..
                } => crate::executor::extract_unknown_string_literal(expr).map(|text| {
                    (
                        data_type.clone(),
                        ast::Value::SingleQuotedString(text.to_owned()),
                    )
                }),
                _ => None,
            };
            if let Some((
                ast::DataType::Timestamp(
                    precision,
                    ast::TimezoneInfo::WithTimeZone | ast::TimezoneInfo::Tz,
                ),
                ast::Value::SingleQuotedString(text),
            )) = literal
            {
                let data_type =
                    ast::DataType::Timestamp(precision, ast::TimezoneInfo::WithTimeZone);
                let value = crate::coercion::convert_ast_data_type(&data_type).and_then(|target| {
                    crate::coercion::coerce_unknown(
                        &text,
                        target,
                        crate::coercion::CastContext::Explicit,
                        &self.settings.timezone,
                    )
                    .map(|value| analyzer::create_typed_literal(value, target))
                });
                match value {
                    Ok(value) => *expression = value,
                    Err(error) => return std::ops::ControlFlow::Break(error),
                }
            }
            std::ops::ControlFlow::Continue(())
        });
        if let std::ops::ControlFlow::Break(error) = frozen {
            return self.abort_with_error(error);
        }
        let setting_columns = match self.describe_setting_statement(&statement) {
            Ok(columns) => columns,
            Err(error) => return self.abort_with_error(error),
        };
        let prepared = {
            let mut state = self.db.state.lock().expect("database mutex is poisoned");
            let (xid, snapshot) = match self.transaction {
                Some(SessionTransactionState::Active(transaction)) => (
                    Some(transaction.xid),
                    transaction
                        .snapshot
                        .unwrap_or_else(|| Snapshot::create(&state.transactions))
                        .use_command(crate::txn::CommandId(transaction.next_command_id)),
                ),
                Some(SessionTransactionState::Aborted { transaction }) => (
                    Some(transaction.xid),
                    Snapshot::create(&state.transactions)
                        .use_command(crate::txn::CommandId(transaction.next_command_id)),
                ),
                None => (None, Snapshot::create(&state.transactions)),
            };
            state.load_catalog(xid, snapshot, Some(self.temporary_schema_id));
            state.catalog.set_search_path(&self.settings.search_path);
            analyzer::count_parameters(&statement)
                .and_then(|parameter_count| {
                    let parameter_count = parameter_count.max(parameter_types.len());
                    executor::expand_ctes_for_analysis(&statement, &state)
                        .map(|(statement, mutations)| (statement, mutations, parameter_count))
                })
                .and_then(|(statement, mutations, parameter_count)| {
                    let catalog_dependencies = collect_catalog_dependencies(
                        &state.catalog,
                        std::iter::once(statement.as_ref()).chain(mutations.iter()),
                    )?;
                    analyzer::substitute_typed_subqueries(&statement, &state.catalog).map(
                        |statement| (statement, mutations, parameter_count, catalog_dependencies),
                    )
                })
                .and_then(
                    |(statement, mutations, parameter_count, catalog_dependencies)| {
                        mutations
                            .iter()
                            .map(|mutation| {
                                analyzer::substitute_typed_subqueries(mutation, &state.catalog)
                            })
                            .collect::<Result<Vec<_>>>()
                            .map(|mutations| {
                                (statement, mutations, parameter_count, catalog_dependencies)
                            })
                    },
                )
                .and_then(
                    |(described, mutations, parameter_count, catalog_dependencies)| {
                        analyzer::analyze_prepared_statement_parameters(
                            &described,
                            &mutations,
                            &state.catalog,
                            parameter_count,
                            parameter_types,
                        )
                        .and_then(|(parameter_types, described)| {
                            let columns = match &setting_columns {
                                Some(columns) => columns.clone(),
                                None => {
                                    executor::describe_query_result_columns(&state, &described)?
                                }
                            };
                            let query_plan = executor::build_prepared_query_plan(
                                &state,
                                &statement,
                                &parameter_types,
                                Some(&columns),
                            )?;
                            let relation_locks = if can_cache_read_locks(&statement)
                                && catalog_dependencies
                                    .iter()
                                    .all(|dependency| match dependency {
                                        CatalogDependency::View { schema, .. } => {
                                            can_cache_read_locks(&ast::Statement::Query(
                                                schema.query.clone(),
                                            ))
                                        }
                                        _ => true,
                                    }) {
                                collect_relation_locks(
                                    &state,
                                    &statement,
                                    Some(&catalog_dependencies),
                                )
                                .ok()
                            } else {
                                None
                            };
                            Ok((
                                parameter_types,
                                columns,
                                query_plan,
                                catalog_dependencies,
                                relation_locks,
                                state.catalog.create_identity(),
                            ))
                        })
                    },
                )
        };
        match prepared {
            Ok((
                parameter_types,
                columns,
                query_plan,
                catalog_dependencies,
                relation_locks,
                catalog_identity,
            )) => Ok(PreparedStatement {
                search_path: self.settings.search_path.clone(),
                source_sql: sql.into(),
                literal_timezone: self.settings.timezone.clone(),
                replanned: Default::default(),
                statement,
                parameter_types,
                columns,
                query_plan,
                catalog_dependencies,
                relation_locks,
                catalog_identity,
            }),
            Err(error) => self.abort_with_error(error),
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_prepared(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<u64> {
        match self.execute_prepared_statement(statement, params)? {
            StatementResult::Affected(rows) => Ok(rows),
            StatementResult::Query(_) => {
                reject_unsupported("use query_prepared for row-producing statements")
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn query_prepared(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<QueryResult> {
        match self.execute_prepared_statement(statement, params)? {
            StatementResult::Query(result) => Ok(result),
            StatementResult::Affected(_) => {
                reject_unsupported("query_prepared requires a row-producing statement")
            }
        }
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn execute_prepared_statement(
        &mut self,
        statement: &PreparedStatement,
        params: &[Value],
    ) -> Result<StatementResult> {
        let replan_cache = &statement.replanned;
        let cached = replan_cache
            .lock()
            .expect("prepared statement mutex is poisoned")
            .clone();
        let statement = cached.as_deref().unwrap_or(statement);
        if statement.search_path != self.settings.search_path {
            let types = statement
                .parameter_types
                .iter()
                .copied()
                .map(Some)
                .collect::<Vec<_>>();
            let refreshed = self.prepare_with_parameter_types(&statement.source_sql, &types)?;
            if refreshed.columns != statement.columns {
                return self.abort_with_error(PgError::create(
                    SqlState::FeatureNotSupported,
                    "cached plan must not change result type",
                ));
            }
            let refreshed = std::sync::Arc::new(refreshed);
            *replan_cache
                .lock()
                .expect("prepared statement mutex is poisoned") = Some(refreshed.clone());
            return self.execute_prepared_statement(&refreshed, params);
        }
        let parameters;
        let (bound_statement, prepared_query) = if let Some(query_plan) = &statement.query_plan {
            parameters = match analyzer::coerce_parameters(&statement.parameter_types, params) {
                Ok(parameters) => parameters,
                Err(error) => return self.abort_with_error(error),
            };
            (
                if statement.relation_locks.is_some() {
                    None
                } else {
                    Some(
                        match analyzer::bind_parameters(
                            &statement.statement,
                            &statement.parameter_types,
                            params,
                        ) {
                            Ok(statement) => statement,
                            Err(error) => return self.abort_with_error(error),
                        },
                    )
                },
                Some((
                    query_plan,
                    parameters.as_slice(),
                    statement.columns.as_slice(),
                )),
            )
        } else if statement.parameter_types.is_empty() && params.is_empty() {
            (None, None)
        } else {
            (
                Some(
                    match analyzer::bind_parameters(
                        &statement.statement,
                        &statement.parameter_types,
                        params,
                    ) {
                        Ok(statement) => statement,
                        Err(error) => return self.abort_with_error(error),
                    },
                ),
                None,
            )
        };
        let execution_statement = bound_statement.as_deref().unwrap_or(&statement.statement);
        let started_implicit_transaction = self.transaction.is_none();
        if started_implicit_transaction {
            self.start_transaction(self.settings.default_isolation, true);
        }
        match self.execute_statement(execution_statement, prepared_query, Some(statement), None) {
            Ok(result) => {
                if started_implicit_transaction && self.is_transaction_implicit_batch() {
                    self.commit_transaction()?;
                }
                Ok(result)
            }
            Err(error) => {
                if started_implicit_transaction && self.is_transaction_implicit_batch() {
                    let _ = self.rollback_transaction();
                }
                Err(error)
            }
        }
    }
}

impl PreparedStatement {
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_parameter_types(&self) -> &[crate::value::BaseType] {
        &self.parameter_types
    }

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    pub fn get_result_columns(&self) -> &[ColumnMeta] {
        &self.columns
    }
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn can_cache_read_locks(statement: &ast::Statement) -> bool {
    let only_queries = ast::visit_statements(statement, |statement| {
        if matches!(statement, ast::Statement::Query(_)) {
            std::ops::ControlFlow::Continue(())
        } else {
            std::ops::ControlFlow::Break(())
        }
    })
    .is_continue();
    only_queries && !contains_sequence_function(statement)
}
