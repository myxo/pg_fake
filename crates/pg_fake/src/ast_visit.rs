use sqlparser::ast;
use std::ops::ControlFlow;

pub(crate) struct ReadVisitor<'a>(pub(crate) &'a mut dyn ast::Visitor<Break = ()>);
pub(crate) struct WriteVisitor<'a>(pub(crate) &'a mut dyn ast::VisitorMut<Break = ()>);

macro_rules! forward_read {
    ($method:ident, $node:ty) => {
        fn $method(&mut self, node: &$node) -> ControlFlow<()> {
            self.0.$method(node)
        }
    };
}

macro_rules! forward_write {
    ($method:ident, $node:ty) => {
        fn $method(&mut self, node: &mut $node) -> ControlFlow<()> {
            self.0.$method(node)
        }
    };
}

impl ast::Visitor for ReadVisitor<'_> {
    type Break = ();

    forward_read!(pre_visit_query, ast::Query);
    forward_read!(post_visit_query, ast::Query);
    forward_read!(pre_visit_select, ast::Select);
    forward_read!(post_visit_select, ast::Select);
    forward_read!(pre_visit_relation, ast::ObjectName);
    forward_read!(post_visit_relation, ast::ObjectName);
    forward_read!(pre_visit_table_factor, ast::TableFactor);
    forward_read!(post_visit_table_factor, ast::TableFactor);
    forward_read!(pre_visit_expr, ast::Expr);
    forward_read!(post_visit_expr, ast::Expr);
    forward_read!(pre_visit_statement, ast::Statement);
    forward_read!(post_visit_statement, ast::Statement);
    forward_read!(pre_visit_value, ast::ValueWithSpan);
    forward_read!(post_visit_value, ast::ValueWithSpan);
    forward_read!(pre_visit_ident, ast::Ident);
    forward_read!(post_visit_ident, ast::Ident);
    forward_read!(pre_visit_order_by, ast::OrderBy);
    forward_read!(post_visit_order_by, ast::OrderBy);
    forward_read!(pre_visit_order_by_expr, ast::OrderByExpr);
    forward_read!(post_visit_order_by_expr, ast::OrderByExpr);
    forward_read!(pre_visit_group_by, ast::GroupByExpr);
    forward_read!(post_visit_group_by, ast::GroupByExpr);
}

impl ast::VisitorMut for WriteVisitor<'_> {
    type Break = ();

    forward_write!(pre_visit_query, ast::Query);
    forward_write!(post_visit_query, ast::Query);
    forward_write!(pre_visit_select, ast::Select);
    forward_write!(post_visit_select, ast::Select);
    forward_write!(pre_visit_relation, ast::ObjectName);
    forward_write!(post_visit_relation, ast::ObjectName);
    forward_write!(pre_visit_table_factor, ast::TableFactor);
    forward_write!(post_visit_table_factor, ast::TableFactor);
    forward_write!(pre_visit_expr, ast::Expr);
    forward_write!(post_visit_expr, ast::Expr);
    forward_write!(pre_visit_statement, ast::Statement);
    forward_write!(post_visit_statement, ast::Statement);
    forward_write!(pre_visit_value, ast::ValueWithSpan);
    forward_write!(post_visit_value, ast::ValueWithSpan);
    forward_write!(pre_visit_ident, ast::Ident);
    forward_write!(post_visit_ident, ast::Ident);
    forward_write!(pre_visit_order_by, ast::OrderBy);
    forward_write!(post_visit_order_by, ast::OrderBy);
    forward_write!(pre_visit_order_by_expr, ast::OrderByExpr);
    forward_write!(post_visit_order_by_expr, ast::OrderByExpr);
    forward_write!(pre_visit_group_by, ast::GroupByExpr);
    forward_write!(post_visit_group_by, ast::GroupByExpr);
}
