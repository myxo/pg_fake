//! SQL parser.

pub use sqlparser::ast::Statement;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::error::{PgError, Result, SqlState};

/// Parses one or more PostgreSQL statements into owned syntax trees.
pub fn parse(sql: &str) -> Result<Vec<Statement>> {
    Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|error| PgError::new(SqlState::SyntaxError, error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatementKind {
    Ddl,
    Dml,
    Select,
    TransactionControl,
    Set,
    Unsupported,
}

pub(crate) fn classify(statement: &Statement) -> StatementKind {
    match statement {
        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::CreateSchema { .. }
        | Statement::CreateView { .. }
        | Statement::AlterTable { .. }
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::Drop { .. } => StatementKind::Ddl,
        Statement::Insert(_) | Statement::Update { .. } | Statement::Delete(_) => {
            StatementKind::Dml
        }
        Statement::Query(_) => StatementKind::Select,
        Statement::StartTransaction { .. }
        | Statement::SetTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. }
        | Statement::ReleaseSavepoint { .. } => StatementKind::TransactionControl,
        Statement::SetVariable { .. }
        | Statement::SetTimeZone { .. }
        | Statement::SetRole { .. } => StatementKind::Set,
        _ => StatementKind::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_postgres_sql() {
        let statements = parse("CREATE TABLE users (id INTEGER); SELECT * FROM users").unwrap();

        assert_eq!(statements.len(), 2);
        assert!(matches!(statements[0], Statement::CreateTable(_)));
        assert!(matches!(statements[1], Statement::Query(_)));
    }

    #[test]
    fn reports_syntax_errors() {
        let error = parse("SELECT FROM").unwrap_err();

        assert_eq!(error.sqlstate, SqlState::SyntaxError);
    }

    #[test]
    fn classifies_statement_families() {
        let cases = [
            ("CREATE TABLE t (id INTEGER)", StatementKind::Ddl),
            ("INSERT INTO t VALUES (1)", StatementKind::Dml),
            ("SELECT * FROM t", StatementKind::Select),
            ("BEGIN", StatementKind::TransactionControl),
            ("SET application_name = 'pg_fake'", StatementKind::Set),
            ("EXPLAIN SELECT * FROM t", StatementKind::Unsupported),
        ];

        for (sql, expected) in cases {
            let statement = parse(sql).unwrap().pop().unwrap();
            assert_eq!(classify(&statement), expected, "{sql}");
        }
    }
}
