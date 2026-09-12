use crate::executor::{DatabaseState, normalize_identifier, views};
use crate::{
    catalog::{ForeignKey, TableId, TableSchema},
    error::{PgError, Result, SqlState},
};
use sqlparser::ast;

pub(super) enum ForeignKeyTarget<'a> {
    Constraint {
        columns: &'a [String],
        primary_key: bool,
    },
    Column {
        name: &'a str,
        primary_columns: &'a [String],
    },
}

impl ForeignKeyTarget<'_> {
    fn matches_foreign_key(&self, foreign_key: &ForeignKey, table_id: TableId) -> bool {
        if foreign_key.foreign_table_id != table_id {
            return false;
        }
        match self {
            Self::Constraint {
                columns,
                primary_key,
            } => {
                (foreign_key.referred_columns.is_empty() && *primary_key)
                    || foreign_key.referred_columns.as_slice() == *columns
            }
            Self::Column {
                name,
                primary_columns,
            } => {
                foreign_key
                    .referred_columns
                    .iter()
                    .any(|column| column == *name)
                    || (foreign_key.referred_columns.is_empty()
                        && primary_columns.iter().any(|column| column == *name))
            }
        }
    }
}

pub(super) fn remove_column_dependencies(
    state: &mut DatabaseState,
    schema: &mut TableSchema,
    column: &str,
    behavior: Option<ast::DropBehavior>,
) -> Result<()> {
    if views::has_view_column_dependency(&state.catalog, schema.id, column) {
        return Err(PgError::create(
            SqlState::DependentObjectsStillExist,
            "cannot drop column because a view depends on it",
        ));
    }
    views::preserve_column_drop_references(&mut state.catalog, schema, column);
    let primary_columns = schema
        .constraints
        .iter()
        .find_map(|constraint| match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. } => Some(columns.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let dependency = ForeignKeyTarget::Column {
        name: column,
        primary_columns: &primary_columns,
    };
    remove_local_referencing_foreign_keys(schema, usize::MAX, &dependency, behavior)?;
    remove_referencing_foreign_keys(state, schema.id, dependency, behavior)?;
    state.catalog.drop_column_owned_sequences(schema.id, column);
    schema.indexes.retain(|index| {
        !index.columns.iter().any(|key| key.name == column)
            && !index.include.iter().any(|included| included == column)
            && !index
                .predicate
                .as_ref()
                .is_some_and(|predicate| references_column(predicate, column))
    });
    schema.constraints.retain(|constraint| match constraint {
        crate::catalog::Constraint::PrimaryKey { columns, .. }
        | crate::catalog::Constraint::Unique { columns, .. } => {
            !columns.contains(&column.to_owned())
        }
        crate::catalog::Constraint::ForeignKey(foreign_key) => {
            !foreign_key.columns.contains(&column.to_owned())
        }
        crate::catalog::Constraint::Check { expression, .. } => {
            !references_column(expression, column)
        }
    });
    Ok(())
}

pub(super) fn remove_referencing_foreign_keys(
    state: &mut DatabaseState,
    table_id: TableId,
    dependency: ForeignKeyTarget<'_>,
    behavior: Option<ast::DropBehavior>,
) -> Result<()> {
    let mut tables = state
        .catalog
        .iterate_tables()
        .filter(|table| table.id != table_id)
        .cloned()
        .collect::<Vec<_>>();
    let depends = |foreign_key: &ForeignKey| dependency.matches_foreign_key(foreign_key, table_id);
    let referenced = tables.iter().any(|table| {
        table.constraints.iter().any(|constraint| {
            matches!(constraint, crate::catalog::Constraint::ForeignKey(foreign_key) if depends(foreign_key))
        })
    });
    if referenced && behavior != Some(ast::DropBehavior::Cascade) {
        return Err(PgError::create(
            SqlState::DependentObjectsStillExist,
            "cannot drop constraint because other objects depend on it",
        ));
    }
    for table in &mut tables {
        let before = table.constraints.len();
        table.constraints.retain(|constraint| {
            !matches!(constraint, crate::catalog::Constraint::ForeignKey(foreign_key) if depends(foreign_key))
        });
        if table.constraints.len() != before {
            state.catalog.replace_table(table.clone())?;
        }
    }
    Ok(())
}

pub(super) fn remove_local_referencing_foreign_keys(
    schema: &mut TableSchema,
    dropped_constraint: usize,
    dependency: &ForeignKeyTarget<'_>,
    behavior: Option<ast::DropBehavior>,
) -> Result<()> {
    let depends = |index: usize, constraint: &crate::catalog::Constraint| {
        if index == dropped_constraint {
            return false;
        }
        let crate::catalog::Constraint::ForeignKey(foreign_key) = constraint else {
            return false;
        };
        if let ForeignKeyTarget::Column { name, .. } = dependency
            && foreign_key.columns.iter().any(|column| column == *name)
        {
            return false;
        }
        dependency.matches_foreign_key(foreign_key, schema.id)
    };
    if schema
        .constraints
        .iter()
        .enumerate()
        .any(|(index, constraint)| depends(index, constraint))
        && behavior != Some(ast::DropBehavior::Cascade)
    {
        return Err(PgError::create(
            SqlState::DependentObjectsStillExist,
            "cannot drop object because other objects depend on it",
        ));
    }
    let mut index = 0;
    schema.constraints.retain(|constraint| {
        let keep = !depends(index, constraint);
        index += 1;
        keep
    });
    Ok(())
}

pub(super) fn rename_local_constraint_columns(
    schema: &mut TableSchema,
    old_name: &str,
    new_name: &str,
) {
    for constraint in &mut schema.constraints {
        match constraint {
            crate::catalog::Constraint::PrimaryKey { columns, .. }
            | crate::catalog::Constraint::Unique { columns, .. } => {
                for column in columns {
                    if column == old_name {
                        *column = new_name.to_owned();
                    }
                }
            }
            crate::catalog::Constraint::ForeignKey(foreign_key) => {
                for column in &mut foreign_key.columns {
                    if column == old_name {
                        *column = new_name.to_owned();
                    }
                }
                if foreign_key.foreign_table_id == schema.id {
                    for column in &mut foreign_key.referred_columns {
                        if column == old_name {
                            *column = new_name.to_owned();
                        }
                    }
                }
            }
            crate::catalog::Constraint::Check { .. } => {}
        }
    }
}

pub(super) fn rename_schema_expressions(schema: &mut TableSchema, old_name: &str, new_name: &str) {
    for column in &mut schema.columns {
        if let Some(default) = &mut column.default {
            rename_expression_column(default, old_name, new_name);
        }
    }
    for constraint in &mut schema.constraints {
        if let crate::catalog::Constraint::Check { expression, .. } = constraint {
            rename_expression_column(expression, old_name, new_name);
        }
    }
    for index in &mut schema.indexes {
        for column in &mut index.columns {
            if column.name == old_name {
                column.name = new_name.to_owned();
            }
        }
        for column in &mut index.include {
            if column == old_name {
                *column = new_name.to_owned();
            }
        }
        if let Some(predicate) = &mut index.predicate {
            rename_expression_column(predicate, old_name, new_name);
        }
    }
}

fn rename_expression_column(expression: &mut ast::Expr, old_name: &str, new_name: &str) {
    let _ = ast::visit_expressions_mut(expression, |nested| {
        match nested {
            ast::Expr::Identifier(identifier) if normalize_identifier(identifier) == old_name => {
                identifier.value = new_name.to_owned();
            }
            ast::Expr::CompoundIdentifier(identifiers)
                if identifiers
                    .last()
                    .is_some_and(|identifier| normalize_identifier(identifier) == old_name) =>
            {
                identifiers.last_mut().expect("identifier exists").value = new_name.to_owned();
            }
            _ => {}
        }
        std::ops::ControlFlow::<()>::Continue(())
    });
}

fn references_column(expression: &ast::Expr, column: &str) -> bool {
    let mut referenced = false;
    let _ = ast::visit_expressions(expression, |nested| {
        let matches = match nested {
            ast::Expr::Identifier(identifier) => normalize_identifier(identifier) == column,
            ast::Expr::CompoundIdentifier(identifiers) => identifiers
                .last()
                .is_some_and(|identifier| normalize_identifier(identifier) == column),
            _ => false,
        };
        if matches {
            referenced = true;
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    });
    referenced
}
