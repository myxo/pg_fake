#![cfg(feature = "time")]

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    str::FromStr,
    time::Duration,
};

use bigdecimal::BigDecimal;
use pg_fake::parser::{self, Statement};
use pg_fake_sqlx::{Db, PgFakeConnectOptions, PgFakeConnection, PgFakePoolOptions};
use serde_json::{Value as JsonValue, json};
use sqlx::{Column, Connection, Executor, Row, Statement as _, TypeInfo};
use sqlx_core::migrate::{Migration, MigrationType, Migrator};
use sqlx_postgres::{PgConnection, PgPoolOptions};
use time::OffsetDateTime;
use uuid::Uuid;

mod common;
#[allow(dead_code)]
#[path = "common/differential.rs"]
mod differential;
#[allow(dead_code)]
#[path = "postgres_regress/phase3_manifest.rs"]
mod phase3_manifest;

use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[derive(Clone, Copy)]
enum ScenarioCoverage {
    Application,
    FocusedReplay(&'static str),
}

const TASK30_SCENARIO_COVERAGE: &[(&str, ScenarioCoverage)] = &[
    ("schema_evolution", ScenarioCoverage::Application),
    ("procedural_triggers", ScenarioCoverage::Application),
    ("data_reconciliation", ScenarioCoverage::Application),
    (
        "concurrent_unique_insert",
        ScenarioCoverage::FocusedReplay("../pg_fake/src/session/tests.rs"),
    ),
    (
        "concurrent_conflict_recheck",
        ScenarioCoverage::FocusedReplay("../pg_fake/src/session/tests.rs"),
    ),
    (
        "uncommitted_ddl_visibility",
        ScenarioCoverage::FocusedReplay("../pg_fake/src/session/tests.rs"),
    ),
    (
        "drop_and_recreate_visibility",
        ScenarioCoverage::FocusedReplay("../pg_fake/src/session/tests.rs"),
    ),
    (
        "lock_mode_compatibility_matrix",
        ScenarioCoverage::FocusedReplay("tests/row_lock_differential.rs"),
    ),
    (
        "skip_locked_work_queue",
        ScenarioCoverage::FocusedReplay("tests/row_lock_differential.rs"),
    ),
    (
        "hashed_advisory_contention",
        ScenarioCoverage::FocusedReplay("tests/text_hash_differential.rs"),
    ),
    (
        "priority_application_workload",
        ScenarioCoverage::Application,
    ),
];

#[derive(Debug, PartialEq)]
struct SessionSnapshot {
    identity_id: i64,
    permissions: Vec<Option<i64>>,
    related_ids: Vec<Option<Uuid>>,
    context: JsonValue,
    expires_at: OffsetDateTime,
    parameter_types: Vec<String>,
    columns: Vec<(String, String)>,
}

#[derive(Debug, PartialEq)]
struct WorkloadSnapshot {
    migration_count: i64,
    migration_reapply_count: i64,
    session: SessionSnapshot,
    session_upsert_affected: u64,
    duplicate_session_sqlstate: String,
    trigger_value: i64,
    trigger_flags: (Option<bool>, bool),
    claimed_work: Vec<i64>,
    ready_work: Vec<i64>,
    accounting: Vec<(i64, i64)>,
    advisory_results: (bool, bool, bool),
    table_lock_sqlstate: String,
    promotion_remaining: i64,
    payment: (String, String),
    request_limit: (i64, Option<i64>),
    member_sessions: Vec<Uuid>,
    latest_execution: (Uuid, String),
    execution_payload_state: String,
    log_ids: Vec<i64>,
    rolled_back_log_count: i64,
    retry_schedule: Vec<i32>,
    set_values: Vec<i32>,
    runtime_values: (String, bool, bool),
    compatibility_values: (bool, String),
    maintenance_id: i32,
}

fn create_migrator() -> Migrator {
    let migrations = [
        (
            1,
            "schema evolution core",
            include_str!("migrations/schema_evolution/001_create_core.sql"),
        ),
        (
            2,
            "schema evolution catalog",
            include_str!("migrations/schema_evolution/002_evolve_catalog.sql"),
        ),
        (
            101,
            "procedural trigger creation",
            include_str!("migrations/procedural_triggers/001_create_triggers.sql"),
        ),
        (
            102,
            "procedural trigger exercise",
            include_str!("migrations/procedural_triggers/002_exercise_triggers.sql"),
        ),
        (
            103,
            "procedural trigger retirement",
            include_str!("migrations/procedural_triggers/003_validate_and_retire.sql"),
        ),
        (
            201,
            "reconciliation sources",
            include_str!("migrations/data_reconciliation/001_create_sources.sql"),
        ),
        (
            202,
            "reconcile records",
            include_str!("migrations/data_reconciliation/002_reconcile_records.sql"),
        ),
        (
            203,
            "validate reconciliation",
            include_str!("migrations/data_reconciliation/003_validate_relationship.sql"),
        ),
        (
            301,
            "application workload schema",
            include_str!("application_workload/001_create_application.sql"),
        ),
    ]
    .into_iter()
    .map(|(version, description, sql)| {
        Migration::new(
            version,
            Cow::Borrowed(description),
            MigrationType::Simple,
            Cow::Borrowed(sql),
            false,
        )
    })
    .collect();
    Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    }
}

fn get_sqlstate(error: sqlx::Error) -> String {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .expect("database errors must expose SQLSTATE")
        .into_owned()
}

fn get_manifest_row_order(sql: &str) -> RowOrder {
    let mut statements = parser::parse(sql).expect("manifest SQL must parse");
    assert_eq!(statements.len(), 1);
    match statements.pop().unwrap() {
        Statement::Query(query) if query.order_by.is_some() => RowOrder::Ordered,
        _ => RowOrder::Unordered,
    }
}

macro_rules! run_workload {
    ($pool:expr) => {{
        let pool = $pool;
        let migrator = create_migrator();
        migrator.run(pool).await.unwrap();
        let migration_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(pool)
                .await
                .unwrap();
        migrator.run(pool).await.unwrap();
        let migration_reapply_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(pool)
                .await
                .unwrap();

        let session_id = Uuid::parse_str("10000000-0000-4000-8000-000000000001").unwrap();
        let related_ids = vec![
            Some(Uuid::parse_str("20000000-0000-4000-8000-000000000001").unwrap()),
            None,
            Some(Uuid::parse_str("20000000-0000-4000-8000-000000000002").unwrap()),
        ];
        let expires_at = OffsetDateTime::from_unix_timestamp(1_767_225_600).unwrap();
        sqlx::query(
            "INSERT INTO public.app_sessions \
             (id, identity_id, permissions, related_ids, context, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(session_id)
        .bind(1_i64)
        .bind(vec![Some(10_i64), None, Some(30_i64)])
        .bind(related_ids.clone())
        .bind(sqlx::types::Json(json!({"device":"test","risk":2})))
        .bind(expires_at)
        .execute(pool)
        .await
        .unwrap();
        let session_upsert_affected = sqlx::query(
            "INSERT INTO public.app_sessions \
             (id, identity_id, permissions, related_ids, context, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (id) DO UPDATE SET context = excluded.context",
        )
        .bind(session_id)
        .bind(1_i64)
        .bind(vec![Some(10_i64), None, Some(30_i64)])
        .bind(related_ids.clone())
        .bind(sqlx::types::Json(json!({"device":"test","risk":3})))
        .bind(expires_at)
        .execute(pool)
        .await
        .unwrap()
        .rows_affected();

        let mut connection = pool.acquire().await.unwrap();
        let statement = connection
            .prepare(
                "SELECT identity_id, permissions, related_ids, context, expires_at \
                 FROM public.app_sessions WHERE id = $1",
            )
            .await
            .unwrap();
        let parameter_types = statement
            .parameters()
            .unwrap()
            .left()
            .unwrap()
            .iter()
            .map(|type_info| type_info.name().to_owned())
            .collect();
        let columns = statement
            .columns()
            .iter()
            .map(|column| (column.name().to_owned(), column.type_info().name().to_owned()))
            .collect();
        let row = statement
            .query()
            .bind(session_id)
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let session = SessionSnapshot {
            identity_id: row.get("identity_id"),
            permissions: row.get("permissions"),
            related_ids: row.get("related_ids"),
            context: row.get::<sqlx::types::Json<JsonValue>, _>("context").0,
            expires_at: row.get("expires_at"),
            parameter_types,
            columns,
        };
        drop(connection);

        let duplicate_session_sqlstate = get_sqlstate(
            sqlx::query(
                "INSERT INTO public.app_sessions \
                 (id, identity_id, permissions, related_ids, context, expires_at) \
                 VALUES ($1, 1, ARRAY[]::BIGINT[], ARRAY[]::UUID[], '{}', $2)",
            )
            .bind(Uuid::parse_str("10000000-0000-4000-8000-000000000002").unwrap())
            .bind(expires_at)
            .execute(pool)
            .await
            .unwrap_err(),
        );

        sqlx::query("INSERT INTO public.records (id, value, compatible) VALUES (900, 10, NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("UPDATE public.records SET value = 20 WHERE id = 900")
            .execute(pool)
            .await
            .unwrap();
        let trigger_row = sqlx::query(
            "SELECT value, compatible, inserted_by_trigger, updated_at >= created_at \
             FROM public.records WHERE id = 900",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let trigger_value = trigger_row.get(0);
        let trigger_flags = (trigger_row.get(1), trigger_row.get(2));
        assert!(trigger_row.get::<bool, _>(3));

        let owner_one = Uuid::parse_str("30000000-0000-4000-8000-000000000001").unwrap();
        let owner_two = Uuid::parse_str("30000000-0000-4000-8000-000000000002").unwrap();
        let tag = Uuid::parse_str("40000000-0000-4000-8000-000000000001").unwrap();
        for (id, priority, claim_key) in [
            (101_i64, 10_i32, "queue:low"),
            (102_i64, 30_i32, "queue:shared"),
            (103_i64, 20_i32, "queue:shared"),
        ] {
            sqlx::query(
                "INSERT INTO public.app_work_items \
                 (id, priority, claim_key, tags, payload, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $6)",
            )
            .bind(id)
            .bind(priority)
            .bind(claim_key)
            .bind(vec![tag])
            .bind(sqlx::types::Json(json!({"cost":id,"kind":"test"})))
            .bind(expires_at)
            .execute(pool)
            .await
            .unwrap();
        }

        let mut first = pool.begin().await.unwrap();
        let first_advisory: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
        )
        .bind("queue:shared")
        .fetch_one(&mut *first)
        .await
        .unwrap();
        let first_claim: i64 = sqlx::query_scalar(
            "SELECT id FROM public.app_work_items WHERE state = 'ready' \
             ORDER BY priority DESC, id LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_one(&mut *first)
        .await
        .unwrap();

        let mut second = pool.begin().await.unwrap();
        let second_advisory: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
        )
        .bind("queue:shared")
        .fetch_one(&mut *second)
        .await
        .unwrap();
        let second_claim: i64 = sqlx::query_scalar(
            "SELECT id FROM public.app_work_items WHERE state = 'ready' \
             ORDER BY priority DESC, id LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_one(&mut *second)
        .await
        .unwrap();

        for (transaction, id, owner) in [
            (&mut first, first_claim, owner_one),
            (&mut second, second_claim, owner_two),
        ] {
            sqlx::query(
                "UPDATE public.app_work_items \
                 SET state = 'claimed', owner_id = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(owner)
            .bind(expires_at)
            .bind(id)
            .execute(&mut **transaction)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO public.app_work_accounting \
                 (work_id, owner_id, charged, recorded_at) VALUES ($1, $2, 1, $3)",
            )
            .bind(id)
            .bind(owner)
            .bind(expires_at)
            .execute(&mut **transaction)
            .await
            .unwrap();
        }
        first.commit().await.unwrap();
        second.commit().await.unwrap();

        let mut released = pool.begin().await.unwrap();
        let released_advisory: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
        )
        .bind("queue:shared")
        .fetch_one(&mut *released)
        .await
        .unwrap();
        released.rollback().await.unwrap();

        let claimed_work = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM public.app_work_items WHERE state = 'claimed' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let ready_work =
            sqlx::query_scalar::<_, i64>("SELECT id FROM public.app_ready_work ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        let accounting = sqlx::query("SELECT work_id, charged FROM public.app_work_accounting ORDER BY work_id")
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();

        let mut table_holder = pool.begin().await.unwrap();
        table_holder
            .execute("LOCK TABLE public.app_work_accounting IN ACCESS EXCLUSIVE MODE")
            .await
            .unwrap();
        let mut table_waiter = pool.begin().await.unwrap();
        table_waiter
            .execute("SET LOCAL lock_timeout = '20ms'")
            .await
            .unwrap();
        let table_lock_sqlstate = get_sqlstate(
            sqlx::query(
                "INSERT INTO public.app_work_accounting \
                 (work_id, owner_id, charged, recorded_at) VALUES (101, $1, 1, $2)",
            )
            .bind(owner_one)
            .bind(expires_at)
            .execute(&mut *table_waiter)
            .await
            .unwrap_err(),
        );
        table_waiter.rollback().await.unwrap();
        table_holder.rollback().await.unwrap();

        sqlx::query(
            "INSERT INTO public.app_promotions VALUES \
             ('WELCOME', 2, '{\"kind\":\"fixed\"}')",
        )
        .execute(pool)
        .await
        .unwrap();
        let payment_id = Uuid::parse_str("50000000-0000-4000-8000-000000000001").unwrap();
        let payment_amount = BigDecimal::from_str("12.50").unwrap();
        let mut payment_transaction = pool.begin().await.unwrap();
        let promotion_remaining: i64 = sqlx::query_scalar(
            "WITH charged AS ( \
                 INSERT INTO public.app_payments \
                 (id, identity_id, promotion_code, amount, state, metadata, created_at) \
                 VALUES ($1, 1, 'WELCOME', $2, 'captured', $3, $4) \
                 RETURNING promotion_code \
             ) \
             UPDATE public.app_promotions AS promotion \
             SET remaining = remaining - 1 \
             FROM charged \
             WHERE promotion.code = charged.promotion_code \
             RETURNING promotion.remaining",
        )
        .bind(payment_id)
        .bind(payment_amount)
        .bind(sqlx::types::Json(json!({"processor":"test"})))
        .bind(expires_at)
        .fetch_one(&mut *payment_transaction)
        .await
        .unwrap();
        payment_transaction.commit().await.unwrap();
        let payment_row = sqlx::query(
            "SELECT amount::text, metadata #>> '{processor}' \
             FROM public.app_payments WHERE id = $1",
        )
        .bind(payment_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let payment = (payment_row.get(0), payment_row.get(1));

        for (issued_at, accepted) in [(10_i64, true), (20, true), (30, false)] {
            sqlx::query("INSERT INTO public.app_request_events VALUES ('member:1', $1, $2)")
                .bind(issued_at)
                .bind(accepted)
                .execute(pool)
                .await
                .unwrap();
        }
        let request_row = sqlx::query(
            "SELECT max(issued_at), \
             (array_agg(issued_at ORDER BY issued_at DESC) \
              FILTER (WHERE accepted AND issued_at <= $2))[2] \
             FROM public.app_request_events WHERE request_key = $1",
        )
        .bind("member:1")
        .bind(30_i64)
        .fetch_one(pool)
        .await
        .unwrap();
        let request_limit = (request_row.get(0), request_row.get(1));
        let member_sessions = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM public.app_sessions WHERE id = ANY($1) ORDER BY id",
        )
        .bind(vec![session_id, Uuid::nil()])
        .fetch_all(pool)
        .await
        .unwrap();

        let thread_id = Uuid::parse_str("60000000-0000-4000-8000-000000000001").unwrap();
        let first_execution =
            Uuid::parse_str("70000000-0000-4000-8000-000000000001").unwrap();
        let latest_execution_id =
            Uuid::parse_str("70000000-0000-4000-8000-000000000002").unwrap();
        sqlx::query("INSERT INTO public.app_threads VALUES ($1, 1, 'conformance', '{}')")
            .bind(thread_id)
            .execute(pool)
            .await
            .unwrap();
        for (id, state, started_at, payload) in [
            (
                first_execution,
                "failed",
                OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
                json!({"state":"failed"}),
            ),
            (
                latest_execution_id,
                "running",
                OffsetDateTime::from_unix_timestamp(1_700_000_100).unwrap(),
                json!({"state":"running"}),
            ),
        ] {
            sqlx::query(
                "INSERT INTO public.app_executions \
                 (id, thread_id, state, started_at, payload) VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(id)
            .bind(thread_id)
            .bind(state)
            .bind(started_at)
            .bind(sqlx::types::Json(payload))
            .execute(pool)
            .await
            .unwrap();
        }
        let latest_row = sqlx::query(
            "SELECT thread.id, latest.state \
             FROM public.app_threads AS thread \
             CROSS JOIN LATERAL ( \
                 SELECT state FROM public.app_executions AS execution \
                 WHERE execution.thread_id = thread.id \
                 ORDER BY started_at DESC LIMIT 1 \
             ) AS latest \
             WHERE thread.id = $1",
        )
        .bind(thread_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let latest_execution = (latest_row.get(0), latest_row.get(1));
        let execution_payload_state: String = sqlx::query_scalar(
            "SELECT payload ->> 'state' FROM public.app_executions WHERE id = $1",
        )
        .bind(latest_execution_id)
        .fetch_one(pool)
        .await
        .unwrap();

        for (ordinal, message) in [(1_i64, "started"), (2, "running")] {
            sqlx::query(
                "INSERT INTO public.app_logs (execution_id, ordinal, payload, created_at) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(latest_execution_id)
            .bind(ordinal)
            .bind(sqlx::types::Json(json!({"message":message})))
            .bind(expires_at)
            .execute(pool)
            .await
            .unwrap();
        }
        let skipped_log = sqlx::query(
            "INSERT INTO public.app_logs (execution_id, ordinal, payload, created_at) \
             VALUES ($1, 2, '{}', $2) ON CONFLICT (execution_id, ordinal) DO NOTHING",
        )
        .bind(latest_execution_id)
        .bind(expires_at)
        .execute(pool)
        .await
        .unwrap();
        assert_eq!(skipped_log.rows_affected(), 0);
        let mut rolled_back = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO public.app_logs (execution_id, ordinal, payload, created_at) \
             VALUES ($1, 3, '{}', $2)",
        )
        .bind(latest_execution_id)
        .bind(expires_at)
        .execute(&mut *rolled_back)
        .await
        .unwrap();
        rolled_back.rollback().await.unwrap();
        let log_ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM public.app_logs WHERE execution_id = $1 ORDER BY ordinal",
        )
        .bind(latest_execution_id)
        .fetch_all(pool)
        .await
        .unwrap();
        let rolled_back_log_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM public.app_logs \
             WHERE execution_id = $1 AND ordinal = 3",
        )
        .bind(latest_execution_id)
        .fetch_one(pool)
        .await
        .unwrap();

        let retry_schedule = sqlx::query_scalar::<_, i32>(
            "WITH RECURSIVE retry(attempt) AS ( \
                 VALUES (1) UNION ALL \
                 SELECT attempt + 1 FROM retry WHERE attempt < 3 \
             ) SELECT attempt FROM retry ORDER BY attempt",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let set_values = sqlx::query_scalar::<_, i32>(
            "SELECT value FROM (SELECT 1 AS value UNION SELECT 2 UNION SELECT 1) values \
             ORDER BY value",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let runtime_row = sqlx::query(
            "SELECT to_char(date_trunc('day', $1::timestamptz), 'YYYY-MM-DD'), \
             'Application_30' ILIKE 'application!_%' ESCAPE '!', \
             regexp_like('queue-123', '^queue-[0-9]{3}$')",
        )
        .bind(expires_at)
        .fetch_one(pool)
        .await
        .unwrap();
        let runtime_values = (runtime_row.get(0), runtime_row.get(1), runtime_row.get(2));
        let compatibility_row = sqlx::query(
            "SELECT pg_is_in_recovery(), to_regclass('public.app_sessions')::text",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let compatibility_values = (compatibility_row.get(0), compatibility_row.get(1));

        sqlx::query("INSERT INTO public.app_maintenance (value) VALUES ('discard')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("TRUNCATE public.app_maintenance RESTART IDENTITY")
            .execute(pool)
            .await
            .unwrap();
        let maintenance_id: i32 = sqlx::query_scalar(
            "INSERT INTO public.app_maintenance (value) VALUES ('kept') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();

        WorkloadSnapshot {
            migration_count,
            migration_reapply_count,
            session,
            session_upsert_affected,
            duplicate_session_sqlstate,
            trigger_value,
            trigger_flags,
            claimed_work,
            ready_work,
            accounting,
            advisory_results: (first_advisory, second_advisory, released_advisory),
            table_lock_sqlstate,
            promotion_remaining,
            payment,
            request_limit,
            member_sessions,
            latest_execution,
            execution_payload_state,
            log_ids,
            rolled_back_log_count,
            retry_schedule,
            set_values,
            runtime_values,
            compatibility_values,
            maintenance_id,
        }
    }};
}

#[test]
fn matches_postgres_application_workload() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let postgres = PgPoolOptions::new()
            .max_connections(6)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&server.url)
            .await
            .unwrap();
        let fake = PgFakePoolOptions::new()
            .max_connections(6)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(PgFakeConnectOptions::new(Db::create()))
            .await
            .unwrap();

        let expected = run_workload!(&postgres);
        let actual = run_workload!(&fake);
        assert_eq!(actual, expected);
        assert_eq!(actual.migration_count, 9);
        assert_eq!(actual.advisory_results, (true, false, true));
        assert_eq!(actual.claimed_work, vec![102, 103]);
        assert_eq!(actual.ready_work, vec![101]);
        assert_eq!(actual.duplicate_session_sqlstate, "23505");
        assert_eq!(actual.table_lock_sqlstate, "55P03");
        assert_eq!(actual.rolled_back_log_count, 0);
        assert_eq!(actual.maintenance_id, 1);

        postgres.close().await;
        fake.close().await;
    });
}

#[test]
fn replays_every_priority_manifest_case() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    for feature in phase3_manifest::FEATURES
        .iter()
        .filter(|feature| feature.task <= 30)
    {
        for case in feature.cases {
            let mut fake = PgFakeConnection::new(Db::create());
            for setup in case.setup {
                assert_statement(
                    &runtime,
                    &mut postgres,
                    &mut fake,
                    setup,
                    RowOrder::Unordered,
                );
            }
            assert_statement_allow_error(
                &runtime,
                &mut postgres,
                &mut fake,
                case.sql,
                get_manifest_row_order(case.sql),
            );
            runtime
                .block_on(sqlx::raw_sql("ROLLBACK").execute(&mut postgres))
                .unwrap();
        }
    }
}

#[test]
fn classifies_every_priority_manifest_scenario() {
    let expected = phase3_manifest::SCENARIOS
        .iter()
        .filter(|scenario| scenario.task <= 30)
        .map(|scenario| scenario.name)
        .collect::<BTreeSet<_>>();
    let mut coverage = BTreeMap::new();
    for (name, classification) in TASK30_SCENARIO_COVERAGE {
        assert!(
            coverage.insert(*name, *classification).is_none(),
            "duplicate Task 30 scenario classification {name}"
        );
    }
    assert_eq!(coverage.keys().copied().collect::<BTreeSet<_>>(), expected);
    for (name, classification) in coverage {
        match classification {
            ScenarioCoverage::Application => assert!(matches!(
                name,
                "schema_evolution"
                    | "procedural_triggers"
                    | "data_reconciliation"
                    | "priority_application_workload"
            )),
            ScenarioCoverage::FocusedReplay(path) => assert!(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(path).is_file(),
                "Task 30 scenario {name} replay target does not exist: {path}"
            ),
        }
    }
}
