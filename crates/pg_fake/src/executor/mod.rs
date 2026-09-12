use bigdecimal::ToPrimitive;

use crate::{
    ColumnMeta, StatementResult,
    catalog::{
        Catalog, ColumnDef, ConstraintId, ForeignKey, ForeignKeyAction, IdentityKind,
        IndexColumnDefinition, IndexSchema, RelationName, ResolvedRelationName, TableId,
        TablePersistence, TableSchema,
    },
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    storage::RowId,
    txn::{Snapshot, Xid, find_visible_version},
    value::{BaseType, DAYS_PER_MONTH, MICROSECONDS_PER_DAY, PgType, Value},
};
use sqlparser::ast;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub(crate) use crate::database::DatabaseState;

mod aggregates;
mod alter_table;
mod arithmetic;
mod column_defaults;
mod context;
mod ctes;
mod equality;
mod expressions;
mod foreign_keys;
mod from;
mod indexes;
mod json;
pub(crate) use context::{
    PreparedConflictUpdate, PreparedInsert, PreparedMutationTarget, PreparedUpdateRow,
    StatementContext,
};
pub(crate) use json::{
    JsonTableFunction, extract_json_table_function, resolve_json_function_arguments,
    resolve_json_operator_types,
};
mod locks;
mod outer_references;
mod prepared;
mod procedural;
mod query;
mod row_constraints;
mod scope;
pub(crate) use scope::{bind_join, bind_table_factor};
mod sequence_ddl;
mod sequences;
mod subqueries;
mod table_ddl;
mod views;
mod writes;

use aggregates::{
    AggregateDescriptor, AggregateInput, AggregateState, is_aggregate_function,
    parse_aggregate_call,
};
use arithmetic::{
    evaluate_boolean_operator, evaluate_numeric_operator, evaluate_temporal_arithmetic,
};
use column_defaults::{evaluate_column_default, validate_column_default};
use expressions::{
    compare_values, evaluate, evaluate_and_coerce, evaluate_assignment_expression,
    evaluate_comparison, validate_equality_type, validate_ordering_type,
};
pub(crate) use expressions::{
    create_constant_expression_schema, extract_unknown_string_literal, infer_expression_type,
    is_null_literal,
};
pub(crate) use foreign_keys::{contains_deferred_foreign_keys, validate_deferred_foreign_keys};
use foreign_keys::{
    convert_referential_action, resolve_foreign_key_name, validate_foreign_key_definitions,
    validate_row_foreign_keys,
};
pub(crate) use indexes::evaluate_index_predicate;
use indexes::{execute_alter_index, execute_create_index, execute_drop_indexes};
pub(crate) use locks::{
    MutationCandidate, RequiredRowLock, collect_required_cte_row_locks, collect_required_row_locks,
    mutation_locks_cover_targets,
};
use row_constraints::{
    validate_check_constraint_types, validate_check_constraints, validate_not_null,
};

fn validate_btree_key_type(data_type: BaseType) -> Result<()> {
    if data_type == BaseType::Json {
        Err(PgError::create(
            SqlState::UndefinedObject,
            "data type json has no default operator class for access method btree",
        ))
    } else {
        Ok(())
    }
}
pub(crate) use prepared::{PreparedQueryPlan, build_prepared_query_plan, execute_prepared_query};
pub(crate) use procedural::coerce_procedural_value;
pub(crate) use scope::infer_query_output_columns;
pub(crate) use scope::{
    BoundScope, RowScope, bind_from_scope, bind_query_scope, bind_target_scope,
    combine_bound_scopes, create_value_scope, identify_unknown_query_columns,
    identify_unknown_set_operand_columns, substitute_typed_subqueries,
};
pub(crate) use sequences::{
    SequenceExecutionContext, SequenceSessionState, SequenceSessionStorage, SequenceStorage,
    SequenceValueState,
};
use table_ddl::{
    create_generated_sequence_name, find_first_referenced_column, generate_constraint_name,
    resolve_default_sequence,
};
use views::{execute_comment_on_view, execute_create_view, execute_drop_views};
use writes::{execute_delete, execute_insert, execute_update};

pub(crate) use ctes::{expand_ctes_for_analysis, materialize_statement_ctes};
pub(crate) use procedural::substitute_procedural_references;
pub(crate) use query::describe_query_result_columns;
pub(crate) use query::detect_statement_features;
pub(crate) use sequences::normalize_sequence_name;
pub(crate) use subqueries::materialize_uncorrelated_subqueries;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn execute_statement(
    state: &mut DatabaseState,
    statement: &ast::Statement,
    xid: Xid,
    snapshot: &Snapshot,
    deferred_constraints: &BTreeSet<ConstraintId>,
    defer_all: bool,
    context: &StatementContext,
    mutation_targets: Option<Vec<RequiredRowLock>>,
) -> Result<StatementResult> {
    context.check_timeout()?;
    match statement {
        ast::Statement::CreateTable(create) => table_ddl::execute_create_table(state, create),
        ast::Statement::CreateSequence {
            temporary,
            if_not_exists,
            name,
            data_type,
            sequence_options,
            owned_by,
        } => sequence_ddl::execute_create_sequence(
            state,
            *temporary,
            *if_not_exists,
            name,
            data_type.as_ref(),
            sequence_options,
            owned_by.as_ref(),
        ),
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
        } => procedural::execute_alter_trigger(state, name, table_name, new_name),
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
        } => table_ddl::execute_drop_tables(state, names, *if_exists, *cascade, *restrict),
        ast::Statement::Drop {
            object_type: ast::ObjectType::Sequence,
            names,
            if_exists,
            cascade,
            restrict,
            ..
        } => sequence_ddl::execute_drop_sequences(state, names, *if_exists, *cascade, *restrict),
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

fn create_relation_object_name(name: RelationName) -> ast::ObjectName {
    let mut parts = Vec::with_capacity(2);
    if let Some(schema) = name.schema {
        parts.push(ast::ObjectNamePart::Identifier(ast::Ident::with_quote(
            '"', schema,
        )));
    }
    parts.push(ast::ObjectNamePart::Identifier(ast::Ident::with_quote(
        '"', name.name,
    )));
    ast::ObjectName(parts)
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
#[path = "mod_test.rs"]
mod tests;
