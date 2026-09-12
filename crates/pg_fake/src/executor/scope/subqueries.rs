use super::{BoundScope, RowScope, output::describe_bound_query_columns};
use crate::executor::expressions;
use crate::{
    catalog::Catalog,
    error::{PgError, Result, SqlState},
    value::{BaseType, PgType},
};
use sqlparser::ast::{self, VisitMut as _};
use std::ops::ControlFlow;

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(in crate::executor) fn infer_expression_data_type(
    catalog: &Catalog,
    expr: &ast::Expr,
    scope: &BoundScope,
) -> Result<PgType> {
    if matches!(expr, ast::Expr::Value(value) if matches!(value.value, ast::Value::Placeholder(_)))
    {
        return Ok(PgType::create(BaseType::Text));
    }
    if let ast::Expr::Subquery(query) = expr {
        let columns = describe_bound_query_columns(catalog, query, Some(scope))?;
        if columns.len() != 1 {
            return Err(PgError::create(
                SqlState::SyntaxError,
                "subquery must return only one column",
            ));
        }
        return Ok(columns[0].data_type);
    }
    let mut expression = expr.clone();
    let mut describer = TypedSubquerySubstituter {
        catalog,
        outer: scope,
        error: None,
    };
    let _ = expression.visit(&mut describer);
    if let Some(error) = describer.error {
        return Err(error);
    }
    expressions::infer_expression_data_type(&expression, RowScope::Bound(scope))
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn substitute_typed_subqueries(
    catalog: &Catalog,
    expression: &ast::Expr,
    outer: &BoundScope,
) -> Result<ast::Expr> {
    let mut expression = expression.clone();
    let mut describer = TypedSubquerySubstituter {
        catalog,
        outer,
        error: None,
    };
    let _ = expression.visit(&mut describer);
    describer.error.map_or(Ok(expression), Err)
}

struct TypedSubquerySubstituter<'a> {
    catalog: &'a Catalog,
    outer: &'a BoundScope,
    error: Option<PgError>,
}

impl ast::VisitorMut for TypedSubquerySubstituter<'_> {
    type Break = ();

    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn pre_visit_expr(&mut self, expression: &mut ast::Expr) -> ControlFlow<Self::Break> {
        if self.error.is_some() {
            return ControlFlow::Break(());
        }
        let result = match expression {
            ast::Expr::Subquery(query) => {
                describe_bound_query_columns(self.catalog, query, Some(self.outer)).and_then(
                    |columns| {
                        if columns.len() != 1 {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "subquery must return only one column",
                            ));
                        }
                        Ok(crate::analyzer::create_typed_literal(
                            crate::value::Value::Null,
                            columns[0].data_type,
                        ))
                    },
                )
            }
            ast::Expr::Exists { .. } => Ok(crate::analyzer::create_typed_literal(
                crate::value::Value::Bool(false),
                PgType::create(BaseType::Bool),
            )),
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => describe_bound_query_columns(self.catalog, subquery, Some(self.outer)).and_then(
                |columns| {
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
                    let mut fields = columns.into_iter().map(|column| {
                        crate::analyzer::create_typed_literal(
                            crate::value::Value::Null,
                            column.data_type,
                        )
                    });
                    let candidate = if left_width == 1 {
                        fields.next().expect("subquery has one column")
                    } else {
                        ast::Expr::Tuple(fields.collect())
                    };
                    Ok(ast::Expr::InList {
                        expr: expr.clone(),
                        list: vec![candidate],
                        negated: *negated,
                    })
                },
            ),
            ast::Expr::AnyOp {
                left,
                compare_op,
                right,
                is_some,
            } if matches!(right.as_ref(), ast::Expr::Subquery(_)) => {
                let ast::Expr::Subquery(query) = right.as_ref() else {
                    unreachable!("quantified right side was checked")
                };
                describe_bound_query_columns(self.catalog, query, Some(self.outer)).and_then(
                    |columns| {
                        if columns.len() != 1 {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "subquery has too many columns",
                            ));
                        }
                        Ok(ast::Expr::AnyOp {
                            left: left.clone(),
                            compare_op: compare_op.clone(),
                            right: Box::new(ast::Expr::Tuple(vec![
                                crate::analyzer::create_typed_literal(
                                    crate::value::Value::Null,
                                    columns[0].data_type,
                                ),
                            ])),
                            is_some: *is_some,
                        })
                    },
                )
            }
            ast::Expr::AllOp {
                left,
                compare_op,
                right,
            } if matches!(right.as_ref(), ast::Expr::Subquery(_)) => {
                let ast::Expr::Subquery(query) = right.as_ref() else {
                    unreachable!("quantified right side was checked")
                };
                describe_bound_query_columns(self.catalog, query, Some(self.outer)).and_then(
                    |columns| {
                        if columns.len() != 1 {
                            return Err(PgError::create(
                                SqlState::SyntaxError,
                                "subquery has too many columns",
                            ));
                        }
                        Ok(ast::Expr::AllOp {
                            left: left.clone(),
                            compare_op: compare_op.clone(),
                            right: Box::new(ast::Expr::Tuple(vec![
                                crate::analyzer::create_typed_literal(
                                    crate::value::Value::Null,
                                    columns[0].data_type,
                                ),
                            ])),
                        })
                    },
                )
            }
            _ => return ControlFlow::Continue(()),
        };
        match result {
            Ok(replacement) => *expression = replacement,
            Err(error) => self.error = Some(error),
        }
        ControlFlow::Continue(())
    }
}
