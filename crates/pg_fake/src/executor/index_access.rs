use crate::catalog::{Constraint, TableSchema};

#[derive(Debug, Clone)]
pub(super) struct EqualityIndexAccess<T> {
    pub(super) columns: Vec<usize>,
    pub(super) values: Vec<T>,
    pub(super) unique: bool,
}

pub(super) fn find_complete_index_access<T: Clone>(
    schema: &TableSchema,
    equalities: &[(usize, T)],
    unique_only: bool,
) -> Option<EqualityIndexAccess<T>> {
    let mut best: Option<EqualityIndexAccess<T>> = None;
    let mut consider = |names: Vec<&str>, unique: bool| {
        if unique_only && !unique {
            return;
        }
        let Some(columns) = names
            .iter()
            .map(|name| {
                schema
                    .columns
                    .iter()
                    .position(|column| column.name == *name)
            })
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let Some(values) = columns
            .iter()
            .map(|column| {
                equalities
                    .iter()
                    .find(|(candidate, _)| candidate == column)
                    .map(|(_, value)| value.clone())
            })
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        if best.as_ref().is_none_or(|current| {
            (unique && !current.unique)
                || (unique == current.unique
                    && if unique {
                        columns.len() < current.columns.len()
                    } else {
                        columns.len() > current.columns.len()
                    })
        }) {
            best = Some(EqualityIndexAccess {
                columns,
                values,
                unique,
            });
        }
    };
    for constraint in &schema.constraints {
        if let Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } =
            constraint
        {
            consider(columns.iter().map(String::as_str).collect(), true);
        }
    }
    for index in &schema.indexes {
        if index.predicate.is_none() {
            consider(
                index
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect(),
                index.unique,
            );
        }
    }
    best
}

pub(super) fn find_prefix_index_access<T: Clone>(
    schema: &TableSchema,
    equalities: &[(usize, T)],
) -> Option<EqualityIndexAccess<T>> {
    if equalities.is_empty() || (schema.constraints.is_empty() && schema.indexes.is_empty()) {
        return None;
    }
    let mut best: Option<EqualityIndexAccess<T>> = None;
    let mut consider = |names: Vec<&str>| {
        let Some(columns) = names
            .iter()
            .map(|name| {
                schema
                    .columns
                    .iter()
                    .position(|column| column.name == *name)
            })
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };
        let values = columns
            .iter()
            .map_while(|column| {
                equalities
                    .iter()
                    .find(|(candidate, _)| candidate == column)
                    .map(|(_, value)| value.clone())
            })
            .collect::<Vec<_>>();
        if values.is_empty() || values.len() == columns.len() {
            return;
        }
        if best.as_ref().is_none_or(|current| {
            values.len() > current.values.len()
                || (values.len() == current.values.len() && columns.len() < current.columns.len())
        }) {
            best = Some(EqualityIndexAccess {
                columns,
                values,
                unique: false,
            });
        }
    };
    for constraint in &schema.constraints {
        if let Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } =
            constraint
        {
            consider(columns.iter().map(String::as_str).collect());
        }
    }
    for index in &schema.indexes {
        if index.predicate.is_none() {
            consider(
                index
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect(),
            );
        }
    }
    best
}
