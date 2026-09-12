use super::{
    cte_scope::{CteNameScope, enter_cte_scope},
    quote_identifier,
};
use crate::catalog::{Catalog, ColumnDef, TableId, TableSchema, ViewDependency};
use crate::executor::{create_relation_object_name, normalize_relation_name};
use sqlparser::ast::{self, VisitMut as _};
use std::collections::BTreeSet;

struct TableReferenceRenamer<'a> {
    catalog: &'a Catalog,
    table_id: TableId,
    new_name: &'a str,
    cte_scopes: Vec<CteNameScope>,
}

impl ast::VisitorMut for TableReferenceRenamer<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        enter_cte_scope(&mut self.cte_scopes, query);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte_scopes
            .pop()
            .expect("visited query pushed a CTE scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_relation(
        &mut self,
        relation: &mut ast::ObjectName,
    ) -> std::ops::ControlFlow<Self::Break> {
        let Ok(name) = normalize_relation_name(relation) else {
            return std::ops::ControlFlow::Continue(());
        };
        if name.schema.is_none()
            && self
                .cte_scopes
                .last()
                .is_some_and(|scope| scope.body_mask.contains(&name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        if self
            .catalog
            .require_named_table(&name)
            .is_ok_and(|table| table.id == self.table_id)
        {
            let ast::ObjectNamePart::Identifier(identifier) = relation
                .0
                .last_mut()
                .expect("normalized relation name is non-empty")
            else {
                unreachable!("normalized relation name ends in an identifier")
            };
            identifier.value = self.new_name.to_owned();
        }
        std::ops::ControlFlow::Continue(())
    }
}

struct ColumnSourceWrapper<'a> {
    catalog: &'a Catalog,
    table: &'a TableSchema,
    old_name: &'a str,
    new_name: Option<&'a str>,
    dependent_columns: &'a BTreeSet<String>,
    preserve_full_arity: bool,
    skip_generated_source: bool,
    cte_scopes: Vec<CteNameScope>,
    nested_join_depth: usize,
}

impl ColumnSourceWrapper<'_> {
    fn get_bound_name<'a>(&'a self, column: &'a ColumnDef) -> &'a str {
        if self.new_name == Some(column.name.as_str()) {
            self.old_name
        } else {
            &column.name
        }
    }
}

impl ast::VisitorMut for ColumnSourceWrapper<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        enter_cte_scope(&mut self.cte_scopes, query);
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.cte_scopes
            .pop()
            .expect("visited query pushed a CTE scope");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::NestedJoin { .. }) {
            self.nested_join_depth += 1;
            return std::ops::ControlFlow::Continue(());
        }
        if self.skip_generated_source {
            self.skip_generated_source = false;
            return std::ops::ControlFlow::Continue(());
        }
        let ast::TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(relation_name) = normalize_relation_name(name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if relation_name.schema.is_none()
            && self
                .cte_scopes
                .last()
                .is_some_and(|scope| scope.body_mask.contains(&relation_name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let matches_table = self
            .catalog
            .require_named_table(&relation_name)
            .is_ok_and(|table| table.id == self.table.id)
            || (relation_name.name == self.table.name
                && relation_name.schema.as_deref().is_none_or(|schema| {
                    schema == self.catalog.get_schema_name(self.table.schema_id)
                }));
        if !matches_table {
            return std::ops::ControlFlow::Continue(());
        }
        let source_alias = quote_identifier("__pg_fake_column_source");
        let dependency_len = self
            .table
            .columns
            .iter()
            .rposition(|column| self.dependent_columns.contains(self.get_bound_name(column)))
            .map_or(0, |index| index + 1);
        let projection_len = if self.preserve_full_arity || self.nested_join_depth != 0 {
            self.table.columns.len()
        } else {
            dependency_len.max(alias.as_ref().map_or(0, |alias| alias.columns.len()))
        };
        assert_ne!(projection_len, 0);
        let projections = self
            .table
            .columns
            .iter()
            .take(projection_len)
            .map(|column| {
                let bound_name = self.get_bound_name(column);
                if !self.dependent_columns.contains(bound_name) {
                    return format!("NULL AS {}", quote_identifier(bound_name));
                }
                let source = quote_identifier(&column.name);
                let output = quote_identifier(bound_name);
                if bound_name == column.name {
                    format!("{source_alias}.{source}")
                } else {
                    format!("{source_alias}.{source} AS {output}")
                }
            })
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT {} FROM {} AS {source_alias}",
            projections.join(", "),
            create_relation_object_name(relation_name)
        );
        let mut statements = crate::parser::parse(&sql).expect("generated column wrapper parses");
        let ast::Statement::Query(query) = statements
            .pop()
            .expect("generated column wrapper contains one statement")
        else {
            unreachable!("generated column wrapper is a query")
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: query,
            alias: alias.clone(),
            sample: None,
        };
        self.skip_generated_source = true;
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        if matches!(factor, ast::TableFactor::NestedJoin { .. }) {
            self.nested_join_depth -= 1;
        }
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn rename_table_references(catalog: &mut Catalog, table_id: TableId, new_name: &str) {
    let snapshot = catalog.clone();
    for view in catalog
        .iterate_views_mut()
        .filter(|view| view.dependencies.contains(&ViewDependency::Table(table_id)))
    {
        let mut query = view.query.as_ref().clone();
        let mut renamer = TableReferenceRenamer {
            catalog: &snapshot,
            table_id,
            new_name,
            cte_scopes: Vec::new(),
        };
        let _ = query.visit(&mut renamer);
        view.query = Box::new(query);
    }
}

pub(crate) fn rename_column_references(
    catalog: &mut Catalog,
    table: &TableSchema,
    old_name: &str,
    new_name: &str,
) {
    let table_id = table.id;
    let snapshot = catalog.clone();
    for view in catalog
        .iterate_views_mut()
        .filter(|view| view.dependencies.contains(&ViewDependency::Table(table_id)))
    {
        let Some(columns) = view.column_dependencies.get_mut(&table_id) else {
            continue;
        };
        if !columns.contains(old_name) {
            continue;
        }
        let mut query = view.query.as_ref().clone();
        let mut wrapper = ColumnSourceWrapper {
            catalog: &snapshot,
            table,
            old_name,
            new_name: Some(new_name),
            dependent_columns: columns,
            preserve_full_arity: false,
            skip_generated_source: false,
            cte_scopes: Vec::new(),
            nested_join_depth: 0,
        };
        let _ = query.visit(&mut wrapper);
        assert!(columns.remove(old_name));
        columns.insert(new_name.to_owned());
        view.query = Box::new(query);
    }
}

pub(crate) fn preserve_column_drop_references(
    catalog: &mut Catalog,
    table: &TableSchema,
    column_name: &str,
) {
    let table_id = table.id;
    let snapshot = catalog.clone();
    for view in catalog
        .iterate_views_mut()
        .filter(|view| view.dependencies.contains(&ViewDependency::Table(table_id)))
    {
        let columns = view
            .column_dependencies
            .get(&table_id)
            .expect("table view dependency has column dependency storage");
        assert!(!columns.contains(column_name));
        let mut query = view.query.as_ref().clone();
        let mut wrapper = ColumnSourceWrapper {
            catalog: &snapshot,
            table,
            old_name: column_name,
            new_name: None,
            dependent_columns: columns,
            preserve_full_arity: true,
            skip_generated_source: false,
            cte_scopes: Vec::new(),
            nested_join_depth: 0,
        };
        let _ = query.visit(&mut wrapper);
        view.query = Box::new(query);
    }
}
