use super::quote_identifier;
use crate::executor::{normalize_identifier, normalize_relation_name};
use crate::{
    catalog::{Catalog, ViewColumn, ViewId},
    error::{PgError, Result, SqlState},
};
use sqlparser::ast::{self, VisitMut as _};

fn freeze_view_output(query: &ast::Query, columns: &[ViewColumn]) -> Result<Box<ast::Query>> {
    let alias = quote_identifier("__pg_fake_view_input");
    let names = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>();
    let projection = names
        .iter()
        .map(|name| format!("{alias}.{name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let aliases = names.join(", ");
    let sql = format!("SELECT {projection} FROM ({query}) AS {alias} ({aliases})");
    let mut statements = crate::parser::parse(&sql)?;
    let ast::Statement::Query(query) = statements
        .pop()
        .expect("generated view projection contains one statement")
    else {
        unreachable!("generated view projection is a query")
    };
    Ok(query)
}

struct ViewExpander<'a> {
    catalog: &'a Catalog,
    stack: Vec<ViewId>,
    masked: Vec<Vec<String>>,
    error: Option<PgError>,
}

impl ast::VisitorMut for ViewExpander<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.push(
            query
                .with
                .as_ref()
                .map(|with| {
                    with.cte_tables
                        .iter()
                        .map(|cte| normalize_identifier(&cte.alias.name))
                        .collect()
                })
                .unwrap_or_default(),
        );
        std::ops::ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &mut ast::Query) -> std::ops::ControlFlow<Self::Break> {
        self.masked.pop().expect("visited query pushed a CTE mask");
        std::ops::ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(
        &mut self,
        factor: &mut ast::TableFactor,
    ) -> std::ops::ControlFlow<Self::Break> {
        let ast::TableFactor::Table {
            name,
            alias,
            args: None,
            ..
        } = factor
        else {
            return std::ops::ControlFlow::Continue(());
        };
        let relation_name = match normalize_relation_name(name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if relation_name.schema.is_none()
            && self
                .masked
                .iter()
                .any(|names| names.contains(&relation_name.name))
        {
            return std::ops::ControlFlow::Continue(());
        }
        if crate::executor::describe_visible_system_relation(self.catalog, &relation_name).is_some()
        {
            return std::ops::ControlFlow::Continue(());
        }
        let view = match self.catalog.require_named_view(&relation_name) {
            Ok(view) => view,
            Err(error)
                if matches!(
                    error.sqlstate,
                    SqlState::UndefinedTable | SqlState::WrongObjectType
                ) =>
            {
                return std::ops::ControlFlow::Continue(());
            }
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        if self.stack.contains(&view.id) {
            self.error = Some(PgError::create(
                SqlState::InvalidObjectDefinition,
                format!(
                    "infinite recursion detected in rules for relation {:?}",
                    view.name
                ),
            ));
            return std::ops::ControlFlow::Break(());
        }
        let mut query = view.query.as_ref().clone();
        let mut expander = ViewExpander {
            catalog: self.catalog,
            stack: self
                .stack
                .iter()
                .copied()
                .chain(std::iter::once(view.id))
                .collect(),
            masked: Vec::new(),
            error: None,
        };
        let _ = query.visit(&mut expander);
        if let Some(error) = expander.error {
            self.error = Some(error);
            return std::ops::ControlFlow::Break(());
        }
        let query = match freeze_view_output(&query, &view.columns) {
            Ok(query) => query,
            Err(error) => {
                self.error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        let columns = view
            .columns
            .iter()
            .map(|column| ast::TableAliasColumnDef {
                name: ast::Ident::with_quote('"', column.name.clone()),
                data_type: None,
            })
            .collect::<Vec<_>>();
        let alias = match alias {
            Some(alias) if alias.columns.is_empty() => ast::TableAlias {
                columns,
                ..alias.clone()
            },
            Some(alias) => alias.clone(),
            None => ast::TableAlias {
                explicit: true,
                name: ast::Ident::with_quote('"', view.name.clone()),
                columns,
                at: None,
            },
        };
        *factor = ast::TableFactor::Derived {
            lateral: false,
            subquery: query,
            alias: Some(alias),
            sample: None,
        };
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn expand_query_views(
    catalog: &Catalog,
    query: &ast::Query,
) -> Result<Option<ast::Query>> {
    struct ViewProbe<'a> {
        catalog: &'a Catalog,
    }

    impl ast::Visitor for ViewProbe<'_> {
        type Break = ();

        fn pre_visit_table_factor(
            &mut self,
            factor: &ast::TableFactor,
        ) -> std::ops::ControlFlow<Self::Break> {
            let ast::TableFactor::Table {
                name, args: None, ..
            } = factor
            else {
                return std::ops::ControlFlow::Continue(());
            };
            if let Ok(name) = normalize_relation_name(name)
                && let Err(error) = self.catalog.require_named_view(&name)
                && matches!(
                    error.sqlstate,
                    SqlState::UndefinedTable | SqlState::WrongObjectType
                )
            {
                return std::ops::ControlFlow::Continue(());
            }
            std::ops::ControlFlow::Break(())
        }
    }

    if ast::Visit::visit(query, &mut ViewProbe { catalog }).is_continue() {
        return Ok(None);
    }
    let mut expanded = query.clone();
    let mut expander = ViewExpander {
        catalog,
        stack: Vec::new(),
        masked: Vec::new(),
        error: None,
    };
    let _ = expanded.visit(&mut expander);
    match expander.error {
        Some(error) => Err(error),
        None => Ok((expanded != *query).then_some(expanded)),
    }
}
