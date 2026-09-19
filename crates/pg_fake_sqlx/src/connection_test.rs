use super::*;

#[test]
fn evicts_least_recently_used_statements_within_the_byte_limit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create()).set_statement_cache_limit_bytes(16);
    runtime.block_on(async {
        sqlx::query("SELECT 1")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("SELECT 2")
            .execute(&mut connection)
            .await
            .unwrap();
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
    assert_eq!(state.statement_cache_bytes, 16);
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
