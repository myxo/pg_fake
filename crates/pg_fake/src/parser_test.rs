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
    let Some(ast::ConflictTarget::Columns { columns, predicate }) = conflict.conflict_target else {
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
    let statement = parse("LOCK TABLE public.first_table, public.second_table IN EXCLUSIVE MODE")
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
fn parses_insert_source_with_nested_parentheses() {
    parse(
        "INSERT INTO destination SELECT 1, 10 \
             ON CONFLICT (id) DO UPDATE SET value = excluded.value RETURNING id, value",
    )
    .unwrap();
    parse(
        "INSERT INTO destination (id, value) \
             ((SELECT 1, 10 LIMIT 1) UNION ALL SELECT 2, 20) \
             UNION ALL SELECT 3, 30 \
             ON CONFLICT (id) DO UPDATE SET value = excluded.value RETURNING id, value",
    )
    .unwrap();
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

mod postgres_guc_grammar {
    use sqlparser::{
        ast::{ContextModifier, Set, Statement},
        dialect::PostgreSqlDialect,
        parser::Parser,
    };

    #[test]
    fn parse_postgres_schema_and_reset_aliases() {
        for scope in ["", "SESSION ", "LOCAL "] {
            let statements = Parser::parse_sql(
                &PostgreSqlDialect {},
                &format!("SET {scope}SCHEMA 'public'"),
            )
            .unwrap();
            assert_eq!(
                statements[0].to_string(),
                format!("SET {scope}search_path = 'public'")
            );
        }
        let statements = Parser::parse_sql(
            &PostgreSqlDialect {},
            "SET LOCAL SCHEMA 'public'; RESET TIME ZONE",
        )
        .unwrap();
        let Statement::Set(Set::SingleAssignment {
            scope,
            variable,
            values,
            ..
        }) = &statements[0]
        else {
            panic!("expected SET assignment");
        };
        assert_eq!(*scope, Some(ContextModifier::Local));
        assert_eq!(variable.to_string(), "search_path");
        assert_eq!(values[0].to_string(), "'public'");
        assert_eq!(statements[1].to_string(), "RESET TimeZone");
        for sql in [
            "SET SCHEMA DEFAULT",
            "SET SCHEMA 'public', 'other'",
            "RESET TIME ZONE extra",
        ] {
            assert!(Parser::parse_sql(&PostgreSqlDialect {}, sql).is_err());
        }
    }
}
