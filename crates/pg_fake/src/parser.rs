//! SQL parser.

pub use sqlparser::ast::Statement;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::error::{PgError, Result, SqlState};

/// Parses one or more PostgreSQL statements into owned syntax trees.
pub fn parse(sql: &str) -> Result<Vec<Statement>> {
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
        Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) => StatementKind::Dml,
        Statement::Query(_) => StatementKind::Query,
        Statement::StartTransaction { .. }
        | Statement::Commit { .. }
        | Statement::Rollback { .. }
        | Statement::Savepoint { .. }
        | Statement::ReleaseSavepoint { .. } => StatementKind::TransactionControl,
        Statement::Set(_) => StatementKind::Set,
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
    fn preserves_foreign_key_match_kind() {
        let statements = parse(
            "CREATE TABLE child (parent_id INTEGER, CONSTRAINT child_parent_fkey FOREIGN KEY (parent_id) REFERENCES parent (id) MATCH FULL)",
        )
        .unwrap();
        let Statement::CreateTable(create) = &statements[0] else {
            panic!("statement should be CREATE TABLE");
        };
        let sqlparser::ast::TableConstraint::ForeignKey(foreign_key) = &create.constraints[0]
        else {
            panic!("constraint should be FOREIGN KEY");
        };

        assert_eq!(
            foreign_key.match_kind,
            Some(sqlparser::ast::ConstraintReferenceMatchKind::Full)
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
