use super::RewrittenRow;
use crate::executor::{
    DatabaseState, StatementContext,
    column_defaults::validate_column_default,
    expressions::evaluate_assignment_expression,
    normalize_identifier,
    sequences::{self, SequenceValueState},
    table_ddl::{create_generated_sequence_name, resolve_default_sequence},
    validate_btree_key_type, views,
};
use crate::{
    catalog::{ColumnDef, IdentityKind, ResolvedRelationName, TableSchema},
    coercion::{self, CastContext},
    error::{PgError, Result, SqlState, reject_unsupported},
    value::{BaseType, PgType},
};
use sqlparser::ast;

pub(super) fn create_column_definition(
    state: &mut DatabaseState,
    schema: &TableSchema,
    definition: &ast::ColumnDef,
) -> Result<ColumnDef> {
    let serial_type = match definition
        .data_type
        .to_string()
        .to_ascii_lowercase()
        .as_str()
    {
        "smallserial" | "serial2" => Some(BaseType::Int2),
        "serial" | "serial4" => Some(BaseType::Int4),
        "bigserial" | "serial8" => Some(BaseType::Int8),
        _ => None,
    };
    let data_type = match serial_type {
        Some(base) => PgType::create(base),
        None => coercion::convert_ast_data_type(&definition.data_type)?,
    };
    let mut nullable = true;
    let mut default = None;
    let mut default_sequence = None;
    let mut identity = None;
    let mut sequence_options = None;
    for option in &definition.options {
        match &option.option {
            ast::ColumnOption::Null => nullable = true,
            ast::ColumnOption::NotNull => nullable = false,
            ast::ColumnOption::Default(expression) => {
                if serial_type.is_some() || identity.is_some() {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "multiple default values specified for column",
                    ));
                }
                default = Some(expression.clone());
                default_sequence =
                    resolve_default_sequence(&state.catalog, expression, schema.persistence)?;
            }
            ast::ColumnOption::PrimaryKey(_)
            | ast::ColumnOption::Unique(_)
            | ast::ColumnOption::Check(_)
            | ast::ColumnOption::ForeignKey(_) => {}
            ast::ColumnOption::Generated {
                generated_as,
                sequence_options: options,
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
                identity = Some(match generated_as {
                    ast::GeneratedAs::Always => IdentityKind::Always,
                    ast::GeneratedAs::ByDefault => IdentityKind::ByDefault,
                    ast::GeneratedAs::ExpStored => {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "invalid identity column declaration",
                        ));
                    }
                });
                sequence_options = options.clone();
            }
            option => {
                return reject_unsupported(format!(
                    "ALTER TABLE column option is not implemented: {option}"
                ));
            }
        }
    }
    if identity.is_some()
        && !matches!(
            data_type.base,
            BaseType::Int2 | BaseType::Int4 | BaseType::Int8
        )
    {
        return Err(PgError::create(
            SqlState::DatatypeMismatch,
            "identity column type must be smallint, integer, or bigint",
        ));
    }
    if serial_type.is_some() || identity.is_some() {
        let column_name = normalize_identifier(&definition.name);
        let resolved_table = ResolvedRelationName {
            schema_id: schema.schema_id,
            name: schema.name.clone(),
        };
        let sequence_name =
            create_generated_sequence_name(&state.catalog, &[], &resolved_table, &column_name);
        let mut sequence = sequences::create_sequence_schema_for_type(
            sequence_name.clone(),
            data_type.base,
            sequence_options.as_deref().unwrap_or(&[]),
        )?;
        sequence.owned_by = Some((schema.id, column_name));
        let initial = SequenceValueState {
            last_value: sequence.start_value,
            is_called: false,
        };
        let id = state.catalog.create_named_sequence(
            ResolvedRelationName {
                schema_id: schema.schema_id,
                name: sequence_name.clone(),
            },
            sequence,
        )?;
        state
            .sequence_values
            .lock()
            .expect("sequence storage is poisoned")
            .insert(id, initial);
        nullable = false;
        default_sequence = Some(ResolvedRelationName {
            schema_id: schema.schema_id,
            name: sequence_name,
        });
    }
    Ok(ColumnDef {
        name: normalize_identifier(&definition.name),
        data_type,
        nullable,
        default,
        default_sequence,
        identity,
    })
}

pub(super) fn alter_column(
    state: &DatabaseState,
    schema: &mut TableSchema,
    rows: &mut [RewrittenRow],
    column_name: &ast::Ident,
    operation: &ast::AlterColumnOperation,
    context: &StatementContext,
) -> Result<()> {
    let name = normalize_identifier(column_name);
    let index = schema
        .columns
        .iter()
        .position(|column| column.name == name)
        .ok_or_else(|| {
            PgError::create(
                SqlState::UndefinedColumn,
                format!("column {name:?} does not exist"),
            )
        })?;
    match operation {
        ast::AlterColumnOperation::SetNotNull => schema.columns[index].nullable = false,
        ast::AlterColumnOperation::DropNotNull => {
            if schema.constraints.iter().any(|constraint| {
                matches!(constraint, crate::catalog::Constraint::PrimaryKey { columns, .. } if columns.contains(&name))
            }) {
                return Err(PgError::create(
                    SqlState::InvalidTableDefinition,
                    format!("column {name:?} is in a primary key"),
                ));
            }
            schema.columns[index].nullable = true;
        }
        ast::AlterColumnOperation::SetDefault { value } => {
            schema.columns[index].default_sequence =
                resolve_default_sequence(&state.catalog, value, schema.persistence)?;
            schema.columns[index].default = Some(value.clone());
            validate_column_default(&schema.columns[index])?;
        }
        ast::AlterColumnOperation::DropDefault => {
            schema.columns[index].default = None;
            schema.columns[index].default_sequence = None;
        }
        ast::AlterColumnOperation::SetDataType {
            data_type, using, ..
        } => {
            if views::has_view_column_dependency(&state.catalog, schema.id, &name) {
                return Err(PgError::create(
                    SqlState::FeatureNotSupported,
                    "cannot alter column type because a view depends on it",
                ));
            }
            let target = coercion::convert_ast_data_type(data_type)?;
            if schema.constraints.iter().any(|constraint| {
                matches!(
                    constraint,
                    crate::catalog::Constraint::PrimaryKey { columns, .. }
                        | crate::catalog::Constraint::Unique { columns, .. }
                        if columns.contains(&name)
                )
            }) || schema
                .indexes
                .iter()
                .any(|index| index.columns.iter().any(|column| column.name == name))
            {
                validate_btree_key_type(target.base)?;
            }
            let old_schema = schema.clone();
            let source = old_schema.columns[index].data_type.base;
            let values = rows
                .iter()
                .map(|row| match using {
                    Some(expression) => evaluate_assignment_expression(
                        expression,
                        target,
                        &old_schema,
                        &row.row,
                        context,
                    ),
                    None => coercion::coerce(
                        row.row[index].clone(),
                        source,
                        target,
                        CastContext::Assignment,
                    ),
                })
                .collect::<Result<Vec<_>>>()?;
            for (row, value) in rows.iter_mut().zip(values) {
                row.row[index] = value;
            }
            schema.columns[index].data_type = target;
            validate_column_default(&schema.columns[index])?;
        }
        ast::AlterColumnOperation::AddGenerated { .. } => {
            return reject_unsupported("ALTER TABLE ADD GENERATED is not implemented");
        }
    }
    Ok(())
}
