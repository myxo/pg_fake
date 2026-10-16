use crate::catalog::{Constraint, IndexId, TableSchema};
use sqlparser::ast;

#[derive(Debug, Clone)]
pub(super) struct EqualityIndexAccess<T> {
    pub(super) columns: Vec<usize>,
    pub(super) values: Vec<T>,
    pub(super) unique: bool,
    pub(super) index_id: Option<IndexId>,
}

pub(super) fn find_complete_index_access<T: Clone>(
    schema: &TableSchema,
    equalities: &[(usize, T)],
    unique_only: bool,
    implies_predicate: Option<&dyn Fn(&ast::Expr) -> bool>,
) -> Option<EqualityIndexAccess<T>> {
    let mut best: Option<EqualityIndexAccess<T>> = None;
    let mut consider = |names: Vec<&str>, unique: bool, index_id| {
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
                index_id,
            });
        }
    };
    for constraint in &schema.constraints {
        if let Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } =
            constraint
        {
            consider(columns.iter().map(String::as_str).collect(), true, None);
        }
    }
    for index in &schema.indexes {
        if index
            .predicate
            .as_ref()
            .is_none_or(|predicate| implies_predicate.is_some_and(|implies| implies(predicate)))
        {
            consider(
                index
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect(),
                index.unique,
                Some(index.id),
            );
        }
    }
    best
}

pub(super) fn find_prefix_index_access<T: Clone>(
    schema: &TableSchema,
    equalities: &[(usize, T)],
    implies_predicate: Option<&dyn Fn(&ast::Expr) -> bool>,
) -> Option<EqualityIndexAccess<T>> {
    if equalities.is_empty() || (schema.constraints.is_empty() && schema.indexes.is_empty()) {
        return None;
    }
    let mut best: Option<EqualityIndexAccess<T>> = None;
    let mut consider = |names: Vec<&str>, index_id| {
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
                index_id,
            });
        }
    };
    for constraint in &schema.constraints {
        if let Constraint::PrimaryKey { columns, .. } | Constraint::Unique { columns, .. } =
            constraint
        {
            consider(columns.iter().map(String::as_str).collect(), None);
        }
    }
    for index in &schema.indexes {
        if index
            .predicate
            .as_ref()
            .is_none_or(|predicate| implies_predicate.is_some_and(|implies| implies(predicate)))
        {
            consider(
                index
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect(),
                Some(index.id),
            );
        }
    }
    best
}

pub(super) fn implies_partial_index_predicate(
    predicate: &ast::Expr,
    query_conjuncts: &[&ast::Expr],
    schema: &TableSchema,
    resolve_query_column: &dyn Fn(&[ast::Ident]) -> Option<usize>,
) -> bool {
    fn strip_nested(mut expression: &ast::Expr) -> &ast::Expr {
        while let ast::Expr::Nested(inner) = expression {
            expression = inner;
        }
        expression
    }
    fn resolve_index_column(expression: &ast::Expr, schema: &TableSchema) -> Option<usize> {
        let ast::Expr::Identifier(identifier) = strip_nested(expression) else {
            return None;
        };
        let name = if identifier.quote_style.is_some() {
            identifier.value.clone()
        } else {
            identifier.value.to_ascii_lowercase()
        };
        schema.columns.iter().position(|column| column.name == name)
    }
    fn resolve_query_atom(
        expression: &ast::Expr,
        resolve_query_column: &dyn Fn(&[ast::Ident]) -> Option<usize>,
    ) -> Option<(usize, bool)> {
        let (inner, is_null) = match strip_nested(expression) {
            ast::Expr::IsNull(inner) => (inner.as_ref(), true),
            ast::Expr::IsNotNull(inner) => (inner.as_ref(), false),
            _ => return None,
        };
        let slot = match strip_nested(inner) {
            ast::Expr::Identifier(identifier) => {
                resolve_query_column(std::slice::from_ref(identifier))
            }
            ast::Expr::CompoundIdentifier(identifiers) => resolve_query_column(identifiers),
            _ => None,
        }?;
        Some((slot, is_null))
    }
    fn prove(
        predicate: &ast::Expr,
        query_conjuncts: &[&ast::Expr],
        schema: &TableSchema,
        resolve_query_column: &dyn Fn(&[ast::Ident]) -> Option<usize>,
    ) -> bool {
        match strip_nested(predicate) {
            ast::Expr::BinaryOp {
                left,
                op: ast::BinaryOperator::And,
                right,
            } => {
                prove(left, query_conjuncts, schema, resolve_query_column)
                    && prove(right, query_conjuncts, schema, resolve_query_column)
            }
            ast::Expr::IsNull(inner) | ast::Expr::IsNotNull(inner) => {
                let Some(slot) = resolve_index_column(inner, schema) else {
                    return false;
                };
                let is_null = matches!(strip_nested(predicate), ast::Expr::IsNull(_));
                query_conjuncts.iter().any(|conjunct| {
                    resolve_query_atom(conjunct, resolve_query_column) == Some((slot, is_null))
                })
            }
            _ => false,
        }
    }
    prove(predicate, query_conjuncts, schema, resolve_query_column)
}
