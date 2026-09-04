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
        | ast::Statement::CreateTrigger(_)
        | ast::Statement::AlterTable { .. }
        | ast::Statement::AlterIndex { .. }
        | ast::Statement::AlterTrigger { .. }
        | ast::Statement::AlterView { .. }
        | ast::Statement::Comment { .. }
        | ast::Statement::Drop { .. } => StatementKind::Ddl,
        ast::Statement::Insert(_) | ast::Statement::Update(_) | ast::Statement::Delete(_) => {
            StatementKind::Dml
        }
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
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn parses_postgres_sql() {
        let statements = parse("CREATE TABLE users (id INTEGER); SELECT * FROM users").unwrap();

        assert_eq!(statements.len(), 2);
        assert!(matches!(statements[0], ast::Statement::CreateTable(_)));
        assert!(matches!(statements[1], ast::Statement::Query(_)));
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn reports_syntax_errors() {
        let error = parse("SELECT FROM").unwrap_err();

        assert_eq!(error.sqlstate, SqlState::SyntaxError);
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
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
    fn preserves_alter_index_if_exists() {
        let statement = parse("ALTER INDEX IF EXISTS public.old_name RENAME TO new_name")
            .unwrap()
            .pop()
            .unwrap();
        let ast::Statement::AlterIndex {
            if_exists,
            name,
            operation,
        } = statement
        else {
            panic!("statement should be ALTER INDEX");
        };

        assert!(if_exists);
        assert_eq!(name.to_string(), "public.old_name");
        assert!(matches!(
            operation,
            ast::AlterIndexOperation::RenameIndex { index_name }
                if index_name.to_string() == "new_name"
        ));
    }

    #[test]
    fn preserves_on_conflict_arbiter_predicate() {
        let statement = parse(
            "INSERT INTO values_table VALUES (1, true) \
             ON CONFLICT (id) WHERE active DO NOTHING",
        )
        .unwrap()
        .pop()
        .unwrap();
        let ast::Statement::Insert(insert) = statement else {
            panic!("statement should be INSERT");
        };
        let Some(ast::OnInsert::OnConflict(conflict)) = insert.on else {
            panic!("statement should have ON CONFLICT");
        };
        let Some(ast::ConflictTarget::Columns { columns, predicate }) = conflict.conflict_target
        else {
            panic!("conflict target should contain columns");
        };

        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].value, "id");
        assert!(matches!(
            predicate,
            Some(ast::Expr::Identifier(identifier)) if identifier.value == "active"
        ));
    }

    #[test]
    fn preserves_postgres_table_lock_targets_and_mode() {
        let statement =
            parse("LOCK TABLE public.first_table, public.second_table IN EXCLUSIVE MODE")
                .unwrap()
                .pop()
                .unwrap();
        let ast::Statement::Lock(lock) = statement else {
            panic!("statement should be LOCK TABLE");
        };

        assert_eq!(lock.tables.len(), 2);
        assert_eq!(lock.tables[0].name.to_string(), "public.first_table");
        assert_eq!(lock.tables[1].name.to_string(), "public.second_table");
        assert_eq!(lock.lock_mode, Some(ast::LockTableMode::Exclusive));
        assert!(!lock.nowait);
    }

    #[test]
    #[cfg_attr(feature = "execution-log", tracing::instrument(skip_all))]
    fn classifies_statement_families() {
        let cases = [
            ("CREATE TABLE t (id INTEGER)", StatementKind::Ddl),
            ("CREATE SEQUENCE s", StatementKind::Ddl),
            ("CREATE INDEX i ON t (id)", StatementKind::Ddl),
            ("ALTER INDEX IF EXISTS i RENAME TO j", StatementKind::Ddl),
            ("INSERT INTO t VALUES (1)", StatementKind::Dml),
            ("SELECT * FROM t", StatementKind::Query),
            ("BEGIN", StatementKind::TransactionControl),
            (
                "LOCK TABLE t IN ACCESS EXCLUSIVE MODE",
                StatementKind::TransactionControl,
            ),
            ("SET application_name = 'pg_fake'", StatementKind::Set),
            ("EXPLAIN SELECT * FROM t", StatementKind::Unsupported),
        ];

        for (sql, expected) in cases {
            let statement = parse(sql).unwrap().pop().unwrap();
            assert_eq!(classify(&statement), expected, "{sql}");
        }
    }
}
