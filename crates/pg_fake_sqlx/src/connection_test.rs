use super::*;

#[test]
fn evicts_least_recently_used_statements_within_the_byte_limit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create());
    runtime.block_on(async {
        sqlx::query("SELECT 1")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("SELECT 2")
            .execute(&mut connection)
            .await
            .unwrap();
    });
    let two_statement_limit = connection
        .state
        .lock()
        .unwrap()
        .statements
        .values()
        .map(|cached| cached.owned_bytes)
        .sum();
    connection = connection.set_statement_cache_limit_bytes(two_statement_limit);
    runtime.block_on(async {
        sqlx::query("SELECT 1")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("SELECT 3")
            .execute(&mut connection)
            .await
            .unwrap();
    });

    let state = connection.state.lock().unwrap();
    assert_eq!(state.statement_cache_bytes, two_statement_limit);
    assert_eq!(
        state
            .statements
            .keys()
            .map(|(sql, _)| sql.as_str())
            .collect::<Vec<_>>(),
        ["SELECT 1", "SELECT 3"]
    );
}

#[test]
fn bounds_cache_accounting_across_distinct_sql_and_clear() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create());
    runtime.block_on(async {
        for index in 0..100_000 {
            sqlx::query(&format!("SELECT {index}"))
                .execute(&mut connection)
                .await
                .unwrap();
            if index == 32_767 || index == 99_999 {
                let state = connection.state.lock().unwrap();
                assert!(state.statements.len() <= MAX_STATEMENT_CACHE_ENTRIES);
                assert!(state.statement_cache_bytes <= state.statement_cache_limit_bytes);
                assert_eq!(
                    state.statement_cache_bytes,
                    state
                        .statements
                        .values()
                        .map(|cached| cached.owned_bytes)
                        .sum::<usize>()
                );
            }
        }
        connection.clear_cached_statements().await.unwrap();
    });
    let state = connection.state.lock().unwrap();
    assert!(state.statements.is_empty());
    assert_eq!(state.statement_cache_bytes, 0);
    drop(state);

    let mut connection =
        PgFakeConnection::new(Db::create()).set_statement_cache_limit_bytes(usize::MAX);
    runtime.block_on(async {
        for index in 0..3_000 {
            sqlx::query(&format!("SELECT {index}"))
                .execute(&mut connection)
                .await
                .unwrap();
        }
    });
    assert_eq!(
        connection.state.lock().unwrap().statements.len(),
        MAX_STATEMENT_CACHE_ENTRIES
    );
}

#[test]
fn charges_for_schema_retained_after_drop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create());
    let default = "x".repeat(16 * 1024);
    let query = "SELECT id FROM large_default";
    runtime.block_on(async {
        sqlx::query(&format!(
            "CREATE TABLE large_default (id INT, payload TEXT DEFAULT '{default}')"
        ))
        .persistent(false)
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(query).execute(&mut connection).await.unwrap();
        sqlx::query("DROP TABLE large_default")
            .persistent(false)
            .execute(&mut connection)
            .await
            .unwrap();
    });

    let state = connection.state.lock().unwrap();
    let cached = state
        .statements
        .get(&(query.to_owned(), Vec::new()))
        .expect("short query remains cached after DROP");
    assert!(cached.owned_bytes > 1024 * 1024);
    assert_eq!(state.statement_cache_bytes, cached.owned_bytes);
    drop(state);

    connection = connection.set_statement_cache_limit_bytes(1024 * 1024);
    assert_eq!(connection.cached_statements_size(), 0);
    assert_eq!(connection.state.lock().unwrap().statement_cache_bytes, 0);
}

#[test]
fn charges_for_large_check_and_view_after_drop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let literal = "x".repeat(16 * 1024);
    for (create, query, drop_sql) in [
        (
            format!("CREATE TABLE large_check (payload TEXT CHECK (payload <> '{literal}'))"),
            "SELECT payload FROM large_check",
            "DROP TABLE large_check",
        ),
        (
            format!("CREATE VIEW large_view AS SELECT '{literal}'::TEXT AS payload"),
            "SELECT payload FROM large_view",
            "DROP VIEW large_view",
        ),
    ] {
        let mut connection = PgFakeConnection::new(Db::create());
        runtime.block_on(async {
            sqlx::query(&create)
                .persistent(false)
                .execute(&mut connection)
                .await
                .unwrap();
            sqlx::query(query).fetch_all(&mut connection).await.unwrap();
            sqlx::query(drop_sql)
                .persistent(false)
                .execute(&mut connection)
                .await
                .unwrap();
        });
        let state = connection.state.lock().unwrap();
        let cached = state
            .statements
            .get(&(query.to_owned(), Vec::new()))
            .expect("short query remains cached after DROP");
        assert!(cached.owned_bytes > 1024 * 1024, "{query}");
        drop(state);
        connection = connection.set_statement_cache_limit_bytes(1024 * 1024);
        assert_eq!(connection.cached_statements_size(), 0);
    }
}

#[test]
fn retains_ordinary_join_in_default_cache() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create());
    let query = "SELECT l.id FROM left_rows l INNER JOIN right_rows r ON l.bucket = r.bucket WHERE l.bucket = 0";
    runtime.block_on(async {
        for table in ["left_rows", "right_rows"] {
            sqlx::query(&format!(
                "CREATE TABLE {table} (id INTEGER, bucket INTEGER)"
            ))
            .execute(&mut connection)
            .await
            .unwrap();
        }
        for _ in 0..2 {
            sqlx::query(query).fetch_all(&mut connection).await.unwrap();
        }
    });
    let state = connection.state.lock().unwrap();
    let cached = state
        .statements
        .get(&(query.to_owned(), Vec::new()))
        .unwrap();
    assert!(cached.owned_bytes < state.statement_cache_limit_bytes);
}

#[test]
fn preserves_typed_keys_and_nonpersistent_and_explicit_statements() {
    use sqlx::Row as _;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create());
    runtime.block_on(async {
        let row = sqlx::query("SELECT $1")
            .bind(1_i32)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(row.get::<i32, _>(0), 1);
        let row = sqlx::query("SELECT $1")
            .bind(2_i64)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(row.get::<i64, _>(0), 2);
        assert_eq!(connection.cached_statements_size(), 2);
        let explicit = connection.prepare("SELECT 42").await.unwrap();
        sqlx::query("SELECT 3")
            .persistent(false)
            .execute(&mut connection)
            .await
            .unwrap();
        assert_eq!(connection.cached_statements_size(), 2);
        let row = explicit.query().fetch_one(&mut connection).await.unwrap();
        assert_eq!(row.get::<i32, _>(0), 42);
        assert_eq!(connection.cached_statements_size(), 2);
    });
}

#[test]
fn updates_accounting_when_cached_statement_is_replanned() {
    use sqlx::Row as _;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create());
    runtime.block_on(async {
        for sql in [
            "CREATE TABLE public.replanned(id INT)",
            "CREATE TEMP TABLE replanned(id INT, extra INT)",
            "INSERT INTO public.replanned VALUES (1)",
            "INSERT INTO pg_temp.replanned VALUES (2, 3)",
            "SET search_path = public, pg_temp",
        ] {
            sqlx::query(sql).execute(&mut connection).await.unwrap();
        }
        let query = "SELECT id FROM replanned WHERE id = $1";
        assert_eq!(
            sqlx::query(query)
                .bind(1_i32)
                .fetch_one(&mut connection)
                .await
                .unwrap()
                .get::<i32, _>(0),
            1
        );
        let key = (query.to_owned(), vec![Some(BaseType::Int4)]);
        let original_bytes = connection
            .state
            .lock()
            .unwrap()
            .statements
            .get(&key)
            .unwrap()
            .owned_bytes;
        sqlx::query("SET search_path = pg_temp, public")
            .execute(&mut connection)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query(query)
                .bind(2_i32)
                .fetch_one(&mut connection)
                .await
                .unwrap()
                .get::<i32, _>(0),
            2
        );
        let old_statement = {
            let state = connection.state.lock().unwrap();
            let cached = state.statements.get(&key).unwrap();
            assert_eq!(cached.statement.get_replan_revision(), 1);
            assert_eq!(
                state.statement_cache_bytes,
                state
                    .statements
                    .values()
                    .map(|cached| cached.owned_bytes)
                    .sum::<usize>()
            );
            assert!(cached.owned_bytes >= original_bytes);
            cached.statement.clone()
        };
        sqlx::query("ALTER TABLE pg_temp.replanned ADD COLUMN another INT")
            .execute(&mut connection)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query(query)
                .bind(2_i32)
                .fetch_one(&mut connection)
                .await
                .unwrap()
                .get::<i32, _>(0),
            2
        );
        let state = connection.state.lock().unwrap();
        let cached = state.statements.get(&key).unwrap();
        assert!(!Arc::ptr_eq(&cached.statement, &old_statement));
        assert_eq!(
            state.statement_cache_bytes,
            state
                .statements
                .values()
                .map(|cached| cached.owned_bytes)
                .sum::<usize>()
        );
    });
}

#[test]
fn preserves_pending_nested_rollbacks_when_futures_are_never_polled() {
    use sqlx::Row as _;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        for operation in 0..3 {
            let mut connection = PgFakeConnection::new(Db::create());
            sqlx::query("CREATE TABLE pending_drop(id INT)")
                .execute(&mut connection)
                .await
                .unwrap();
            let mut outer = connection.begin().await.unwrap();
            sqlx::query("INSERT INTO pending_drop VALUES(1)")
                .execute(&mut *outer)
                .await
                .unwrap();
            {
                let mut inner = outer.begin().await.unwrap();
                sqlx::query("INSERT INTO pending_drop VALUES(2)")
                    .execute(&mut *inner)
                    .await
                    .unwrap();
            }
            match operation {
                0 => drop(sqlx::query("SELECT * FROM pending_drop").fetch(&mut *outer)),
                1 => drop(outer.ping()),
                2 => drop((&mut *outer).prepare("SELECT * FROM pending_drop")),
                _ => unreachable!(),
            }
            let rows = sqlx::query("SELECT id FROM pending_drop ORDER BY id")
                .fetch_all(&mut *outer)
                .await
                .unwrap();
            assert_eq!(
                rows.iter()
                    .map(|row| row.get::<i32, _>(0))
                    .collect::<Vec<_>>(),
                vec![1]
            );
            drop(outer);
            match operation {
                0 => drop(sqlx::query("SELECT * FROM pending_drop").fetch(&mut connection)),
                1 => drop(connection.ping()),
                2 => drop((&mut connection).prepare("SELECT * FROM pending_drop")),
                _ => unreachable!(),
            }
            assert!(
                sqlx::query("SELECT id FROM pending_drop")
                    .fetch_all(&mut connection)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    });
}

#[test]
fn queues_rollback_without_blocking_or_overtaking_submitted_work() {
    use std::{sync::mpsc, thread, time::Duration};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut connection = PgFakeConnection::new(Db::create());
        sqlx::query("CREATE TABLE cancelled_work(id INT)")
            .execute(&mut connection)
            .await
            .unwrap();
        let state = connection.state.clone();
        let mut outer = connection.begin().await.unwrap();
        let mut inner = outer.begin().await.unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocked_state = state.clone();
        let blocker = thread::spawn(move || {
            let _guard = blocked_state.lock().unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).is_ok()
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut query =
            Box::pin(sqlx::query("INSERT INTO cancelled_work VALUES(1)").execute(&mut *inner));
        assert!(futures_util::poll!(query.as_mut()).is_pending());
        drop(query);
        drop(inner);
        release_tx.send(()).unwrap();
        assert!(
            blocker.join().unwrap(),
            "transaction drop blocked on execution state"
        );
        while Arc::strong_count(&state) > 2 {
            tokio::task::yield_now().await;
        }
        let rows = sqlx::query("SELECT id FROM cancelled_work")
            .fetch_all(&mut *outer)
            .await
            .unwrap();
        assert!(rows.is_empty(), "rollback must run after submitted INSERT");
        outer.commit().await.unwrap();
    });
}
