use crate::{Db, error::SqlState, value::Value};
use std::{
    thread,
    time::{Duration, Instant},
};

fn wait_until_advisory_blocked(db: &Db) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if db
            .state
            .lock()
            .unwrap()
            .advisory_locks
            .lock()
            .unwrap()
            .has_waiters()
        {
            return;
        }
        assert!(Instant::now() < deadline, "advisory waiter did not block");
        thread::yield_now();
    }
}

#[test]
fn resumes_advisory_expressions_after_controlled_waits() {
    for (query, expected_value, expected_rows) in [
        (
            "SELECT nextval('advisory_sequence'), pg_advisory_xact_lock(1)",
            1,
            1,
        ),
        (
            "VALUES(nextval('advisory_sequence'),pg_advisory_xact_lock(1))",
            1,
            1,
        ),
        (
            "SELECT CASE WHEN nextval('advisory_sequence')>0 THEN pg_advisory_xact_lock(1) END",
            1,
            1,
        ),
        (
            "SELECT pg_advisory_xact_lock(nextval('advisory_sequence'))",
            1,
            1,
        ),
        (
            "SELECT nextval('advisory_sequence')>0 AND pg_advisory_xact_lock(1) IS NOT NULL",
            1,
            1,
        ),
        (
            "SELECT coalesce(nullif(nextval('advisory_sequence')>0,TRUE),pg_advisory_xact_lock(1) IS NULL)",
            1,
            1,
        ),
        (
            "SELECT pg_advisory_xact_lock(sum(nextval('advisory_sequence'))::BIGINT % 5) FROM advisory_rows",
            3,
            1,
        ),
        (
            "SELECT nextval('advisory_sequence'),pg_advisory_xact_lock(1),count(*) FROM advisory_rows",
            1,
            1,
        ),
        (
            "SELECT nextval('advisory_sequence'),pg_advisory_xact_lock(1) FROM advisory_rows GROUP BY id",
            3,
            3,
        ),
        (
            "SELECT count((pg_advisory_xact_lock(nextval('advisory_sequence')) IS NOT NULL)) FROM advisory_rows",
            3,
            1,
        ),
        (
            "SELECT pg_advisory_xact_lock(1) FROM advisory_rows LIMIT (nextval('advisory_sequence')+1)",
            1,
            2,
        ),
        (
            "SELECT pg_advisory_xact_lock(1) FROM advisory_rows OFFSET nextval('advisory_sequence')",
            1,
            2,
        ),
        (
            "INSERT INTO advisory_rows VALUES(4,(pg_advisory_xact_lock(nextval('advisory_sequence')) IS NOT NULL)::INT) RETURNING *",
            1,
            1,
        ),
        (
            "INSERT INTO advisory_rows VALUES(4,6) RETURNING nextval('advisory_sequence'),pg_advisory_xact_lock(1)",
            1,
            1,
        ),
        (
            "UPDATE advisory_rows SET n=6 RETURNING nextval('advisory_sequence'),pg_advisory_xact_lock(1)",
            3,
            3,
        ),
        (
            "DELETE FROM advisory_rows RETURNING nextval('advisory_sequence'),pg_advisory_xact_lock(1)",
            3,
            3,
        ),
        (
            "WITH x AS(UPDATE advisory_rows SET n=n+1 RETURNING id,nextval('advisory_sequence'),pg_advisory_xact_lock(id)) SELECT * FROM x",
            3,
            3,
        ),
        (
            "WITH x AS(DELETE FROM advisory_rows RETURNING id,nextval('advisory_sequence'),pg_advisory_xact_lock(id)) SELECT * FROM x",
            3,
            3,
        ),
        (
            "UPDATE advisory_rows SET n=nextval('advisory_sequence')+(pg_advisory_xact_lock(id-1) IS NULL)::INT RETURNING *",
            3,
            3,
        ),
    ] {
        for commits in [false, true] {
            let db = Db::create_builder()
                .set_lock_timeout(Duration::from_secs(3))
                .build();
            let mut holder = db.create_session();
            let mut worker = db.create_session();
            holder.execute("CREATE SEQUENCE advisory_sequence; CREATE TABLE advisory_rows(id INT PRIMARY KEY,n INT); INSERT INTO advisory_rows VALUES(1,3),(2,4),(3,5)").unwrap();
            holder
                .execute("BEGIN; SELECT pg_advisory_xact_lock(1)")
                .unwrap();
            let waiting = thread::spawn(move || {
                assert_eq!(
                    worker.query(query, &[]).unwrap().rows.len(),
                    expected_rows,
                    "{query}"
                );
                worker
                    .query("SELECT currval('advisory_sequence')", &[])
                    .unwrap()
            });
            wait_until_advisory_blocked(&db);
            holder
                .execute(if commits { "COMMIT" } else { "ROLLBACK" })
                .unwrap();
            assert_eq!(
                waiting.join().unwrap().rows,
                vec![vec![Value::Int8(expected_value)]],
                "{query}"
            );
        }
    }
}

#[test]
fn detects_mixed_advisory_deadlocks() {
    for second_resource in [
        "SELECT pg_advisory_xact_lock(2)",
        "SELECT id FROM advisory_rows FOR UPDATE",
        "LOCK TABLE advisory_rows IN ACCESS EXCLUSIVE MODE",
    ] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(3))
            .build();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first.execute("CREATE TABLE advisory_rows(id INT PRIMARY KEY); INSERT INTO advisory_rows VALUES(1)").unwrap();
        first
            .execute("BEGIN; SELECT pg_advisory_xact_lock(1)")
            .unwrap();
        second.execute("BEGIN").unwrap();
        second.execute(second_resource).unwrap();
        let waiting = thread::spawn(move || second.execute("SELECT pg_advisory_xact_lock(1)"));
        wait_until_advisory_blocked(&db);
        first.execute(second_resource).unwrap();
        assert_eq!(
            waiting.join().unwrap().unwrap_err().sqlstate,
            SqlState::DeadlockDetected,
            "{second_resource}"
        );
    }
}

#[test]
fn releases_advisory_locks_when_sessions_drop() {
    for function in ["pg_advisory_lock", "pg_advisory_xact_lock"] {
        let db = Db::create_builder()
            .set_lock_timeout(Duration::from_secs(3))
            .build();
        let mut holder = db.create_session();
        let mut worker = db.create_session();
        holder.execute("BEGIN").unwrap();
        holder.execute(&format!("SELECT {function}(1)")).unwrap();
        let waiting = thread::spawn(move || worker.query("SELECT pg_advisory_xact_lock(1)", &[]));
        wait_until_advisory_blocked(&db);
        drop(holder);
        assert_eq!(
            waiting.join().unwrap().unwrap().rows,
            vec![vec![Value::Void]]
        );
    }
}
