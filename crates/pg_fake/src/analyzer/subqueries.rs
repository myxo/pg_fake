use super::{
    literals::create_typed_literal,
    scopes::{bind_delete_scope, bind_update_scope, resolve_table_schema},
};
use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState},
    executor,
    value::{BaseType, PgType, Value},
};
use sqlparser::ast;
use std::ops::ControlFlow;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn substitute_typed_subqueries(
    statement: &ast::Statement,
    catalog: &Catalog,
) -> Result<ast::Statement> {
    let mut statement = statement.clone();
    if let ast::Statement::Insert(insert) = &mut statement {
        if let Some(source) = &mut insert.source
            && let ast::SetExpr::Select(select) = source.body.as_ref()
        {
            let outer = executor::bind_query_scope(catalog, select)?;
            substitute_scoped_subqueries(source.as_mut(), catalog, &outer)?;
        }
        if let Some(returning) = &mut insert.returning {
            let schema = catalog
                .require_named_table(&executor::resolve_insert_table_name(&insert.table)?)?;
            let outer = executor::bind_target_scope(
                schema,
                insert.table_alias.as_ref().map(|alias| &alias.alias),
            );
            substitute_scoped_subqueries(returning, catalog, &outer)?;
        }
        return Ok(statement);
    }
    let outer = match &statement {
        ast::Statement::Query(query) => match query.body.as_ref() {
            ast::SetExpr::Select(select) => Some(executor::bind_query_scope(catalog, select)?),
            _ => None,
        },
        ast::Statement::Update(update) => Some(bind_update_scope(update, catalog)?),
        ast::Statement::Delete(delete) => {
            let ast::FromTable::WithFromKeyword(from) = &delete.from else {
                return Ok(statement);
            };
            match from.first() {
                Some(first) => {
                    let schema = resolve_table_schema(&first.relation, catalog)?;
                    Some(bind_delete_scope(delete, schema, &first.relation, catalog)?)
                }
                None => None,
            }
        }
        _ => None,
    };
    if let Some(outer) = &outer {
        substitute_scoped_subqueries(&mut statement, catalog, outer)?;
        return Ok(statement);
    }
    let mut error = None;
    let _ = ast::visit_expressions_mut(&mut statement, |expression| {
        if error.is_some() {
            return ControlFlow::Break(());
        }
        let result = match expression {
            ast::Expr::Subquery(query) => executor::infer_query_output_columns(catalog, query)
                .and_then(|columns| {
                    if columns.len() != 1 {
                        return Err(PgError::create(
                            SqlState::SyntaxError,
                            "subquery must return only one column",
                        ));
                    }
                    Ok(create_typed_literal(Value::Null, columns[0].1))
                }),
            ast::Expr::Exists { .. } => Ok(create_typed_literal(
                Value::Bool(false),
                PgType::create(BaseType::Bool),
            )),
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => executor::infer_query_output_columns(catalog, subquery).and_then(|columns| {
                let left_width = match expr.as_ref() {
                    ast::Expr::Tuple(fields) => fields.len(),
                    _ => 1,
                };
                if columns.len() != left_width {
                    return Err(PgError::create(
                        SqlState::SyntaxError,
                        "subquery has too many columns",
                    ));
                }
                let fields = columns
                    .into_iter()
                    .map(|(_, data_type)| create_typed_literal(Value::Null, data_type))
                    .collect::<Vec<_>>();
                Ok(ast::Expr::InList {
                    expr: expr.clone(),
                    list: vec![if fields.len() == 1 {
                        fields.into_iter().next().expect("subquery has one column")
                    } else {
                        ast::Expr::Tuple(fields)
                    }],
                    negated: *negated,
                })
            }),
            _ => return ControlFlow::Continue(()),
        };
        match result {
            Ok(value) => *expression = value,
            Err(describe_error) => error = Some(describe_error),
        }
        ControlFlow::Continue(())
    });
    error.map_or(Ok(statement), Err)
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
fn substitute_scoped_subqueries<V: ast::VisitMut>(
    value: &mut V,
    catalog: &Catalog,
    outer: &executor::BoundScope,
) -> Result<()> {
    let mut describer = TypedSubquerySubstituter {
        catalog,
        outer,
        error: None,
    };
    let _ = value.visit(&mut describer);
    describer.error.map_or(Ok(()), Err)
}

struct TypedSubquerySubstituter<'a> {
    catalog: &'a Catalog,
    outer: &'a executor::BoundScope,
    error: Option<PgError>,
}

impl ast::VisitorMut for TypedSubquerySubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> ControlFlow<Self::Break> {
        if self.error.is_some() {
            return ControlFlow::Break(());
        }
        if !matches!(
            expression,
            ast::Expr::Subquery(_)
                | ast::Expr::Exists { .. }
                | ast::Expr::InSubquery { .. }
                | ast::Expr::AnyOp { .. }
                | ast::Expr::AllOp { .. }
        ) {
            return ControlFlow::Continue(());
        }
        match executor::substitute_typed_subqueries(self.catalog, expression, self.outer) {
            Ok(described) => *expression = described,
            Err(error) => {
                self.error = Some(error);
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}
