use super::quote_identifier;
use crate::executor::ctes::scope::{CteNameScope, enter_cte_scope};
use crate::executor::{
    DatabaseState, create_relation_object_name, normalize_function_name, normalize_relation_name,
    normalize_sequence_name, query,
};
use crate::{
    catalog::{Catalog, RelationName, TEMP_SCHEMA, TablePersistence, ViewDependency, ViewId},
    error::{PgError, Result, SqlState},
};
use sqlparser::ast::{self, VisitMut as _};
use std::collections::BTreeSet;

struct ViewDependencyCollector<'a> {
    catalog: &'a Catalog,
    dependencies: BTreeSet<ViewDependency>,
    permanent: bool,
    cte_scopes: Vec<CteNameScope>,
    error: Option<PgError>,
}

impl ast::VisitorMut for ViewDependencyCollector<'_> {
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
        let ast::TableFactor::Table {
            name: object_name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let mut name = match normalize_relation_name(object_name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if name.schema.is_none()
            && self
                .cte_scopes
                .last()
                .is_some_and(|scope| scope.body_mask.contains(&name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        let implicit_alias = name.name.clone();
        match self.catalog.require_named_table(&name) {
            Ok(table) => {
                if self.permanent && matches!(table.persistence, TablePersistence::Temporary { .. })
                {
                    self.error = Some(PgError::create(
                        SqlState::InvalidTableDefinition,
                        "cannot create a permanent view from a temporary relation",
                    ));
                    return std::ops::ControlFlow::Break(());
                }
                self.dependencies.insert(ViewDependency::Table(table.id));
                name = RelationName::create(
                    Some(self.catalog.get_schema_name(table.schema_id).to_owned()),
                    table.name.clone(),
                );
            }
            Err(error) if error.sqlstate == SqlState::WrongObjectType => {
                match self.catalog.require_named_view(&name) {
                    Ok(view) => {
                        if self.permanent
                            && self.catalog.get_schema_name(view.schema_id) == TEMP_SCHEMA
                        {
                            self.error = Some(PgError::create(
                                SqlState::InvalidTableDefinition,
                                "cannot create a permanent view from a temporary relation",
                            ));
                            return std::ops::ControlFlow::Break(());
                        }
                        self.dependencies.insert(ViewDependency::View(view.id));
                        name = RelationName::create(
                            Some(self.catalog.get_schema_name(view.schema_id).to_owned()),
                            view.name.clone(),
                        );
                    }
                    Err(error) => {
                        self.error = Some(error);
                        return std::ops::ControlFlow::Break(());
                    }
                }
            }
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        }
        *object_name = create_relation_object_name(name);
        if alias.is_none() {
            *alias = Some(ast::TableAlias {
                explicit: true,
                name: ast::Ident::with_quote('"', implicit_alias),
                columns: Vec::new(),
                at: None,
            });
        }
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> std::ops::ControlFlow<Self::Break> {
        let ast::Expr::Function(function) = expression else {
            return std::ops::ControlFlow::Continue(());
        };
        let Ok(function_name) = normalize_function_name(&function.name) else {
            return std::ops::ControlFlow::Continue(());
        };
        if !matches!(function_name.as_str(), "nextval" | "currval" | "setval") {
            return std::ops::ControlFlow::Continue(());
        }
        let ast::FunctionArguments::List(arguments) = &mut function.args else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(argument))) =
            arguments.args.first_mut()
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let Some(literal) = extract_sequence_literal_mut(argument) else {
            return std::ops::ControlFlow::Continue(());
        };
        let name = match normalize_sequence_name(literal) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        let sequence = match self.catalog.require_named_sequence(&name) {
            Ok(sequence) => sequence,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if self.permanent && self.catalog.get_schema_name(sequence.schema_id) == TEMP_SCHEMA {
            self.error = Some(PgError::create(
                SqlState::InvalidTableDefinition,
                "cannot create a permanent view from a temporary relation",
            ));
            return std::ops::ControlFlow::Break(());
        }
        self.dependencies
            .insert(ViewDependency::Sequence(sequence.id));
        *literal = format!(
            "{}.{}",
            quote_identifier(self.catalog.get_schema_name(sequence.schema_id)),
            quote_identifier(&sequence.name)
        );
        std::ops::ControlFlow::Continue(())
    }
}

fn extract_sequence_literal_mut(expression: &mut ast::Expr) -> Option<&mut String> {
    match expression {
        ast::Expr::Cast { expr, .. } | ast::Expr::Nested(expr) => {
            extract_sequence_literal_mut(expr)
        }
        ast::Expr::Value(value) => match &mut value.value {
            ast::Value::SingleQuotedString(value) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn bind_view_dependencies(
    state: &DatabaseState,
    query: &ast::Query,
    permanent: bool,
) -> Result<(Box<ast::Query>, BTreeSet<ViewDependency>)> {
    let mut query = query.clone();
    let mut collector = ViewDependencyCollector {
        catalog: &state.catalog,
        dependencies: BTreeSet::new(),
        permanent,
        cte_scopes: Vec::new(),
        error: None,
    };
    let _ = query.visit(&mut collector);
    match collector.error {
        Some(error) => Err(error),
        None => {
            collector.dependencies.extend(
                query::collect_query_primary_key_dependencies(state, &query)
                    .into_iter()
                    .map(ViewDependency::Constraint),
            );
            Ok((Box::new(query), collector.dependencies))
        }
    }
}

pub(super) fn has_view_dependency_path(catalog: &Catalog, start: ViewId, target: ViewId) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if id == target {
            return true;
        }
        if !visited.insert(id) {
            continue;
        }
        if let Some(view) = catalog.iterate_views().find(|view| view.id == id) {
            pending.extend(
                view.dependencies
                    .iter()
                    .filter_map(|dependency| match dependency {
                        ViewDependency::View(id) => Some(*id),
                        ViewDependency::Table(_)
                        | ViewDependency::Sequence(_)
                        | ViewDependency::Constraint(_) => None,
                    }),
            );
        }
    }
    false
}
