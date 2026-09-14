use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Row, TypeInfo};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_advisory_signatures_and_transaction_lifetimes() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = (0..3)
        .map(|_| {
            runtime
                .block_on(PgConnection::connect(&server.url))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let db = Db::create();
    let mut fake = (0..3)
        .map(|_| PgFakeConnection::new(db.clone()))
        .collect::<Vec<_>>();
    for (session, sql) in [
        (0, "CREATE TABLE advisory_rows(id INT PRIMARY KEY)"),
        (0, "INSERT INTO advisory_rows VALUES(1),(2),(3)"),
        (0, "BEGIN"),
        (0, "SELECT pg_advisory_xact_lock(1)"),
        (0, "SELECT pg_advisory_xact_lock(1::BIGINT)"),
        (0, "SELECT pg_try_advisory_xact_lock(1::SMALLINT)"),
        (1, "SELECT pg_try_advisory_xact_lock(1)"),
        (1, "SELECT pg_try_advisory_xact_lock_shared(1)"),
        (1, "SELECT pg_try_advisory_xact_lock(0,1)"),
        (
            1,
            "SELECT id FROM advisory_rows WHERE id=1 FOR UPDATE NOWAIT",
        ),
        (0, "COMMIT"),
        (1, "SELECT pg_try_advisory_xact_lock(1)"),
        (2, "SELECT pg_try_advisory_xact_lock(1)"),
        (0, "BEGIN"),
        (1, "BEGIN"),
        (
            0,
            "SELECT pg_advisory_xact_lock_shared(-2147483648,2147483647)",
        ),
        (
            1,
            "SELECT pg_advisory_xact_lock_shared(-2147483648,2147483647)",
        ),
        (
            2,
            "SELECT pg_try_advisory_xact_lock_shared(-2147483648,2147483647)",
        ),
        (
            2,
            "SELECT pg_try_advisory_xact_lock(-2147483648,2147483647)",
        ),
        (
            0,
            "SELECT pg_try_advisory_xact_lock(-2147483648,2147483647)",
        ),
        (1, "ROLLBACK"),
        (
            0,
            "SELECT pg_try_advisory_xact_lock(-2147483648,2147483647)",
        ),
        (0, "ROLLBACK"),
        (
            2,
            "SELECT pg_try_advisory_xact_lock(-2147483648,2147483647)",
        ),
        (
            0,
            "SELECT pg_advisory_xact_lock('-9223372036854775808'::BIGINT), pg_advisory_xact_lock(9223372036854775807::BIGINT)",
        ),
        (
            0,
            "SELECT pg_advisory_xact_lock(NULL), pg_try_advisory_xact_lock(NULL), pg_try_advisory_xact_lock(NULL,1)",
        ),
        (
            0,
            "SELECT pg_advisory_xact_lock('19'), pg_try_advisory_xact_lock('19')",
        ),
        (
            0,
            "SELECT pg_advisory_xact_lock(19) IS NULL, pg_advisory_xact_lock(NULL) IS NULL",
        ),
    ] {
        assert_statement(
            &runtime,
            &mut postgres[session],
            &mut fake[session],
            sql,
            RowOrder::Ordered,
        );
    }
    for sql in [
        "CREATE TABLE invalid_advisory_void(value VOID)",
        "CREATE VIEW invalid_advisory_void AS SELECT pg_advisory_xact_lock(1)",
        "ALTER TABLE advisory_rows ADD COLUMN invalid VOID",
        "ALTER TABLE advisory_rows ALTER COLUMN id TYPE VOID",
        "SELECT pg_advisory_xact_lock(1)=pg_advisory_xact_lock(2)",
        "SELECT DISTINCT pg_advisory_xact_lock(id) FROM advisory_rows",
        "SELECT EXISTS(SELECT missing LIMIT 1/0)",
        "SELECT EXISTS(SELECT nonexistent_function() LIMIT 1/0)",
        "SELECT pg_advisory_xact_lock()",
        "SELECT pg_advisory_xact_lock(1,2,3)",
        "SELECT pg_advisory_xact_lock(1::TEXT)",
        "SELECT pg_advisory_xact_lock(1.5)",
        "SELECT pg_advisory_xact_lock(1::BIGINT,2)",
        "SELECT pg_advisory_xact_lock('9223372036854775808')",
        "SELECT pg_try_advisory_xact_lock('nope')",
        "SELECT pg_try_advisory_xact_lock(TRUE)",
    ] {
        assert_statement_allow_error(
            &runtime,
            &mut postgres[0],
            &mut fake[0],
            sql,
            RowOrder::Ordered,
        );
    }
}

#[test]
fn matches_session_advisory_reentrancy_and_rollback() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = (0..2)
        .map(|_| {
            runtime
                .block_on(PgConnection::connect(&server.url))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let db = Db::create();
    let mut fake = (0..2)
        .map(|_| PgFakeConnection::new(db.clone()))
        .collect::<Vec<_>>();
    for (session, sql) in [
        (0, "SELECT pg_advisory_lock(7), pg_advisory_lock(7)"),
        (
            1,
            "SELECT pg_try_advisory_xact_lock(7), pg_advisory_unlock(7)",
        ),
        (0, "BEGIN"),
        (0, "SELECT pg_advisory_xact_lock(7)"),
        (0, "ROLLBACK"),
        (1, "SELECT pg_try_advisory_xact_lock(7)"),
        (0, "SELECT pg_advisory_unlock(7)"),
        (1, "SELECT pg_try_advisory_xact_lock(7)"),
        (0, "SELECT pg_advisory_unlock(7)"),
        (1, "SELECT pg_try_advisory_xact_lock(7)"),
        (0, "SELECT pg_advisory_unlock(7)"),
        (0, "BEGIN"),
        (0, "SELECT pg_advisory_lock(7), pg_advisory_xact_lock(7)"),
        (0, "SELECT pg_advisory_unlock(7)"),
        (1, "SELECT pg_try_advisory_xact_lock(7)"),
        (0, "COMMIT"),
        (1, "SELECT pg_try_advisory_xact_lock(7)"),
        (0, "BEGIN"),
        (0, "SELECT pg_advisory_lock(1,2)"),
        (0, "ROLLBACK"),
        (1, "SELECT pg_try_advisory_xact_lock(1,2)"),
        (0, "SELECT pg_advisory_unlock(1,2)"),
        (1, "SELECT pg_try_advisory_xact_lock(1,2)"),
        (0, "SELECT pg_advisory_lock(NULL), pg_advisory_unlock(NULL)"),
    ] {
        assert_statement(
            &runtime,
            &mut postgres[session],
            &mut fake[session],
            sql,
            RowOrder::Ordered,
        );
    }
}

#[test]
fn matches_advisory_timeouts_and_transaction_abort() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut holder = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut worker = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake_holder = PgFakeConnection::new(db.clone());
    let mut fake_worker = PgFakeConnection::new(db);
    for sql in ["BEGIN", "SELECT pg_advisory_xact_lock(1)"] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            sql,
            RowOrder::Unordered,
        );
    }
    for settings in [
        ["SET lock_timeout='15ms'", "SET LOCAL statement_timeout='0'"],
        ["SET lock_timeout='0'", "SET LOCAL statement_timeout='15ms'"],
    ] {
        for sql in ["BEGIN"].into_iter().chain(settings) {
            assert_statement(
                &runtime,
                &mut worker,
                &mut fake_worker,
                sql,
                RowOrder::Ordered,
            );
        }
        for sql in ["SELECT pg_advisory_xact_lock(1)", "SELECT 1"] {
            assert_statement_allow_error(
                &runtime,
                &mut worker,
                &mut fake_worker,
                sql,
                RowOrder::Ordered,
            );
        }
        assert_statement(
            &runtime,
            &mut worker,
            &mut fake_worker,
            "ROLLBACK",
            RowOrder::Ordered,
        );
    }
    assert_statement(
        &runtime,
        &mut holder,
        &mut fake_holder,
        "ROLLBACK",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut worker,
        &mut fake_worker,
        "SELECT pg_advisory_xact_lock(1)",
        RowOrder::Ordered,
    );
}

#[test]
fn preserves_advisory_sqlx_metadata_and_prepared_transactions() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut observer = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake = PgFakeConnection::new(db.clone());
    let mut fake_observer = PgFakeConnection::new(db);
    for commits in [false, true] {
        runtime.block_on(async {
            let mut pg_tx = postgres.begin().await.unwrap();
            let mut fake_tx = fake.begin().await.unwrap();
            let sql = "SELECT pg_advisory_xact_lock($1) AS held";
            let pg_row = sqlx::query(sql)
                .bind(i64::MIN)
                .fetch_one(&mut *pg_tx)
                .await
                .unwrap();
            let fake_row = sqlx::query(sql)
                .bind(i64::MIN)
                .fetch_one(&mut *fake_tx)
                .await
                .unwrap();
            assert_eq!(pg_row.column(0).type_info().name(), "VOID");
            assert_eq!(fake_row.column(0).type_info().name(), "VOID");
            pg_row.try_get::<(), _>(0).unwrap();
            fake_row.try_get::<(), _>(0).unwrap();
            let sql = "SELECT pg_try_advisory_xact_lock($1)";
            let pg_held: bool = sqlx::query_scalar(sql)
                .bind(i64::MIN)
                .fetch_one(&mut observer)
                .await
                .unwrap();
            let fake_held: bool = sqlx::query_scalar(sql)
                .bind(i64::MIN)
                .fetch_one(&mut fake_observer)
                .await
                .unwrap();
            assert!(!pg_held && !fake_held);
            let sql = "SELECT pg_advisory_xact_lock_shared($1,$2)";
            sqlx::query_scalar::<_, ()>(sql)
                .bind(i32::MIN)
                .bind(i32::MAX)
                .fetch_one(&mut *pg_tx)
                .await
                .unwrap();
            sqlx::query_scalar::<_, ()>(sql)
                .bind(i32::MIN)
                .bind(i32::MAX)
                .fetch_one(&mut *fake_tx)
                .await
                .unwrap();
            if commits {
                pg_tx.commit().await.unwrap();
                fake_tx.commit().await.unwrap();
            } else {
                pg_tx.rollback().await.unwrap();
                fake_tx.rollback().await.unwrap();
            }
        });
        assert_statement(
            &runtime,
            &mut observer,
            &mut fake_observer,
            "SELECT pg_try_advisory_xact_lock('-9223372036854775808'::BIGINT),pg_try_advisory_xact_lock(-2147483648,2147483647)",
            RowOrder::Ordered,
        );
    }
}

#[test]
fn matches_advisory_query_demand_and_locked_keys() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut observer = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake = PgFakeConnection::new(db.clone());
    let mut fake_observer = PgFakeConnection::new(db);
    for sql in [
        "CREATE TABLE advisory_demand(id INT)",
        "INSERT INTO advisory_demand VALUES(1),(2),(3)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT 1",
        "SELECT pg_advisory_xact_lock(id) FROM advisory_demand ORDER BY id DESC LIMIT 1",
        "SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT 1 OFFSET 1",
        "SELECT * FROM (SELECT pg_advisory_xact_lock(id) FROM advisory_demand) x LIMIT 1",
        "WITH x AS (SELECT pg_advisory_xact_lock(id) FROM advisory_demand) SELECT * FROM x LIMIT 1",
        "VALUES(pg_advisory_xact_lock(1)),(pg_advisory_xact_lock(2)) LIMIT 1",
        "SELECT * FROM (VALUES(pg_advisory_xact_lock(1)),(pg_advisory_xact_lock(2))) x LIMIT 1",
        "SELECT pg_advisory_xact_lock(1) UNION ALL SELECT pg_advisory_xact_lock(2) LIMIT 1",
        "SELECT CASE WHEN FALSE THEN pg_advisory_xact_lock(1) END",
        "SELECT EXISTS(WITH q AS(SELECT * FROM advisory_demand) SELECT pg_advisory_xact_lock(id) FROM q)",
        "SELECT EXISTS(WITH q AS(SELECT 1) SELECT pg_advisory_xact_lock(id) FROM advisory_demand)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT (1))",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT 1+1)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT 1::BIGINT)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT NULL::BIGINT)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT '1')",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT abs(-1))",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT btrim(' 1 ')::BIGINT)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) IS NULL AS held FROM advisory_demand ORDER BY held)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand LIMIT 1)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand GROUP BY id)",
        "SELECT EXISTS(SELECT DISTINCT pg_advisory_xact_lock(id) IS NULL FROM advisory_demand)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand OFFSET 0)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(1) FROM advisory_demand HAVING TRUE)",
        "SELECT EXISTS(SELECT pg_advisory_xact_lock(id) FROM advisory_demand FOR UPDATE)",
    ] {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "BEGIN",
            RowOrder::Ordered,
        );
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
        for id in 1..=3 {
            assert_statement(
                &runtime,
                &mut observer,
                &mut fake_observer,
                &format!("SELECT pg_try_advisory_xact_lock({id})"),
                RowOrder::Ordered,
            );
        }
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "ROLLBACK",
            RowOrder::Ordered,
        );
    }
}

#[test]
fn matches_advisory_wait_resumption_and_mutation_results() {
    use differential::{Outcome, fake_statement_outcome, postgres_statement_outcome};
    use std::{
        thread,
        time::{Duration, Instant},
    };

    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres_holder = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    for query in [
        "SELECT pg_advisory_xact_lock(sum(nextval('wait_sequence'))::BIGINT % 5) FROM wait_rows",
        "SELECT nextval('wait_sequence'),pg_advisory_xact_lock(1),count(*) FROM wait_rows",
        "SELECT nextval('wait_sequence'),pg_advisory_xact_lock(1) FROM wait_rows GROUP BY id",
        "SELECT count(pg_advisory_xact_lock(nextval('wait_sequence'))) FROM wait_rows",
        "SELECT pg_advisory_xact_lock(1) FROM wait_rows LIMIT (nextval('wait_sequence')+1)",
        "SELECT pg_advisory_xact_lock(1) FROM wait_rows OFFSET nextval('wait_sequence')",
        "INSERT INTO wait_rows VALUES(4,nextval('wait_sequence')+(pg_advisory_xact_lock(1) IS NULL)::INT) RETURNING *",
        "INSERT INTO wait_rows VALUES(4,6) RETURNING nextval('wait_sequence'),pg_advisory_xact_lock(1)",
        "UPDATE wait_rows SET n=nextval('wait_sequence')+(pg_advisory_xact_lock(id-1) IS NULL)::INT RETURNING *",
        "UPDATE wait_rows SET n=6 RETURNING nextval('wait_sequence'),pg_advisory_xact_lock(id)",
        "DELETE FROM wait_rows RETURNING nextval('wait_sequence'),pg_advisory_xact_lock(id)",
        "WITH x AS(UPDATE wait_rows SET n=n+1 RETURNING id,nextval('wait_sequence'),pg_advisory_xact_lock(id)) SELECT * FROM x",
        "WITH x AS(DELETE FROM wait_rows RETURNING id,nextval('wait_sequence'),pg_advisory_xact_lock(id)) SELECT * FROM x",
        "INSERT INTO wait_rows SELECT id+3,n FROM wait_rows RETURNING id,nextval('wait_sequence'),pg_advisory_xact_lock(id-3)",
        "INSERT INTO wait_rows SELECT id+3,n FROM wait_rows ORDER BY id RETURNING id,nextval('wait_sequence'),pg_advisory_xact_lock(id-3)",
        "INSERT INTO wait_rows SELECT id+3,n FROM wait_rows UNION ALL SELECT 7,8 RETURNING id,nextval('wait_sequence'),pg_advisory_xact_lock(id-3)",
        "INSERT INTO wait_defaults(n) SELECT id FROM wait_rows RETURNING *",
        "INSERT INTO wait_defaults(n) SELECT id FROM wait_rows ORDER BY id RETURNING *",
        "INSERT INTO wait_rows VALUES(1,6) ON CONFLICT(id) DO UPDATE SET n=nextval('wait_sequence'),id=wait_rows.id+(pg_advisory_xact_lock(1) IS NULL)::INT RETURNING *",
        "INSERT INTO wait_rows VALUES(1,6),(2,7) ON CONFLICT(id) DO UPDATE SET n=nextval('wait_sequence'),id=wait_rows.id+(pg_advisory_xact_lock(wait_rows.id) IS NULL)::INT RETURNING *",
        "INSERT INTO wait_rows VALUES(1,6) ON CONFLICT(id) DO UPDATE SET n=(pg_advisory_xact_lock(1) IS NOT NULL)::INT WHERE nextval('wait_sequence')>0 RETURNING *",
        "SELECT * FROM wait_view",
        "INSERT INTO wait_defaults(n) VALUES((pg_advisory_xact_lock(1) IS NOT NULL)::INT) RETURNING *",
    ] {
        for ending in ["COMMIT", "ROLLBACK"] {
            eprintln!("advisory wait {ending}: {query}");
            let db = Db::create_builder()
                .set_lock_timeout(Duration::from_secs(5))
                .build();
            let mut fake_holder = PgFakeConnection::new(db.clone());
            runtime
                .block_on(
                    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public")
                        .execute(&mut postgres_holder),
                )
                .unwrap();
            for sql in [
                "CREATE SEQUENCE wait_sequence",
                "CREATE TABLE wait_rows(id INT PRIMARY KEY,n INT)",
                "INSERT INTO wait_rows VALUES(1,3),(2,4),(3,5)",
                "CREATE VIEW wait_view AS SELECT pg_advisory_xact_lock(nextval('wait_sequence')) IS NOT NULL AS held",
                "CREATE TABLE wait_defaults(id SERIAL,n INT,held INT DEFAULT(pg_advisory_xact_lock(1) IS NOT NULL)::INT)",
                "BEGIN",
                "SELECT pg_advisory_xact_lock(1)",
            ] {
                assert_statement(
                    &runtime,
                    &mut postgres_holder,
                    &mut fake_holder,
                    sql,
                    RowOrder::Ordered,
                );
            }
            let url = server.url.clone();
            let (ready, backend) = std::sync::mpsc::channel();
            let postgres = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let mut postgres_worker = runtime.block_on(PgConnection::connect(&url)).unwrap();
                runtime
                    .block_on(sqlx::raw_sql("SET lock_timeout='5s'").execute(&mut postgres_worker))
                    .unwrap();
                let pid: i32 = runtime
                    .block_on(
                        sqlx::query_scalar("SELECT pg_backend_pid()")
                            .fetch_one(&mut postgres_worker),
                    )
                    .unwrap();
                ready.send(pid).unwrap();
                [
                    query,
                    "SELECT currval('wait_sequence')",
                    "SELECT * FROM wait_rows ORDER BY id",
                    "SELECT * FROM wait_defaults ORDER BY id",
                ]
                .map(|sql| postgres_statement_outcome(&runtime, &mut postgres_worker, sql))
            });
            let pid = backend.recv().unwrap();
            let mut fake_worker = PgFakeConnection::new(db.clone());
            let fake = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                [
                    query,
                    "SELECT currval('wait_sequence')",
                    "SELECT * FROM wait_rows ORDER BY id",
                    "SELECT * FROM wait_defaults ORDER BY id",
                ]
                .map(|sql| fake_statement_outcome(&runtime, &mut fake_worker, sql))
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let fake_started = db
                    .inspect_catalog()
                    .sequences
                    .iter()
                    .any(|sequence| sequence.is_called);
                let pg_waiting: bool = runtime.block_on(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event='advisory')").bind(pid).fetch_one(&mut postgres_holder)).unwrap();
                if fake_started && pg_waiting {
                    break;
                }
                assert!(!fake.is_finished(), "fake did not wait: {query}");
                assert!(!postgres.is_finished(), "PostgreSQL did not wait: {query}");
                assert!(Instant::now() < deadline, "waiters did not block: {query}");
                thread::yield_now();
            }
            assert!(
                !fake.is_finished(),
                "fake completed before holder release: {query}"
            );
            assert_statement(
                &runtime,
                &mut postgres_holder,
                &mut fake_holder,
                ending,
                RowOrder::Ordered,
            );
            let mut postgres = postgres.join().unwrap();
            let mut fake = fake.join().unwrap();
            for outcome in [&mut postgres[0], &mut fake[0]] {
                if let Outcome::Rows(rows) = outcome {
                    rows.sort();
                }
            }
            assert_eq!(fake, postgres, "{ending}: {query}");
            assert!(
                matches!(fake[0], Outcome::Rows(_)),
                "{query}: {:?}",
                fake[0]
            );
        }
    }
}

#[test]
fn preserves_distinct_correlated_advisory_invocations() {
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
        "CREATE TABLE inner_rows(id INT)",
        "CREATE TABLE outer_rows(id INT)",
        "INSERT INTO inner_rows VALUES(1),(2),(3)",
        "INSERT INTO outer_rows VALUES(1),(1),(2),(2)",
        "CREATE SEQUENCE correlated_sequence",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SELECT (SELECT sum(nextval('correlated_sequence')) FROM inner_rows a WHERE a.id=o.id) FROM outer_rows o",
        "SELECT (SELECT count(*)+nextval('correlated_sequence') FROM inner_rows a WHERE a.id=o.id) FROM outer_rows o",
        "SELECT o.id,x.n FROM outer_rows o CROSS JOIN LATERAL(SELECT sum(nextval('correlated_sequence')) n FROM inner_rows a WHERE a.id=o.id) x",
        "SELECT (SELECT sum(nextval('correlated_sequence'))+count(pg_advisory_xact_lock(a.id)) FROM inner_rows a WHERE a.id=o.id) FROM outer_rows o",
        "SELECT o.id,x.n FROM outer_rows o CROSS JOIN LATERAL(SELECT sum(nextval('correlated_sequence'))+count(pg_advisory_xact_lock(a.id)) n FROM inner_rows a WHERE a.id=o.id) x",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "SELECT currval('correlated_sequence')",
            RowOrder::Ordered,
        );
    }
}
