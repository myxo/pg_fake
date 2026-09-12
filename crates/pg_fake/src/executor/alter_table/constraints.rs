use crate::executor::{
    DatabaseState,
    foreign_keys::{convert_referential_action, resolve_foreign_key_name},
    normalize_identifier, normalize_relation_name, resolve_index_column_name,
    table_ddl::{find_first_referenced_column, generate_constraint_name},
    validate_btree_key_type,
};
use crate::{
    catalog::{ForeignKey, TableSchema},
    error::{PgError, Result, SqlState, reject_unsupported},
};
use sqlparser::ast;

pub(super) fn create_table_constraint(
    state: &mut DatabaseState,
    schema: &TableSchema,
    constraint: &ast::TableConstraint,
    not_valid: bool,
) -> Result<crate::catalog::Constraint> {
    match constraint {
        ast::TableConstraint::PrimaryKey(primary_key) => {
            if not_valid {
                return reject_unsupported("PRIMARY KEY constraints cannot be marked NOT VALID");
            }
            let columns = primary_key
                .columns
                .iter()
                .map(resolve_index_column_name)
                .collect::<Result<Vec<_>>>()?;
            validate_constraint_columns(schema, &columns)?;
            for name in &columns {
                validate_btree_key_type(
                    schema
                        .columns
                        .iter()
                        .find(|column| column.name == *name)
                        .expect("validated constraint column must exist")
                        .data_type
                        .base,
                )?;
            }
            Ok(crate::catalog::Constraint::PrimaryKey {
                id: state.catalog.allocate_constraint_id(),
                name: primary_key
                    .name
                    .as_ref()
                    .map(normalize_identifier)
                    .unwrap_or_else(|| format!("{}_pkey", schema.name)),
                columns,
            })
        }
        ast::TableConstraint::Unique(unique) => {
            if not_valid {
                return reject_unsupported("UNIQUE constraints cannot be marked NOT VALID");
            }
            let columns = unique
                .columns
                .iter()
                .map(resolve_index_column_name)
                .collect::<Result<Vec<_>>>()?;
            validate_constraint_columns(schema, &columns)?;
            for name in &columns {
                validate_btree_key_type(
                    schema
                        .columns
                        .iter()
                        .find(|column| column.name == *name)
                        .expect("validated constraint column must exist")
                        .data_type
                        .base,
                )?;
            }
            Ok(crate::catalog::Constraint::Unique {
                id: state.catalog.allocate_constraint_id(),
                name: unique
                    .name
                    .as_ref()
                    .map(normalize_identifier)
                    .unwrap_or_else(|| format!("{}_{}_key", schema.name, columns.join("_"))),
                columns,
            })
        }
        ast::TableConstraint::Check(check) => {
            let base = find_first_referenced_column(&check.expr, &schema.columns).map_or_else(
                || format!("{}_check", schema.name),
                |column| format!("{}_{column}_check", schema.name),
            );
            Ok(crate::catalog::Constraint::Check {
                id: state.catalog.allocate_constraint_id(),
                name: check
                    .name
                    .as_ref()
                    .map(normalize_identifier)
                    .unwrap_or_else(|| generate_constraint_name(base, &schema.constraints)),
                expression: check.expr.clone(),
                validated: !not_valid,
            })
        }
        ast::TableConstraint::ForeignKey(foreign_key) => create_foreign_key_constraint(
            state,
            schema,
            foreign_key.name.as_ref(),
            foreign_key
                .columns
                .iter()
                .map(normalize_identifier)
                .collect(),
            foreign_key,
            !not_valid,
        ),
        constraint => reject_unsupported(format!(
            "ALTER TABLE constraint is not implemented: {constraint}"
        )),
    }
}

pub(super) fn create_foreign_key_constraint(
    state: &mut DatabaseState,
    schema: &TableSchema,
    name: Option<&ast::Ident>,
    columns: Vec<String>,
    foreign_key: &ast::ForeignKeyConstraint,
    validated: bool,
) -> Result<crate::catalog::Constraint> {
    validate_constraint_columns(schema, &columns)?;
    let foreign_table = normalize_relation_name(&foreign_key.foreign_table)?;
    let foreign_table_id = if foreign_table.name == schema.name
        && foreign_table.schema.as_deref().is_none_or(|name| {
            state
                .catalog
                .require_schema(name)
                .is_ok_and(|candidate| candidate.id == schema.schema_id)
        }) {
        schema.id
    } else {
        state.catalog.require_named_table(&foreign_table)?.id
    };
    let default_name = format!("{}_{}_fkey", schema.name, columns.join("_"));
    Ok(crate::catalog::Constraint::ForeignKey(ForeignKey {
        id: state.catalog.allocate_constraint_id(),
        name: resolve_foreign_key_name(name, default_name),
        columns,
        foreign_table,
        foreign_table_id,
        referred_columns: foreign_key
            .referred_columns
            .iter()
            .map(normalize_identifier)
            .collect(),
        on_delete: convert_referential_action(foreign_key.on_delete),
        on_update: convert_referential_action(foreign_key.on_update),
        deferrable: foreign_key
            .characteristics
            .is_some_and(|characteristics| characteristics.deferrable.unwrap_or(false)),
        initially_deferred: foreign_key.characteristics.is_some_and(|characteristics| {
            characteristics.initially == Some(ast::DeferrableInitial::Deferred)
        }),
        match_kind: foreign_key.match_kind,
        validated,
    }))
}

fn validate_constraint_columns(schema: &TableSchema, columns: &[String]) -> Result<()> {
    for name in columns {
        if !schema.columns.iter().any(|column| &column.name == name) {
            return Err(PgError::create(
                SqlState::UndefinedColumn,
                format!("column {name:?} does not exist"),
            ));
        }
    }
    Ok(())
}
