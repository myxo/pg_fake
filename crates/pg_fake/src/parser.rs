//! SQL parser.

pub use ast::Statement;
use sqlparser::ast;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::error::{PgError, Result, SqlState};

/// Parses one or more PostgreSQL statements into owned syntax trees.
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

pub(crate) fn classify(statement: &ast::Statement) -> StatementKind {
    match statement {
        ast::Statement::CreateTable(_)
        | ast::Statement::CreateSequence { .. }
        | ast::Statement::CreateIndex(_)
        | ast::Statement::CreateSchema { .. }
        | ast::Statement::CreateView { .. }
        | ast::Statement::AlterTable { .. }
        | ast::Statement::AlterIndex { .. }
        | ast::Statement::AlterView { .. }
        | ast::Statement::Drop { .. } => StatementKind::Ddl,
        ast::Statement::Insert(_) | ast::Statement::Update(_) | ast::Statement::Delete(_) => {
            StatementKind::Dml
        }
        ast::Statement::Query(_) => StatementKind::Query,
        ast::Statement::StartTransaction { .. }
        | ast::Statement::Commit { .. }
        | ast::Statement::Rollback { .. }
        | ast::Statement::Savepoint { .. }
        | ast::Statement::ReleaseSavepoint { .. } => StatementKind::TransactionControl,
        ast::Statement::Set(_) => StatementKind::Set,
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
        assert!(matches!(statements[0], ast::Statement::CreateTable(_)));
        assert!(matches!(statements[1], ast::Statement::Query(_)));
    }

    #[test]
    fn reports_syntax_errors() {
        let error = parse("SELECT FROM").unwrap_err();

        assert_eq!(error.sqlstate, SqlState::SyntaxError);
    }

    #[test]
    fn preserves_foreign_key_match_kind() {
        let statements = parse(
            "CREATE TABLE child (parent_id INTEGER, CONSTRAINT child_parent_fkey FOREIGN KEY (parent_id) REFERENCES parent (id) MATCH FULL)",
        )
        .unwrap();
        let ast::Statement::CreateTable(create) = &statements[0] else {
            panic!("statement should be CREATE TABLE");
        };
        let ast::TableConstraint::ForeignKey(foreign_key) = &create.constraints[0] else {
            panic!("constraint should be FOREIGN KEY");
        };

        assert_eq!(
            foreign_key.match_kind,
            Some(ast::ConstraintReferenceMatchKind::Full)
        );
        assert_eq!(
            foreign_key.name.as_ref().map(|name| name.value.as_str()),
            Some("child_parent_fkey")
        );
    }

    #[test]
    fn classifies_statement_families() {
        let cases = [
            ("CREATE TABLE t (id INTEGER)", StatementKind::Ddl),
            ("CREATE SEQUENCE s", StatementKind::Ddl),
            ("INSERT INTO t VALUES (1)", StatementKind::Dml),
            ("SELECT * FROM t", StatementKind::Query),
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
