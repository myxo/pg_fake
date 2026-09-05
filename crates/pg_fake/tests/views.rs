use pg_fake::{api::Db, error::SqlState, value::Value};

fn query_rows(session: &mut pg_fake::api::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn executes_nested_views_with_stable_metadata() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (id INTEGER, label VARCHAR(12)); \
             INSERT INTO source VALUES (1, 'one'), (2, 'two'); \
             CREATE VIEW filtered (key, name) AS \
               SELECT id, label FROM source WHERE id > 1; \
             CREATE VIEW nested AS \
               SELECT key, name FROM filtered WHERE name = 'two'",
        )
        .unwrap();

    let result = session.query("SELECT key, name FROM nested", &[]).unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Int4(2), Value::Text("two".into())]]
    );
    assert_eq!(result.columns[0].name, "key");
    assert_eq!(result.columns[1].name, "name");
    assert_eq!(result.columns[1].typmod, 16);
}

#[test]
fn preserves_view_dependencies_and_transactional_changes() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (id INTEGER, value TEXT); \
             INSERT INTO source VALUES (1, 'one'); \
             CREATE VIEW first_view AS SELECT id, value FROM source; \
             CREATE VIEW second_view AS SELECT id, value FROM first_view; \
             COMMENT ON VIEW second_view IS 'compatibility view'",
        )
        .unwrap();

    assert_eq!(
        session.execute("DROP TABLE source").unwrap_err().sqlstate,
        SqlState::DependentObjectsStillExist
    );
    assert_eq!(
        session
            .execute("DROP VIEW first_view")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    session
        .execute(
            "BEGIN; \
             CREATE OR REPLACE VIEW second_view AS SELECT id, value FROM first_view WHERE id > 9; \
             COMMENT ON VIEW second_view IS NULL; \
             DROP VIEW second_view; \
             ROLLBACK",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT * FROM second_view"),
        vec![vec![Value::Int4(1), Value::Text("one".into())]]
    );
    assert_eq!(
        session
            .execute("CREATE OR REPLACE VIEW second_view AS SELECT value, id FROM first_view")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTableDefinition
    );
    assert_eq!(
        session
            .execute("UPDATE second_view SET value = 'changed'")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
}

#[test]
fn follows_table_and_column_renames() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (id INTEGER, value TEXT); \
             INSERT INTO source VALUES (1, 'one'); \
             CREATE VIEW stable_view AS SELECT source.id, value FROM source; \
             ALTER TABLE source RENAME COLUMN value TO label; \
             ALTER TABLE source RENAME TO renamed_source",
        )
        .unwrap();

    assert_eq!(
        query_rows(&mut session, "SELECT id, value FROM stable_view"),
        vec![vec![Value::Int4(1), Value::Text("one".into())]]
    );
    assert_eq!(
        session
            .execute("ALTER TABLE renamed_source DROP COLUMN label")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
}

#[test]
fn rejects_unsupported_or_unresolved_trigger_definitions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE accounts (id INTEGER)")
        .unwrap();

    for (sql, expected) in [
        (
            "CREATE TRIGGER audit_changes BEFORE INSERT ON accounts \
             FOR EACH ROW EXECUTE FUNCTION missing()",
            SqlState::UndefinedFunction,
        ),
        (
            "CREATE TRIGGER row_truncate AFTER TRUNCATE ON accounts \
             FOR EACH ROW EXECUTE FUNCTION missing()",
            SqlState::FeatureNotSupported,
        ),
        (
            "CREATE TRIGGER missing_column BEFORE UPDATE OF unknown ON accounts \
             FOR EACH ROW EXECUTE FUNCTION missing()",
            SqlState::FeatureNotSupported,
        ),
        (
            "CREATE TRIGGER missing_table BEFORE INSERT ON unknown \
             FOR EACH ROW EXECUTE FUNCTION missing()",
            SqlState::UndefinedTable,
        ),
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            expected,
            "SQL: {sql}"
        );
    }
}

#[test]
fn rejects_recursive_and_parameterized_view_definitions() {
    let db = Db::create();
    let mut session = db.create_session();
    assert_eq!(
        session
            .execute("CREATE VIEW recursive_view AS SELECT * FROM recursive_view")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    assert_eq!(
        session
            .prepare("CREATE VIEW parameterized_view AS SELECT $1 AS value")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedParameter
    );
}

#[test]
fn stored_view_relations_are_not_changed_by_temporary_shadowing() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (id INTEGER); \
             INSERT INTO source VALUES (1); \
             CREATE VIEW stable_view AS SELECT id FROM source; \
             CREATE TEMP TABLE source (id INTEGER); \
             INSERT INTO source VALUES (2)",
        )
        .unwrap();

    assert_eq!(
        query_rows(&mut session, "SELECT id FROM stable_view"),
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
fn table_rename_preserves_an_explicit_alias_matching_the_old_name() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (id INTEGER); \
             INSERT INTO source VALUES (7); \
             CREATE VIEW aliased_view AS SELECT source.id FROM source AS source; \
             ALTER TABLE source RENAME TO renamed_source",
        )
        .unwrap();

    assert_eq!(
        query_rows(&mut session, "SELECT id FROM aliased_view"),
        vec![vec![Value::Int4(7)]]
    );
}

#[test]
fn freezes_wildcards_when_base_tables_gain_columns() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (a INTEGER); \
             INSERT INTO source VALUES (1); \
             CREATE VIEW stable_view AS SELECT * FROM source; \
             ALTER TABLE source ADD COLUMN b INTEGER DEFAULT 2",
        )
        .unwrap();

    let result = session.query("SELECT * FROM stable_view", &[]).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int4(1)]]);
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.columns[0].name, "a");
}

#[test]
fn tracks_nonrecursive_cte_self_names_as_catalog_dependencies() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE shadow (a INTEGER); \
             CREATE VIEW shadow_view AS \
               WITH shadow AS (SELECT a FROM shadow) SELECT a FROM shadow",
        )
        .unwrap();

    assert_eq!(
        session.execute("DROP TABLE shadow").unwrap_err().sqlstate,
        SqlState::DependentObjectsStillExist
    );
}

#[test]
fn renames_only_bound_relations_and_columns() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE source (id INTEGER); \
             INSERT INTO source VALUES (1); \
             CREATE VIEW cte_view AS \
               WITH source AS (SELECT 9 AS id) SELECT source.id FROM public.source; \
             ALTER TABLE source RENAME TO renamed_source; \
             CREATE TABLE target_table (a INTEGER); \
             CREATE TABLE other_table (x INTEGER); \
             INSERT INTO target_table VALUES (2); \
             INSERT INTO other_table VALUES (3); \
             CREATE VIEW union_view AS \
               SELECT a FROM target_table \
               UNION ALL SELECT a FROM (SELECT x AS a FROM other_table) q; \
             ALTER TABLE target_table RENAME COLUMN a TO b",
        )
        .unwrap();

    assert_eq!(
        query_rows(&mut session, "SELECT * FROM cte_view"),
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT * FROM union_view ORDER BY a"),
        vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
    );
}

#[test]
fn tracks_sequence_and_primary_key_dependencies() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE SEQUENCE view_sequence; \
             CREATE VIEW sequence_view AS SELECT nextval('view_sequence') AS value; \
             CREATE TABLE grouped_source (key INTEGER PRIMARY KEY, data TEXT); \
             CREATE VIEW grouped_view AS \
               SELECT key, data FROM grouped_source GROUP BY key",
        )
        .unwrap();

    assert_eq!(
        session
            .execute("DROP SEQUENCE view_sequence")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    assert_eq!(
        session
            .execute("ALTER TABLE grouped_source DROP CONSTRAINT grouped_source_pkey")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
}

#[test]
fn supports_temporary_views_without_leaking_temporary_dependencies() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TEMP TABLE temporary_source (id INTEGER); \
             INSERT INTO temporary_source VALUES (4); \
             CREATE TEMP VIEW temporary_view AS SELECT id FROM temporary_source; \
             CREATE VIEW pg_temp.qualified_temporary_view AS \
               SELECT id FROM temporary_source",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT id FROM qualified_temporary_view"),
        vec![vec![Value::Int4(4)]]
    );
    assert_eq!(
        session
            .execute("CREATE VIEW invalid_public_view AS SELECT id FROM temporary_view")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTableDefinition
    );
}

#[test]
fn comments_do_not_invalidate_prepared_view_queries() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE VIEW commented_view AS SELECT 1 AS value")
        .unwrap();
    let prepared = session.prepare("SELECT value FROM commented_view").unwrap();
    session
        .execute("COMMENT ON VIEW commented_view IS 'documentation only'")
        .unwrap();

    assert_eq!(
        session.query_prepared(&prepared, &[]).unwrap().rows,
        vec![vec![Value::Int4(1)]]
    );
}

#[test]
fn distinguishes_used_and_unused_column_dependencies() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE dependency_source (a INTEGER, b INTEGER); \
             CREATE VIEW dependency_view AS SELECT a FROM dependency_source; \
             ALTER TABLE dependency_source DROP COLUMN b; \
             CREATE TABLE cte_false_dependency (a INTEGER, b INTEGER); \
             INSERT INTO cte_false_dependency VALUES (4, 9); \
             CREATE VIEW cte_false_dependency_view AS \
               WITH cte_false_dependency AS ( \
                 SELECT a + 1 AS b FROM cte_false_dependency \
               ) \
               SELECT b FROM cte_false_dependency; \
             ALTER TABLE cte_false_dependency DROP COLUMN b; \
             CREATE TABLE alias_drop_source (unused INTEGER, value INTEGER); \
             INSERT INTO alias_drop_source VALUES (8, 12); \
             CREATE VIEW alias_drop_view AS \
               SELECT exposed FROM alias_drop_source AS source (unused_alias, exposed); \
             ALTER TABLE alias_drop_source DROP COLUMN unused",
        )
        .unwrap();
    assert_eq!(
        session
            .execute("ALTER TABLE dependency_source ALTER COLUMN a TYPE BIGINT")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        query_rows(&mut session, "SELECT b FROM cte_false_dependency_view"),
        vec![vec![Value::Int4(5)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT exposed FROM alias_drop_view"),
        vec![vec![Value::Int4(12)]]
    );
}

#[test]
fn preserves_correlated_and_using_column_bindings_across_renames() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE correlated_source (a INTEGER); \
             INSERT INTO correlated_source VALUES (5); \
             CREATE VIEW correlated_view AS \
               SELECT (SELECT a) AS nested FROM correlated_source; \
             ALTER TABLE correlated_source RENAME COLUMN a TO b; \
             ALTER TABLE correlated_source RENAME COLUMN b TO c; \
             CREATE TABLE using_left (a INTEGER); \
             CREATE TABLE using_right (a INTEGER); \
             INSERT INTO using_left VALUES (7); \
             INSERT INTO using_right VALUES (7); \
             CREATE VIEW using_view AS \
               SELECT a FROM using_left JOIN using_right USING (a); \
             ALTER TABLE using_left RENAME COLUMN a TO b; \
             ALTER TABLE using_left RENAME COLUMN b TO c; \
             CREATE TABLE aliased_source (a INTEGER); \
             INSERT INTO aliased_source VALUES (9); \
             CREATE VIEW aliased_column_view AS \
               SELECT exposed FROM aliased_source AS source (exposed); \
             ALTER TABLE aliased_source RENAME COLUMN a TO b; \
             CREATE TABLE positional_alias_source (ignored INTEGER, a INTEGER); \
             INSERT INTO positional_alias_source VALUES (1, 11); \
             CREATE VIEW positional_alias_view AS \
               SELECT a FROM positional_alias_source AS source (unused_alias); \
             ALTER TABLE positional_alias_source RENAME COLUMN a TO b; \
             CREATE TABLE trailing_alias_source (a INTEGER, b INTEGER, c INTEGER); \
             INSERT INTO trailing_alias_source VALUES (13, 14, 15); \
             CREATE VIEW trailing_alias_view AS \
               SELECT first FROM trailing_alias_source AS source (first, second, third); \
             ALTER TABLE trailing_alias_source RENAME COLUMN a TO renamed; \
             CREATE TABLE cte_shadow_source (a INTEGER); \
             INSERT INTO cte_shadow_source VALUES (1); \
             CREATE VIEW cte_shadow_view AS \
               WITH cte_shadow_source AS (SELECT a + 10 AS a FROM cte_shadow_source) \
               SELECT a FROM cte_shadow_source; \
             ALTER TABLE cte_shadow_source RENAME COLUMN a TO b; \
             CREATE TABLE cte_table_rename (a INTEGER); \
             INSERT INTO cte_table_rename VALUES (2); \
             CREATE VIEW cte_table_rename_view AS \
               WITH cte_table_rename AS (SELECT a + 10 AS a FROM cte_table_rename) \
               SELECT a FROM cte_table_rename; \
             ALTER TABLE cte_table_rename RENAME TO cte_table_renamed; \
             CREATE TABLE nested_left (a INTEGER, unused INTEGER); \
             CREATE TABLE nested_right (b INTEGER); \
             INSERT INTO nested_left VALUES (17, 19); \
             INSERT INTO nested_right VALUES (18); \
             CREATE VIEW nested_alias_view AS \
               SELECT first FROM (nested_left JOIN nested_right ON true) \
                 AS joined (first, second, third); \
             ALTER TABLE nested_left RENAME COLUMN a TO c",
        )
        .unwrap();

    assert_eq!(
        query_rows(&mut session, "SELECT nested FROM correlated_view"),
        vec![vec![Value::Int4(5)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT a FROM using_view"),
        vec![vec![Value::Int4(7)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT exposed FROM aliased_column_view"),
        vec![vec![Value::Int4(9)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT a FROM positional_alias_view"),
        vec![vec![Value::Int4(11)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT first FROM trailing_alias_view"),
        vec![vec![Value::Int4(13)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT a FROM cte_shadow_view"),
        vec![vec![Value::Int4(11)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT a FROM cte_table_rename_view"),
        vec![vec![Value::Int4(12)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT first FROM nested_alias_view"),
        vec![vec![Value::Int4(17)]]
    );

    session
        .execute(
            "CREATE TABLE multi_rename_source (a INTEGER, b INTEGER); \
             INSERT INTO multi_rename_source VALUES (20, 21); \
             CREATE VIEW multi_rename_view AS SELECT a, b FROM multi_rename_source",
        )
        .unwrap();
    session
        .execute(
            "ALTER TABLE multi_rename_source \
               RENAME COLUMN a TO x, RENAME COLUMN b TO y",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT a, b FROM multi_rename_view"),
        vec![vec![Value::Int4(20), Value::Int4(21)]]
    );
    session
        .execute(
            "CREATE TABLE combined_rename_source (a INTEGER); \
             INSERT INTO combined_rename_source VALUES (30); \
             CREATE VIEW combined_rename_view AS SELECT a FROM combined_rename_source; \
             ALTER TABLE combined_rename_source \
               RENAME TO combined_renamed, RENAME COLUMN a TO b",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT a FROM combined_rename_view"),
        vec![vec![Value::Int4(30)]]
    );

    session
        .execute(
            "CREATE TABLE drop_left (a INTEGER); \
             CREATE TABLE drop_right (a INTEGER); \
             CREATE VIEW drop_using_view AS \
               SELECT a FROM drop_left NATURAL JOIN drop_right",
        )
        .unwrap();
    assert_eq!(
        session
            .execute("ALTER TABLE drop_left DROP COLUMN a")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
}

#[test]
fn records_only_primary_keys_used_for_grouping() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE positional_source (key INTEGER PRIMARY KEY, data TEXT); \
             CREATE VIEW positional_view AS \
               SELECT key, data FROM positional_source GROUP BY 1; \
             CREATE TABLE alias_source (key INTEGER PRIMARY KEY, data TEXT); \
             CREATE VIEW alias_view AS \
               SELECT key AS grouped_key, data FROM alias_source GROUP BY grouped_key; \
             CREATE TABLE unnecessary_source (key INTEGER PRIMARY KEY, data TEXT); \
             CREATE VIEW unnecessary_view AS \
               SELECT key FROM unnecessary_source GROUP BY key; \
             CREATE TABLE ordered_source (key INTEGER PRIMARY KEY, data TEXT); \
             CREATE VIEW ordered_view AS \
               SELECT key, count(*) FROM ordered_source GROUP BY key ORDER BY data",
        )
        .unwrap();

    for (table, constraint) in [
        ("positional_source", "positional_source_pkey"),
        ("alias_source", "alias_source_pkey"),
        ("ordered_source", "ordered_source_pkey"),
    ] {
        assert_eq!(
            session
                .execute(&format!("ALTER TABLE {table} DROP CONSTRAINT {constraint}"))
                .unwrap_err()
                .sqlstate,
            SqlState::DependentObjectsStillExist
        );
    }
    session
        .execute("ALTER TABLE unnecessary_source DROP CONSTRAINT unnecessary_source_pkey")
        .unwrap();
}

#[test]
fn rejects_permanent_views_over_temporary_sequences() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TEMP SEQUENCE temp_sequence")
        .unwrap();

    assert_eq!(
        session
            .execute("CREATE VIEW public.invalid_view AS SELECT nextval('temp_sequence') AS n")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTableDefinition
    );
    session
        .execute("CREATE TEMP VIEW valid_view AS SELECT nextval('temp_sequence') AS n")
        .unwrap();
}
