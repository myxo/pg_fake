//! SQL parser.

pub use ast::Statement;
use sqlparser::ast;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::error::{PgError, Result, SqlState};

/// Parses one or more PostgreSQL statements into owned syntax trees.
#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub fn parse(sql: &str) -> Result<Vec<ast::Statement>> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|error| PgError::create(SqlState::SyntaxError, error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatementKind {
    Ddl,
    Dml,
    Query,
    TransactionControl,
    Set,
    Unsupported,
}

#[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
pub(crate) fn classify(statement: &ast::Statement) -> StatementKind {
    match statement {
        ast::Statement::CreateTable(_)
        | ast::Statement::CreateSequence { .. }
        | ast::Statement::CreateIndex(_)
        | ast::Statement::CreateSchema { .. }
        | ast::Statement::CreateView { .. }
        | ast::Statement::CreateFunction(_)
        | ast::Statement::CreateTrigger(_)
        | ast::Statement::DropFunction(_)
        | ast::Statement::DropTrigger(_)
        | ast::Statement::AlterTable { .. }
        | ast::Statement::AlterIndex { .. }
        | ast::Statement::AlterTrigger { .. }
        | ast::Statement::AlterView { .. }
        | ast::Statement::Comment { .. }
        | ast::Statement::Drop { .. } => StatementKind::Ddl,
        ast::Statement::Insert(_)
        | ast::Statement::Update(_)
        | ast::Statement::Delete(_)
        | ast::Statement::Truncate(_) => StatementKind::Dml,
        ast::Statement::Query(_) => StatementKind::Query,
        ast::Statement::StartTransaction { .. }
        | ast::Statement::Commit { .. }
        | ast::Statement::Rollback { .. }
        | ast::Statement::Savepoint { .. }
        | ast::Statement::ReleaseSavepoint { .. }
        | ast::Statement::Lock(_) => StatementKind::TransactionControl,
        ast::Statement::Set(_) => StatementKind::Set,
        _ => StatementKind::Unsupported,
    }
}

#[cfg(test)]
#[path = "parser_test.rs"]
mod tests;
