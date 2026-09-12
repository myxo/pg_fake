use super::{
    DatabaseState, SequenceValueState, normalize_identifier, normalize_relation_name, sequences,
};
use crate::{
    StatementResult,
    catalog::TEMP_SCHEMA,
    error::{PgError, Result, SqlState, reject_unsupported},
};
use sqlparser::ast;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_create_sequence(
    state: &mut DatabaseState,
    temporary: bool,
    if_not_exists: bool,
    name: &ast::ObjectName,
    data_type: Option<&ast::DataType>,
    sequence_options: &[ast::SequenceOptions],
    owned_by: Option<&ast::ObjectName>,
) -> Result<StatementResult> {
    let relation_name = normalize_relation_name(name)?;
    let temporary = temporary || relation_name.schema.as_deref() == Some(TEMP_SCHEMA);
    let resolved_name = state
        .catalog
        .resolve_creation_name(&relation_name, temporary)?;
    if if_not_exists && state.catalog.has_resolved_relation(&resolved_name) {
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
    let mut sequence =
        sequences::create_sequence_schema(resolved_name.name.clone(), data_type, sequence_options)?;
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

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(super) fn execute_drop_sequences(
    state: &mut DatabaseState,
    names: &[ast::ObjectName],
    if_exists: bool,
    cascade: bool,
    restrict: bool,
) -> Result<StatementResult> {
    if cascade || restrict {
        return reject_unsupported("DROP SEQUENCE with CASCADE or RESTRICT is not implemented");
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
            Err(error) if if_exists && error.sqlstate == SqlState::UndefinedTable => {}
            Err(error) => return Err(error),
        }
    }
    Ok(StatementResult::Affected(0))
}
