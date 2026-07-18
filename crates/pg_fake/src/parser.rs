//! SQL parser and statement dispatch.

use std::sync::Mutex;

pub use sqlparser::ast::Statement;
use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

use crate::error::{PgError, Result, SqlState};

static PARSER_MUTEX: Mutex<()> = Mutex::new(());

/// Parses one or more PostgreSQL statements into owned syntax trees.
pub fn parse(sql: &str) -> Result<Vec<Statement>> {
    let statements = {
        let _parser_lock = PARSER_MUTEX.lock().expect("parser mutex is poisoned");
        Parser::parse_sql(&PostgreSqlDialect {}, sql)
    };

    statements.map_err(|error| PgError::new(SqlState::SyntaxError, error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Ddl,
    Dml,
    Select,
    TransactionControl,
    Set,
    Unsupported,
}

impl std::fmt::Display for StatementKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            StatementKind::Ddl => "DDL",
            StatementKind::Dml => "DML",
            StatementKind::Select => "SELECT",
            StatementKind::TransactionControl => "transaction-control",
            StatementKind::Set => "SET",
            StatementKind::Unsupported => "unsupported",
        };
        f.write_str(name)
    }
}

pub fn classify(statement: &Statement) -> StatementKind {
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

/// Routes a statement to its executor handler.
///
/// The handlers become real executor entry points as their statement families
/// are implemented. Until then every route reports an unsupported feature.
pub fn dispatch(statement: &Statement) -> Result<()> {
    match classify(statement) {
        StatementKind::Ddl => execute_ddl(statement),
        StatementKind::Dml => execute_dml(statement),
        StatementKind::Select => execute_select(statement),
        StatementKind::TransactionControl => execute_transaction_control(statement),
        StatementKind::Set => execute_set(statement),
        StatementKind::Unsupported => execute_unsupported(statement),
    }
}

fn execute_ddl(_: &Statement) -> Result<()> {
    not_implemented(StatementKind::Ddl)
}

fn execute_dml(_: &Statement) -> Result<()> {
    not_implemented(StatementKind::Dml)
}

fn execute_select(_: &Statement) -> Result<()> {
    not_implemented(StatementKind::Select)
}

fn execute_transaction_control(_: &Statement) -> Result<()> {
    not_implemented(StatementKind::TransactionControl)
}

fn execute_set(_: &Statement) -> Result<()> {
    not_implemented(StatementKind::Set)
}

fn execute_unsupported(_: &Statement) -> Result<()> {
    not_implemented(StatementKind::Unsupported)
}

fn not_implemented(kind: StatementKind) -> Result<()> {
    Err(PgError::new(
        SqlState::FeatureNotSupported,
        format!("{kind} statements are not implemented"),
    ))
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

    #[test]
    fn dispatches_to_not_implemented_handler() {
        let statement = parse("SELECT 1").unwrap().pop().unwrap();
        let error = dispatch(&statement).unwrap_err();

        assert_eq!(error.sqlstate, SqlState::FeatureNotSupported);
        assert_eq!(error.message, "SELECT statements are not implemented");
    }
}
