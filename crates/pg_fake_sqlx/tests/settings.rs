use pg_fake::{Db, error::SqlState, value::Value};
use pg_fake_sqlx::PgFakeConnection;
use sqlx::{Column, Connection, Executor, TypeInfo};
use sqlx_postgres::{PgConnectOptions, PgConnection};
use std::{str::FromStr, time::Duration};

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{RowOrder, assert_statement, assert_statement_allow_error};

#[test]
fn compare_registered_settings_with_postgres() {
    let server = differential::start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let options = PgConnectOptions::from_str(&server.url)
        .unwrap()
        .application_name("")
        .options([("lock_timeout", "1000"), ("timezone", "UTC")]);
    let mut postgres = runtime
        .block_on(PgConnection::connect_with(&options))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for (name, values) in [
        (
            "lock_timeout",
            vec![
                "'1s'",
                "'1.5min'",
                "'0.5ms'",
                "'1.0001min'",
                "'0x1e'",
                "2147483647",
                "0",
            ],
        ),
        ("statement_timeout", vec!["'30min'", "'1.5s'", "0"]),
        (
            "TimeZone",
            vec![
                "'utc'",
                "'europe/paris'",
                "'+03:00'",
                "3.5",
                "-7",
                "24",
                "100",
                "167.9999",
                "3.0001",
                "'America/New_York'",
                "'UTC+3'",
            ],
        ),
        (
            "default_transaction_isolation",
            vec!["'REPEATABLE READ'", "'read committed'"],
        ),
        (
            "search_path",
            vec![
                "public",
                "'a,b'",
                "'UPPER'",
                "'select'",
                "\"UPPER\", '$user', ''",
                "'\"a,b\", public'",
                "pg_temp, public",
            ],
        ),
        (
            "application_name",
            vec![
                "'pg_fake tests'",
                "E'worker\\tname'",
                "$$dollar quoted$$",
                "'héllo'",
                "''",
                "true",
                "123",
            ],
        ),
        ("client_encoding", vec!["'UTF-8'", "unicode", "'uTf8'"]),
        (
            "work_mem",
            vec![
                "64",
                "'1.5MB'",
                "'0100'",
                "'0x100'",
                "'0x100B'",
                "'2GB'",
                "'1.0005GB'",
                "'1025kB'",
            ],
        ),
        ("effective_cache_size", vec!["'1GB'", "'100kB'", "'8192B'"]),
        ("min_parallel_table_scan_size", vec!["0", "'8MB'"]),
        ("min_parallel_index_scan_size", vec!["0", "'1MB'"]),
        ("random_page_cost", vec!["0", "1.25", "'2e1'"]),
        ("seq_page_cost", vec!["0.5"]),
        ("cpu_tuple_cost", vec!["0.02"]),
        ("cpu_index_tuple_cost", vec!["0.01"]),
        ("cpu_operator_cost", vec!["0.005"]),
        ("parallel_setup_cost", vec!["100"]),
        ("parallel_tuple_cost", vec!["0.2"]),
        ("join_collapse_limit", vec!["1", "'0x10'"]),
        ("from_collapse_limit", vec!["12"]),
        (
            "plan_cache_mode",
            vec!["force_generic_plan", "'FORCE_CUSTOM_PLAN'", "auto"],
        ),
        ("geqo", vec!["off", "'yes'", "'n'", "'of'", "1"]),
        ("enable_hashjoin", vec!["'t'", "false", "0", "'ON'"]),
        ("enable_partitionwise_join", vec!["true"]),
        ("jit_above_cost", vec!["-1", "1.5"]),
        ("jit_inline_above_cost", vec!["0"]),
        ("jit_optimize_above_cost", vec!["123"]),
        ("jit_expressions", vec!["off"]),
        ("jit_tuple_deforming", vec!["off"]),
    ] {
        for value in values {
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("SET SESSION \"{name}\" = {value}"),
                RowOrder::Unordered,
            );
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("SHOW \"{name}\""),
                RowOrder::Ordered,
            );
        }
        for sql in [
            format!("RESET \"{name}\""),
            format!("SHOW \"{name}\""),
            format!("SET \"{name}\" TO DEFAULT"),
            format!("SHOW \"{name}\""),
        ] {
            assert_statement(&runtime, &mut postgres, &mut fake, &sql, RowOrder::Ordered);
        }
    }
    for sql in [
        "SET TIME ZONE 'Europe/Paris'",
        "SHOW TIME ZONE",
        "SET SCHEMA 'public'",
        "SHOW search_path",
        "SET NAMES 'unicode'",
        "SHOW client_encoding",
        "SET NAMES DEFAULT",
        "RESET TIME ZONE",
        "SHOW TIME ZONE",
        "SET application_name = 'changed'",
        "SET enable_seqscan = off",
        "RESET ALL",
        "SHOW application_name",
        "SHOW enable_seqscan",
        "SHOW lock_timeout",
        "SHOW search_path",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SET lock_timeout = '-1'",
        "SET lock_timeout = '2147483648'",
        "SET lock_timeout = '2fortnights'",
        "SET work_mem = '1mb'",
        "SET work_mem = '10'",
        "SET work_mem = 'NaN'",
        "SET enable_hashjoin = 'o'",
        "SET enable_hashjoin = 'nope'",
        "SET random_page_cost = '-1'",
        "SET plan_cache_mode = nonsense",
        "SET default_transaction_isolation = 'nonsense'",
        "SET timezone = 'not/a/zone'",
        "SET client_encoding = 'nonsense'",
        "SET enable_nonexistent = on",
        "SHOW nonexistent",
        "SHOW \"time zone\"",
        "SET \"time zone\" = 'UTC'",
        "RESET nonexistent",
        "SET jit_provider = 'foo'",
        "RESET jit_debugging_support",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn compare_transactional_settings_and_prepared_metadata() {
    let server = differential::start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        "SET application_name = 'base'",
        "BEGIN",
        "SET application_name = 'committed'",
        "SAVEPOINT first",
        "SET application_name = 'discarded'",
        "SET LOCAL work_mem = '8MB'",
        "ROLLBACK TO first",
        "SHOW application_name",
        "COMMIT",
        "SHOW application_name",
        "BEGIN",
        "RESET application_name",
        "SHOW application_name",
        "ROLLBACK",
        "SHOW application_name",
        "BEGIN",
        "SET LOCAL application_name = 'local'",
        "SHOW application_name",
        "COMMIT",
        "SHOW application_name",
        "SET timezone = 'America/New_York'",
        "SELECT TIMESTAMP '2024-01-01 12:00'::timestamptz AT TIME ZONE 'UTC'",
        "SET timezone = 'CET'",
        "SELECT to_char(TIMESTAMPTZ '2024-07-01 12:00:00+00', 'YYYY-MM-DD HH24:MI')",
        "SELECT date_trunc('day', TIMESTAMPTZ '2024-07-01 12:00:00+00') AT TIME ZONE 'UTC'",
        "SET timezone = 'Europe/Paris'",
        "SELECT TIMESTAMP '2024-07-01 12:00'::timestamptz AT TIME ZONE 'UTC'",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    runtime.block_on(async {
        for sql in [
            "SHOW TIME ZONE",
            "SHOW search_path",
            "SHOW application_name",
            "SHOW lock_timeout",
        ] {
            let expected = postgres.describe(sql).await.unwrap();
            let actual = fake.describe(sql).await.unwrap();
            assert_eq!(actual.columns().len(), expected.columns().len());
            for (actual, expected) in actual.columns().iter().zip(expected.columns()) {
                assert_eq!(actual.name(), expected.name());
                assert_eq!(actual.type_info().name(), expected.type_info().name());
            }
        }
        for zone in ["UTC", "America/New_York", "Europe/Paris"] {
            let sql = format!("SET timezone = '{zone}'");
            sqlx::raw_sql(&sql).execute(&mut postgres).await.unwrap();
            sqlx::raw_sql(&sql).execute(&mut fake).await.unwrap();
            let expected: String = sqlx::query_scalar("SHOW TIME ZONE")
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual: String = sqlx::query_scalar("SHOW TIME ZONE")
                .fetch_one(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
            let sql = "SELECT CAST($1 AS timestamp)::timestamptz";
            let expected: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(sql)
                .bind(
                    chrono::NaiveDate::from_ymd_opt(2024, 7, 1)
                        .unwrap()
                        .and_hms_opt(12, 0, 0)
                        .unwrap(),
                )
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(sql)
                .bind(
                    chrono::NaiveDate::from_ymd_opt(2024, 7, 1)
                        .unwrap()
                        .and_hms_opt(12, 0, 0)
                        .unwrap(),
                )
                .fetch_one(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
    });
}

#[test]
fn compare_guc_functions_and_transaction_isolation() {
    let server = differential::start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        "SET application_name = 'base'",
        "SELECT current_setting('application_name')",
        "BEGIN",
        "SELECT set_config('application_name', 'local', true)",
        "SAVEPOINT guc",
        "SELECT set_config('application_name', 'discarded', false)",
        "SELECT current_setting('application_name')",
        "ROLLBACK TO guc",
        "SELECT current_setting('application_name')",
        "COMMIT",
        "SELECT current_setting('application_name')",
        "BEGIN",
        "SELECT set_config('application_name', 'committed', false)",
        "SELECT set_config('application_name', 'temporary', true)",
        "SELECT current_setting('application_name')",
        "COMMIT",
        "SELECT current_setting('application_name')",
        "SELECT current_setting('missing.parameter', true)",
        "SELECT set_config('custom.setting', 'value', false)",
        "SELECT current_setting('custom.setting')",
        "SELECT set_config('custom.a$b', 'dollar', false)",
        "SELECT current_setting('custom.a$b')",
        "SELECT set_config('custom.😀', 'unicode', false)",
        "SELECT current_setting('custom.😀')",
        "BEGIN",
        "SELECT set_config('custom.first_local', 'local', true)",
        "COMMIT",
        "SELECT current_setting('custom.first_local', true)",
        "BEGIN",
        "SELECT set_config('custom.first_rollback', 'rollback', false)",
        "ROLLBACK",
        "SELECT current_setting('custom.first_rollback', true)",
        "BEGIN",
        "SAVEPOINT custom_first",
        "SELECT set_config('custom.first_savepoint', 'savepoint', false)",
        "ROLLBACK TO custom_first",
        "SELECT current_setting('custom.first_savepoint', true)",
        "ROLLBACK",
        "SELECT current_setting('custom.first_savepoint', true)",
        "SELECT set_config('application_name', 'reset-me', false)",
        "SELECT set_config('application_name', NULL, false)",
        "SELECT current_setting('application_name')",
        "SELECT set_config('application_name', 'null-local-flag', NULL)",
        "SELECT current_setting('application_name')",
        "SELECT set_config('custom.reset_value', 'reset-me', false)",
        "SELECT set_config('custom.reset_value', NULL, false)",
        "SELECT current_setting('custom.reset_value')",
        "SELECT set_config('custom.reset_direct', 'reset-me', false)",
        "RESET custom.reset_direct",
        "SELECT current_setting('custom.reset_direct')",
        "SELECT set_config('custom.reset_all', 'reset-me', false)",
        "RESET ALL",
        "SELECT current_setting('custom.reset_all')",
        "SELECT set_config('search_path', '', false)",
        "SELECT current_setting('search_path')",
        "SELECT set_config('search_path', '   ', false)",
        "SELECT current_setting('search_path')",
        "SELECT set_config('search_path', '\" a \"', false)",
        "SELECT current_setting('search_path')",
        "SELECT set_config('search_path', 'ABC, $user, a$b, é, 😀', false)",
        "SELECT current_setting('search_path')",
        "RESET search_path",
        "BEGIN",
        "SELECT set_config('TimeZone', 'Europe/Paris', true), current_setting('TimeZone'), to_char(TIMESTAMPTZ '2024-07-01 12:00:00+00', 'HH24:MI')",
        "SELECT set_config('search_path', '\"Mixed\", public', true)",
        "SELECT current_setting('search_path')",
        "ROLLBACK",
        "SET default_transaction_isolation = 'repeatable read'",
        "SHOW transaction_isolation",
        "BEGIN ISOLATION LEVEL READ COMMITTED",
        "SHOW transaction_isolation",
        "SELECT current_setting('transaction_isolation')",
        "COMMIT",
        "SHOW transaction_isolation",
        "BEGIN",
        "SET transaction_isolation = 'read committed'",
        "SHOW transaction_isolation",
        "ROLLBACK",
        "BEGIN ISOLATION LEVEL READ COMMITTED",
        "SELECT 1",
        "SELECT set_config('transaction_isolation', 'read committed', false)",
        "ROLLBACK",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SELECT current_setting('missing_parameter')",
        "SELECT set_config('missing_parameter', 'value', false)",
        "SELECT set_config(NULL, 'value', false)",
        "SELECT set_config('1.bad', 'value', false)",
        "SELECT set_config('search_path', '\"a\"junk', false)",
        "SELECT set_config('search_path', 'a b', false)",
        "SELECT set_config('transaction_isolation', 'read committed', false)",
        "SELECT set_config('transaction_isolation', NULL, false)",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT set_config('custom.failed_statement', 'value', false)::int",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT current_setting('custom.failed_statement', true)",
        RowOrder::Ordered,
    );
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT (SELECT set_config('custom.failed_subquery', 'value', false)::int)",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT current_setting('custom.failed_subquery', true)",
        RowOrder::Ordered,
    );
    for sql in ["BEGIN", "SAVEPOINT failed_subquery"] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT (SELECT set_config('custom.failed_savepoint', 'value', false)::int)",
        RowOrder::Ordered,
    );
    for sql in [
        "ROLLBACK TO failed_subquery",
        "SELECT current_setting('custom.failed_savepoint', true)",
        "ROLLBACK",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in ["BEGIN", "SELECT 1"] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SET transaction_isolation = 'read committed'",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "ROLLBACK",
        RowOrder::Ordered,
    );
    for sql in [
        "BEGIN",
        "RESET transaction_isolation",
        "ROLLBACK",
        "BEGIN",
        "SET transaction_isolation TO DEFAULT",
        "ROLLBACK",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    runtime.block_on(async {
        let expected: String = sqlx::query_scalar("SELECT set_config($1, $2, $3)")
            .bind("application_name")
            .bind("prepared")
            .bind(false)
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual: String = sqlx::query_scalar("SELECT set_config($1, $2, $3)")
            .bind("application_name")
            .bind("prepared")
            .bind(false)
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!(actual, expected);
        let expected: Option<String> = sqlx::query_scalar("SELECT current_setting($1, $2)")
            .bind("missing.parameter")
            .bind(true)
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual: Option<String> = sqlx::query_scalar("SELECT current_setting($1, $2)")
            .bind("missing.parameter")
            .bind(true)
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!(actual, expected);
    });
}

#[test]
fn preserve_session_isolation_snapshot_defaults_and_strict_policy() {
    let db = Db::create_builder()
        .set_lock_timeout(Duration::from_millis(42))
        .build();
    let mut first = db.create_session();
    let mut second = db.create_session();
    let show = first.prepare("SHOW application_name").unwrap();
    first
        .execute("SET application_name = 'first'; SET lock_timeout = '3s'; SET work_mem = '8MB'")
        .unwrap();
    assert_eq!(
        first.query_prepared(&show, &[]).unwrap().rows,
        vec![vec![Value::Text("first".into())]]
    );
    for session in [&mut second, &mut db.snapshot().create_session()] {
        assert_eq!(
            session.query("SHOW application_name", &[]).unwrap().rows,
            vec![vec![Value::Text("".into())]]
        );
        assert_eq!(
            session.query("SHOW lock_timeout", &[]).unwrap().rows,
            vec![vec![Value::Text("42ms".into())]]
        );
        assert_eq!(
            session.query("SHOW work_mem", &[]).unwrap().rows,
            vec![vec![Value::Text("4MB".into())]]
        );
    }
    first.execute("RESET ALL").unwrap();
    assert_eq!(
        first.query("SHOW lock_timeout", &[]).unwrap().rows,
        vec![vec![Value::Text("42ms".into())]]
    );
    let mut strict = Db::create_builder()
        .set_strict_mode_enabled(true)
        .build()
        .create_session();
    for sql in [
        "SET enable_seqscan = off",
        "SHOW enable_seqscan",
        "RESET enable_seqscan",
    ] {
        assert_eq!(
            strict.execute(sql).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
    }
    for sql in [
        "SET enable_nonexistent = on",
        "SHOW enable_nonexistent",
        "RESET enable_nonexistent",
    ] {
        assert_eq!(
            strict.execute(sql).unwrap_err().sqlstate,
            SqlState::UndefinedObject
        );
    }
    strict
        .execute("SET application_name = 'strict'; SET client_encoding = 'UTF8'; RESET ALL")
        .unwrap();
    assert_eq!(
        strict
            .execute("SELECT current_setting('enable_seqscan')")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    assert_eq!(
        strict
            .query(
                "SELECT set_config('custom.audit_setting', 'enabled', false)",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Text("enabled".into())]]
    );
    assert_eq!(
        strict
            .execute("SET client_encoding = 'LATIN1'")
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );
    strict
        .execute("SET default_transaction_isolation = 'serializable'")
        .unwrap();
    assert_eq!(
        strict
            .query("SHOW default_transaction_isolation", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Text("serializable".into())]]
    );
}

#[test]
fn compare_prepared_literal_capture_and_search_path_rebinding() {
    let server = differential::start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let sql = "SELECT '2024-07-01 12:00'::timestamptz";
        for zone in ["UTC", "Europe/Paris", "CET"] {
            let set = format!("SET timezone = '{zone}'");
            sqlx::raw_sql(&set).execute(&mut postgres).await.unwrap();
            sqlx::raw_sql(&set).execute(&mut fake).await.unwrap();
            let expected: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(sql)
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual: chrono::DateTime<chrono::Utc> =
                sqlx::query_scalar(sql).fetch_one(&mut fake).await.unwrap();
            assert_eq!(actual, expected, "cached literal under {zone}");
        }
        for sql in [
            "CREATE TABLE public.items(id INT)",
            "CREATE TEMP TABLE items(id INT)",
            "INSERT INTO public.items VALUES(1)",
            "INSERT INTO pg_temp.items VALUES(2)",
        ] {
            sqlx::raw_sql(sql).execute(&mut postgres).await.unwrap();
            sqlx::raw_sql(sql).execute(&mut fake).await.unwrap();
        }
        for path in ["public, pg_temp", "pg_temp, public", "public, pg_temp"] {
            let sql = format!("SET search_path = {path}");
            sqlx::raw_sql(&sql).execute(&mut postgres).await.unwrap();
            sqlx::raw_sql(&sql).execute(&mut fake).await.unwrap();
            let expected: i32 = sqlx::query_scalar("SELECT id FROM items")
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual: i32 = sqlx::query_scalar("SELECT id FROM items")
                .fetch_one(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
    });
}

#[test]
fn compare_replanned_literals_and_aborted_error_precedence() {
    let server = differential::start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let sql = "SELECT '2024-07-01 12:00'::timestamptz AT TIME ZONE 'UTC'";
        for (zone, path) in [
            ("UTC", "public"),
            ("Europe/Paris", "pg_catalog"),
            ("America/New_York", "pg_catalog"),
            ("Europe/Paris", "public"),
        ] {
            let set = format!("SET timezone = '{zone}'; SET search_path = {path}");
            sqlx::raw_sql(&set).execute(&mut postgres).await.unwrap();
            sqlx::raw_sql(&set).execute(&mut fake).await.unwrap();
            let expected: chrono::NaiveDateTime = sqlx::query_scalar(sql)
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual: chrono::NaiveDateTime =
                sqlx::query_scalar(sql).fetch_one(&mut fake).await.unwrap();
            assert_eq!(actual, expected, "{zone} / {path}");
        }
        for sql in [
            "BEGIN",
            "SELECT 1 / 0",
            "SELECT 'bad'::timestamptz",
            "ROLLBACK",
        ] {
            let expected = sqlx::query(sql)
                .execute(&mut postgres)
                .await
                .map(|_| ())
                .map_err(|error| {
                    error
                        .as_database_error()
                        .unwrap()
                        .code()
                        .unwrap()
                        .into_owned()
                });
            let actual = sqlx::query(sql)
                .execute(&mut fake)
                .await
                .map(|_| ())
                .map_err(|error| {
                    error
                        .as_database_error()
                        .unwrap()
                        .code()
                        .unwrap()
                        .into_owned()
                });
            assert_eq!(actual, expected, "{sql}");
        }
    });
}
