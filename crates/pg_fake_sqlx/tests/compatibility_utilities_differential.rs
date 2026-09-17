use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_recovery_lsn_and_catalog_utilities() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());

    for sql in [
        "SELECT pg_is_in_recovery(), pg_catalog.pg_is_in_recovery()",
        "SELECT '0/0'::pg_lsn::text, '2B/1757980'::pg_lsn::text, 'FFFFFFFF/FFFFFFFF'::pg_lsn::text",
        "SELECT '0/16AE7F8'::pg_lsn > '0/16AE7F7'::pg_lsn, '0/16AE7F8'::pg_lsn - '0/16AE7F7'::pg_lsn",
        "SELECT ('0/16AE7F7'::pg_lsn + 16::numeric)::text, (16::numeric + '0/16AE7F7'::pg_lsn)::text, ('0/16AE807'::pg_lsn - 16::numeric)::text",
        "SELECT ('0/0'::pg_lsn + 9223372036854775808::numeric)::text, ('0/0'::pg_lsn + 18446744073709551615::numeric)::text, ('FFFFFFFF/FFFFFFFF'::pg_lsn + -9223372036854775808::numeric)::text",
        "SELECT format_type(16, -1), format_type(23, -1), format_type(1043, 16), format_type(3220, -1), format_type(2205, -1)",
        "SELECT format_type(1043, 0)",
        "SELECT format_type(1700, 657410)",
        "SELECT typname, typcategory FROM pg_catalog.pg_type WHERE oid IN (16, 23, 2205, 3220) ORDER BY oid",
        "SELECT typcategory FROM pg_catalog.pg_type WHERE typname = 'interval'",
        "SELECT oid, typarray FROM pg_catalog.pg_type WHERE oid IN (16, 20, 23, 25, 2950) ORDER BY oid",
    ] {
        let order = if sql.contains("FROM pg_catalog.pg_type") {
            RowOrder::Unordered
        } else {
            RowOrder::Ordered
        };
        assert_statement(&runtime, &mut postgres, &mut fake, sql, order);
    }

    for sql in [
        "SELECT 'G/0'::pg_lsn",
        "SELECT '100000000/0'::pg_lsn",
        "SELECT '0/0'::pg_lsn - 1::numeric",
        "SELECT 'FFFFFFFF/FFFFFFFF'::pg_lsn + 1::numeric",
        "SELECT '0/0'::pg_lsn + 18446744073709551616::numeric",
        "SELECT format_type(4294967295, -1)",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn matches_regclass_lookup_and_catalog_introspection() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());

    for sql in [
        "CREATE TABLE public.catalog_items (id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name VARCHAR(12) NOT NULL)",
        "CREATE INDEX catalog_items_name_idx ON public.catalog_items (name)",
        "CREATE VIEW public.catalog_items_view AS SELECT id, name FROM public.catalog_items",
        "CREATE SEQUENCE public.catalog_items_sequence",
        "CREATE TABLE public.\"Quoted Item\" (id INTEGER)",
        "CREATE TABLE public.\"select\" (id INTEGER)",
        "CREATE TABLE public.\" padded \" (id INTEGER)",
        "CREATE TABLE public.\"foo-bar\" (id INTEGER)",
        "CREATE TABLE public.\"foo bar\" (id INTEGER)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    for sql in [
        "SELECT to_regclass('catalog_items')::text, to_regclass('catalog_items_view')::text, to_regclass('catalog_items_sequence')::text, to_regclass('catalog_items_name_idx')::text",
        "SELECT to_regclass('\"Quoted Item\"')::text, to_regclass('missing_relation')",
        "SELECT 'catalog_items_pkey'::regclass::text, '4294967295'::regclass::text",
        "SELECT c.relname, c.relkind, c.relpersistence FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND c.relname IN ('catalog_items', 'catalog_items_view', 'catalog_items_sequence', 'catalog_items_name_idx', 'Quoted Item') ORDER BY c.relname",
        "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, a.attidentity FROM pg_catalog.pg_attribute AS a WHERE a.attrelid = 'catalog_items'::regclass AND a.attnum > 0 ORDER BY a.attnum",
        "SELECT 'pg_catalog.pg_class'::regclass::text, to_regclass('pg_catalog.pg_attribute')::text",
        "SELECT '\"select\"'::regclass::text, to_regclass('missing_schema.item')",
        "SELECT '\" padded \"'::regclass::text",
        "SELECT to_regclass('foo-bar')::text",
        "SELECT to_regclass('foo bar')",
        "SELECT to_regclass('a..b')",
        "SELECT '-'::regclass::text, '0'::regclass::text",
        "SELECT to_regclass('4294967296')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "BEGIN",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "CREATE TABLE transactional_catalog_item (id INTEGER)",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT to_regclass('transactional_catalog_item') IS NOT NULL",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "ROLLBACK",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT to_regclass('transactional_catalog_item')",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "CREATE TEMP TABLE catalog_items (id INTEGER PRIMARY KEY)",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT to_regclass('catalog_items') = to_regclass('pg_temp.catalog_items'), to_regclass('public.catalog_items')::text",
        RowOrder::Ordered,
    );
    for sql in [
        "SET search_path TO public, pg_temp",
        "SHOW search_path",
        "SELECT to_regclass('catalog_items') = to_regclass('public.catalog_items')",
        "SELECT to_regclass('catalog_items_pkey') = to_regclass('public.catalog_items_pkey')",
        "CREATE TEMP VIEW temporary_catalog_view AS SELECT 1 AS id",
        "CREATE TEMP SEQUENCE temporary_catalog_sequence",
        "SELECT relname, relpersistence FROM pg_catalog.pg_class WHERE relname IN ('temporary_catalog_view', 'temporary_catalog_sequence') ORDER BY relname",
        "CREATE TEMP TABLE pg_class (marker INTEGER)",
        "SELECT relname FROM pg_class WHERE oid = 'pg_catalog.pg_class'::regclass",
        "SET search_path TO pg_temp, pg_catalog, public",
        "SHOW search_path",
        "INSERT INTO pg_class VALUES (7)",
        "SELECT marker FROM pg_class",
        "SELECT to_regclass('catalog_items_pkey') = to_regclass('pg_temp.catalog_items_pkey')",
        "SELECT relname FROM pg_catalog.pg_class WHERE oid = 'pg_catalog.pg_class'::regclass",
        "SET search_path TO public",
        "SELECT marker, 'pg_catalog.pg_class'::regclass::text FROM pg_class",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    for sql in [
        "SELECT 'missing_relation'::regclass",
        "SELECT 'missing_schema.item'::regclass",
        "SELECT to_regclass('a.b.c')",
        "SELECT 'a.b.c'::regclass",
        "SELECT 'a..b'::regclass",
        "SELECT 'foo bar'::regclass",
        "SELECT '4294967296'::regclass",
        "SELECT unsupported_column FROM pg_catalog.pg_class",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    for sql in [
        "CREATE TABLE public.shared_name (id INTEGER)",
        "CREATE TEMP TABLE later_constraint_owner (id INTEGER CONSTRAINT shared_name PRIMARY KEY)",
        "SET search_path TO public, pg_temp",
        "SELECT to_regclass('shared_name') = to_regclass('public.shared_name')",
        "SET search_path TO public",
        "CREATE TABLE relation_owner (id INTEGER CONSTRAINT occupied_name PRIMARY KEY)",
        "CREATE TABLE occupied_addition (id INTEGER)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "CREATE TABLE occupied_name (id INTEGER)",
        "CREATE INDEX occupied_name ON relation_owner (id)",
        "ALTER TABLE relation_owner ADD CONSTRAINT occupied_addition UNIQUE (id)",
        "ALTER TABLE relation_owner ADD COLUMN extra INTEGER CONSTRAINT occupied_addition UNIQUE",
        "CREATE TABLE self_collision (id INTEGER CONSTRAINT self_collision PRIMARY KEY)",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "CREATE TABLE generated_item_pkey (id INTEGER)",
        "CREATE TABLE generated_item (id INTEGER PRIMARY KEY)",
        "CREATE TABLE altered_item_pkey (id INTEGER)",
        "CREATE TABLE altered_item (id INTEGER)",
        "ALTER TABLE altered_item ADD PRIMARY KEY (id)",
        "CREATE TABLE column_item_extra_key (id INTEGER)",
        "CREATE TABLE column_item (id INTEGER)",
        "ALTER TABLE column_item ADD COLUMN extra INTEGER UNIQUE",
        "SELECT to_regclass('generated_item_pkey1') IS NOT NULL, to_regclass('altered_item_pkey1') IS NOT NULL, to_regclass('column_item_extra_key1') IS NOT NULL",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SET search_path TO nonexistent_schema",
        "SELECT to_regclass('missing_item')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "CREATE TABLE no_schema_selected (id INTEGER)",
        RowOrder::Ordered,
    );
    for sql in [
        "SET search_path TO pg_temp",
        "CREATE TABLE path_temporary (id INTEGER)",
        "SELECT relpersistence FROM pg_catalog.pg_class WHERE oid = 'pg_temp.path_temporary'::regclass",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn matches_transactional_foreign_key_and_identity_truncate() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());

    for sql in [
        "CREATE TABLE truncate_parents (id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY)",
        "CREATE TABLE truncate_children (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES truncate_parents(id))",
        "INSERT INTO truncate_parents DEFAULT VALUES",
        "INSERT INTO truncate_children VALUES (1, 1)",
        "TRUNCATE truncate_parents",
        "BEGIN",
        "TRUNCATE truncate_parents CASCADE",
        "SELECT count(*) FROM truncate_parents",
        "SELECT count(*) FROM truncate_children",
        "ROLLBACK",
        "SELECT count(*) FROM truncate_parents",
        "SELECT count(*) FROM truncate_children",
        "TRUNCATE truncate_parents, truncate_children RESTART IDENTITY RESTRICT",
        "INSERT INTO truncate_parents DEFAULT VALUES RETURNING id",
        "INSERT INTO truncate_children VALUES (2, 1)",
        "TRUNCATE truncate_parents, truncate_children CONTINUE IDENTITY",
        "INSERT INTO truncate_parents DEFAULT VALUES RETURNING id",
    ] {
        if sql == "TRUNCATE truncate_parents" {
            assert_statement_allow_error(
                &runtime,
                &mut postgres,
                &mut fake,
                sql,
                RowOrder::Ordered,
            );
        } else {
            assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
        }
    }

    for sql in [
        "CREATE TEMP TABLE temporary_truncate (id INTEGER)",
        "INSERT INTO temporary_truncate VALUES (1)",
        "TRUNCATE pg_temp.temporary_truncate",
        "SELECT count(*) FROM temporary_truncate",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "TRUNCATE temporary_truncate, temporary_truncate",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_relation_wide_truncate_visibility() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres_reader = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut postgres_writer = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake_reader = PgFakeConnection::new(db.clone());
    let mut fake_writer = PgFakeConnection::new(db);

    for sql in [
        "CREATE TABLE truncate_snapshot (id INTEGER)",
        "INSERT INTO truncate_snapshot VALUES (1)",
    ] {
        assert_statement(
            &runtime,
            &mut postgres_writer,
            &mut fake_writer,
            sql,
            RowOrder::Ordered,
        );
    }
    assert_statement(
        &runtime,
        &mut postgres_reader,
        &mut fake_reader,
        "BEGIN ISOLATION LEVEL REPEATABLE READ",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres_reader,
        &mut fake_reader,
        "SELECT 1",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres_writer,
        &mut fake_writer,
        "TRUNCATE truncate_snapshot",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres_reader,
        &mut fake_reader,
        "SELECT count(*) FROM truncate_snapshot",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres_reader,
        &mut fake_reader,
        "ROLLBACK",
        RowOrder::Ordered,
    );
}
