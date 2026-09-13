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
fn matches_row_lock_compatibility_matrix() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut holder = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut contender = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake_holder = PgFakeConnection::new(db.clone());
    let mut fake_contender = PgFakeConnection::new(db);
    for sql in [
        "CREATE TABLE lock_matrix (id INTEGER PRIMARY KEY)",
        "INSERT INTO lock_matrix VALUES (1)",
    ] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            sql,
            RowOrder::Ordered,
        );
    }
    for held in ["KEY SHARE", "SHARE", "NO KEY UPDATE", "UPDATE"] {
        for requested in ["KEY SHARE", "SHARE", "NO KEY UPDATE", "UPDATE"] {
            assert_statement(
                &runtime,
                &mut holder,
                &mut fake_holder,
                "BEGIN",
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut contender,
                &mut fake_contender,
                "BEGIN",
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut holder,
                &mut fake_holder,
                &format!("SELECT id FROM lock_matrix FOR {held}"),
                RowOrder::Ordered,
            );
            assert_statement_allow_error(
                &runtime,
                &mut contender,
                &mut fake_contender,
                &format!("SELECT id FROM lock_matrix FOR {requested} NOWAIT"),
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut contender,
                &mut fake_contender,
                "ROLLBACK",
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut holder,
                &mut fake_holder,
                "ROLLBACK",
                RowOrder::Ordered,
            );
        }
    }
}

#[test]
fn matches_skip_locked_work_queue_and_locked_identities() {
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
    let mut observer = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake_holder = PgFakeConnection::new(db.clone());
    let mut fake_worker = PgFakeConnection::new(db.clone());
    let mut fake_observer = PgFakeConnection::new(db);
    for sql in [
        "CREATE TABLE lock_jobs (id INTEGER PRIMARY KEY, priority INTEGER)",
        "INSERT INTO lock_jobs VALUES (1,10),(2,50),(3,30),(4,40),(5,20)",
        "CREATE TABLE lock_tags (job_id INTEGER, tag TEXT)",
        "INSERT INTO lock_tags VALUES (1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e')",
        "CREATE VIEW lock_ready AS SELECT id,priority FROM lock_jobs WHERE priority>0",
    ] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            sql,
            RowOrder::Ordered,
        );
    }
    for query in [
        "SELECT id FROM lock_jobs ORDER BY priority DESC LIMIT 2 FOR UPDATE SKIP LOCKED",
        "SELECT id FROM lock_jobs ORDER BY priority DESC LIMIT 2 OFFSET 1 FOR UPDATE SKIP LOCKED",
        "SELECT j.id,t.tag FROM lock_jobs j JOIN lock_tags t ON t.job_id=j.id ORDER BY j.priority DESC LIMIT 2 FOR UPDATE OF j SKIP LOCKED",
        "SELECT x.id FROM (SELECT * FROM lock_jobs) x ORDER BY x.priority DESC LIMIT 2 FOR NO KEY UPDATE OF x SKIP LOCKED",
        "SELECT id FROM lock_ready ORDER BY priority DESC LIMIT 2 FOR UPDATE SKIP LOCKED",
        "SELECT x.id FROM (SELECT * FROM lock_jobs FOR UPDATE SKIP LOCKED) x WHERE x.id<4 ORDER BY x.id",
        "SELECT x.id FROM ((SELECT * FROM lock_jobs)) x WHERE x.id<4 ORDER BY x.id FOR UPDATE SKIP LOCKED",
        "WITH chosen AS (SELECT id FROM lock_jobs ORDER BY priority DESC LIMIT 2 FOR UPDATE SKIP LOCKED) SELECT id FROM chosen ORDER BY id",
    ] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            "BEGIN",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut worker,
            &mut fake_worker,
            "BEGIN",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            "SELECT id FROM lock_jobs WHERE id IN (2,4) ORDER BY id FOR UPDATE",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut worker,
            &mut fake_worker,
            query,
            RowOrder::Ordered,
        );
        for id in 1..=5 {
            assert_statement(
                &runtime,
                &mut observer,
                &mut fake_observer,
                "BEGIN",
                RowOrder::Ordered,
            );
            assert_statement_allow_error(
                &runtime,
                &mut observer,
                &mut fake_observer,
                &format!("SELECT id FROM lock_jobs WHERE id={id} FOR UPDATE NOWAIT"),
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut observer,
                &mut fake_observer,
                "ROLLBACK",
                RowOrder::Ordered,
            );
        }
        assert_statement(
            &runtime,
            &mut observer,
            &mut fake_observer,
            "ROLLBACK",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut worker,
            &mut fake_worker,
            "ROLLBACK",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            "ROLLBACK",
            RowOrder::Ordered,
        );
    }
}

#[test]
fn matches_row_lock_composition_and_errors() {
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
        "CREATE TABLE lock_a (id INTEGER PRIMARY KEY, value INTEGER)",
        "CREATE TABLE lock_b (id INTEGER PRIMARY KEY)",
        "INSERT INTO lock_a VALUES (1,10),(2,20)",
        "INSERT INTO lock_b VALUES (1)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    for sql in [
        "SELECT a.id,b.id FROM lock_a a JOIN lock_b b USING(id) FOR SHARE OF a FOR NO KEY UPDATE OF b NOWAIT",
        "SELECT a.id,b.id FROM lock_a a JOIN lock_b b USING(id) FOR UPDATE OF a,b SKIP LOCKED",
        "SELECT a.id,b.id FROM lock_a a LEFT JOIN lock_b b USING(id) ORDER BY a.id FOR UPDATE OF a",
        "SELECT a.id,b.id FROM lock_a a LEFT JOIN lock_b b USING(id) FOR UPDATE OF b",
        "SELECT * FROM lock_a a FOR UPDATE OF lock_a",
        "SELECT * FROM lock_a FOR UPDATE OF missing",
        "SELECT * FROM lock_a FOR UPDATE OF public.lock_a",
        "SELECT count(*) FROM lock_a FOR UPDATE",
        "SELECT id,count(*) FROM lock_a GROUP BY id FOR SHARE",
        "SELECT id FROM lock_a HAVING TRUE FOR UPDATE",
        "SELECT DISTINCT value FROM lock_a FOR KEY SHARE",
        "SELECT id,row_number() OVER() FROM lock_a FOR NO KEY UPDATE",
        "SELECT id FROM lock_a UNION SELECT id FROM lock_b FOR UPDATE",
        "SELECT * FROM (SELECT count(*) FROM lock_a) x FOR UPDATE",
        "WITH x AS (SELECT * FROM lock_a) SELECT * FROM x FOR UPDATE OF x",
        "SELECT * FROM lock_a FOR KEY SHARE FOR UPDATE NOWAIT",
        "WITH x AS (SELECT * FROM lock_a WHERE FALSE) SELECT * FROM x FOR UPDATE",
        "INSERT INTO lock_b SELECT id FROM lock_a WHERE id=2 FOR KEY SHARE",
        "UPDATE lock_b SET id=id WHERE EXISTS(SELECT 1 FROM lock_a WHERE id=2 FOR UPDATE)",
        "SELECT * FROM (VALUES(1)) x FOR UPDATE",
        "SELECT * FROM (VALUES(1)) x FOR UPDATE OF x",
        "SELECT * FROM (lock_a a JOIN lock_b b USING(id)) j FOR UPDATE OF j",
        "SELECT * FROM (lock_a a JOIN lock_b b USING(id)) j FOR UPDATE OF a",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
}

#[test]
fn matches_key_changes_and_foreign_key_lock_strength() {
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
    for sql in [
        "CREATE TABLE key_locks (id INT PRIMARY KEY, key INT UNIQUE, value INT)",
        "INSERT INTO key_locks VALUES (1,10,100)",
        "CREATE TABLE key_children (parent INT REFERENCES key_locks(id))",
    ] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            sql,
            RowOrder::Ordered,
        );
    }
    assert_statement(
        &runtime,
        &mut worker,
        &mut fake_worker,
        "SET lock_timeout='20ms'",
        RowOrder::Ordered,
    );
    for strength in ["KEY SHARE", "SHARE", "NO KEY UPDATE", "UPDATE"] {
        for sql in [
            "UPDATE key_locks SET value=value+1",
            "UPDATE key_locks SET id=id,key=key",
            "UPDATE key_locks SET id=id+1",
            "UPDATE key_locks SET key=key+1",
            "DELETE FROM key_locks",
            "INSERT INTO key_children VALUES(1)",
        ] {
            assert_statement(
                &runtime,
                &mut holder,
                &mut fake_holder,
                "BEGIN",
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut holder,
                &mut fake_holder,
                &format!("SELECT id FROM key_locks FOR {strength}"),
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut worker,
                &mut fake_worker,
                "BEGIN",
                RowOrder::Ordered,
            );
            assert_statement_allow_error(
                &runtime,
                &mut worker,
                &mut fake_worker,
                sql,
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut worker,
                &mut fake_worker,
                "ROLLBACK",
                RowOrder::Ordered,
            );
            assert_statement(
                &runtime,
                &mut holder,
                &mut fake_holder,
                "ROLLBACK",
                RowOrder::Ordered,
            );
        }
    }
}

#[test]
fn matches_locking_volatile_projections_and_duplicate_rows() {
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
    for sql in [
        "CREATE TABLE volatile_locks(id INT)",
        "INSERT INTO volatile_locks VALUES(1),(2),(3)",
        "CREATE SEQUENCE lock_sequence",
    ] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            sql,
            RowOrder::Ordered,
        );
    }
    assert_statement(
        &runtime,
        &mut holder,
        &mut fake_holder,
        "BEGIN",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut holder,
        &mut fake_holder,
        "SELECT id FROM volatile_locks WHERE id=1 FOR UPDATE",
        RowOrder::Ordered,
    );
    for sql in [
        "SELECT nextval('lock_sequence'),id FROM volatile_locks ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED",
        "WITH x AS (SELECT nextval('lock_sequence') n,id FROM volatile_locks ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED) SELECT * FROM x",
        "SELECT nextval('lock_sequence'),id FROM volatile_locks ORDER BY id LIMIT 0 FOR UPDATE SKIP LOCKED",
        "SELECT n,id FROM (SELECT nextval('lock_sequence') n,id FROM volatile_locks) x LIMIT 1 FOR UPDATE SKIP LOCKED",
        "SELECT nextval('lock_sequence'), (SELECT id FROM volatile_locks WHERE id=2 FOR UPDATE)",
        "SELECT currval('lock_sequence')",
        "SELECT p.id FROM volatile_locks p WHERE nextval('lock_sequence')>0 AND EXISTS(SELECT 1 FROM volatile_locks q WHERE q.id=p.id FOR UPDATE SKIP LOCKED)",
        "SELECT currval('lock_sequence')",
        "SELECT p.id FROM volatile_locks p WHERE nextval('lock_sequence')<0 OR EXISTS(SELECT 1 FROM volatile_locks q WHERE q.id=p.id FOR UPDATE SKIP LOCKED)",
        "SELECT currval('lock_sequence')",
        "SELECT CASE WHEN nextval('lock_sequence')>0 THEN (SELECT id FROM volatile_locks q WHERE q.id=p.id FOR UPDATE SKIP LOCKED) ELSE 0 END FROM volatile_locks p",
        "SELECT currval('lock_sequence')",
        "SELECT CASE nextval('lock_sequence')>0 WHEN TRUE THEN (SELECT id FROM volatile_locks q WHERE q.id=p.id FOR UPDATE SKIP LOCKED) ELSE 0 END FROM volatile_locks p",
        "SELECT currval('lock_sequence')",
        "SELECT coalesce(nullif(nextval('lock_sequence')>0,TRUE),(SELECT id>0 FROM volatile_locks q WHERE q.id=p.id FOR UPDATE SKIP LOCKED)) FROM volatile_locks p",
        "SELECT currval('lock_sequence')",
    ] {
        assert_statement(
            &runtime,
            &mut worker,
            &mut fake_worker,
            sql,
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
    for sql in [
        "CREATE TABLE duplicate_locks(value INT)",
        "INSERT INTO duplicate_locks VALUES(NULL),(NULL),(NULL)",
    ] {
        assert_statement(
            &runtime,
            &mut holder,
            &mut fake_holder,
            sql,
            RowOrder::Ordered,
        );
    }
    assert_statement(
        &runtime,
        &mut holder,
        &mut fake_holder,
        "BEGIN",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut holder,
        &mut fake_holder,
        "SELECT * FROM duplicate_locks LIMIT 1 FOR UPDATE",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut worker,
        &mut fake_worker,
        "SELECT * FROM duplicate_locks LIMIT 3 FOR UPDATE SKIP LOCKED",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut holder,
        &mut fake_holder,
        "ROLLBACK",
        RowOrder::Ordered,
    );
}

#[test]
fn matches_nested_lock_demand_and_inheritance() {
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
    for sql in [
        "CREATE TABLE nested_a(id INT PRIMARY KEY, v INT)",
        "INSERT INTO nested_a VALUES(1,10),(2,20),(3,30)",
        "CREATE TABLE nested_b(id INT PRIMARY KEY)",
        "INSERT INTO nested_b VALUES(1),(2),(3)",
        "CREATE VIEW nested_view AS SELECT * FROM nested_a FOR UPDATE",
        "CREATE SEQUENCE nested_seq",
    ] {
        assert_statement(
            &runtime,
            &mut postgres[0],
            &mut fake[0],
            sql,
            RowOrder::Ordered,
        );
    }
    for (held, query) in [
        (
            None,
            "SELECT id FROM (SELECT id FROM (SELECT id FROM nested_a FOR UPDATE) a UNION ALL SELECT id FROM (SELECT id FROM nested_b FOR UPDATE) b) x LIMIT 1",
        ),
        (
            None,
            "SELECT id FROM ((SELECT id FROM nested_a FOR UPDATE) UNION ALL (SELECT id FROM nested_b FOR UPDATE)) x LIMIT 1",
        ),
        (
            Some("SELECT id FROM nested_a WHERE id=1 FOR UPDATE"),
            "WITH claimed AS (SELECT id FROM nested_a ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED) UPDATE nested_a SET v=v+1 FROM claimed WHERE nested_a.id=claimed.id RETURNING nested_a.id",
        ),
        (
            Some("SELECT id FROM nested_a WHERE id=1 FOR UPDATE"),
            "WITH claimed AS (SELECT id FROM nested_a ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED) DELETE FROM nested_a USING claimed WHERE nested_a.id=claimed.id RETURNING nested_a.id",
        ),
        (
            None,
            "SELECT x.id FROM ((SELECT * FROM nested_a FOR UPDATE) x CROSS JOIN (VALUES(1)) y(z)) LIMIT 1",
        ),
        (
            None,
            "SELECT x.id FROM (SELECT * FROM nested_a FOR UPDATE) x RIGHT JOIN (VALUES(1)) y(z) ON TRUE LIMIT 1",
        ),
        (
            None,
            "SELECT * FROM (SELECT * FROM nested_a FOR UPDATE) x LIMIT 1",
        ),
        (
            None,
            "SELECT x.id FROM (SELECT * FROM nested_a FOR UPDATE) x CROSS JOIN (VALUES(1)) y(z) LIMIT 1",
        ),
        (
            None,
            "SELECT x.id FROM (VALUES(1)) y(z) CROSS JOIN (SELECT * FROM nested_a FOR UPDATE) x LIMIT 1",
        ),
        (
            None,
            "SELECT CASE WHEN FALSE THEN (SELECT id FROM nested_a LIMIT 1 FOR UPDATE) ELSE 0 END",
        ),
        (
            None,
            "SELECT COALESCE(0, (SELECT id FROM nested_a LIMIT 1 FOR UPDATE))",
        ),
        (
            None,
            "SELECT p.id,x.n FROM (VALUES(1),(2)) p(id) CROSS JOIN LATERAL (SELECT nextval('nested_seq') n FROM nested_a WHERE nested_a.id=p.id FOR UPDATE) x",
        ),
        (
            None,
            "SELECT p.id,x.n FROM (VALUES(1),(1)) p(id) CROSS JOIN LATERAL (SELECT nextval('nested_seq') n FROM nested_a WHERE nested_a.id=p.id FOR UPDATE) x",
        ),
        (
            None,
            "SELECT EXISTS(SELECT * FROM nested_a FOR UPDATE) AS present",
        ),
        (
            None,
            "WITH x AS (SELECT * FROM nested_a FOR UPDATE) SELECT * FROM x LIMIT 1",
        ),
        (
            None,
            "WITH x AS (SELECT * FROM nested_a FOR SHARE) SELECT * FROM x LIMIT 1 FOR UPDATE",
        ),
        (
            None,
            "SELECT * FROM (SELECT * FROM nested_a FOR UPDATE OFFSET 0) x WHERE id=2 LIMIT 1",
        ),
        (
            None,
            "SELECT * FROM (SELECT * FROM nested_a ORDER BY id LIMIT 2 OFFSET 1 FOR UPDATE) x WHERE id=2 LIMIT 1",
        ),
        (
            None,
            "SELECT EXISTS(SELECT * FROM nested_a LIMIT 2 OFFSET 1 FOR UPDATE) AS present",
        ),
        (
            None,
            "SELECT * FROM (SELECT * FROM nested_a LIMIT 1 OFFSET 1) x FOR UPDATE",
        ),
        (
            None,
            "SELECT * FROM (SELECT * FROM nested_a LIMIT 1 OFFSET 1) x WHERE id=3 FOR UPDATE",
        ),
        (
            Some("SELECT * FROM nested_a WHERE id=1 FOR KEY SHARE"),
            "SELECT * FROM (SELECT * FROM nested_a FOR SHARE NOWAIT) x FOR UPDATE SKIP LOCKED",
        ),
        (
            Some("SELECT * FROM nested_b WHERE id=1 FOR UPDATE"),
            "SELECT a.id,b.id FROM nested_a a JOIN nested_b b USING(id) WHERE a.id=1 FOR UPDATE SKIP LOCKED",
        ),
        (None, "SELECT * FROM nested_view LIMIT 1"),
        (
            Some("SELECT * FROM nested_a WHERE id=1 FOR KEY SHARE"),
            "INSERT INTO nested_a VALUES(1,11) ON CONFLICT(id) DO UPDATE SET v=EXCLUDED.v",
        ),
        (
            None,
            "SELECT nextval('nested_seq'), id FROM (SELECT * FROM nested_a FOR UPDATE) x LIMIT 2",
        ),
    ] {
        eprintln!("nested lock case: {query}");
        for session in 0..2 {
            assert_statement(
                &runtime,
                &mut postgres[session],
                &mut fake[session],
                "BEGIN",
                RowOrder::Ordered,
            );
        }
        if let Some(held) = held {
            assert_statement(
                &runtime,
                &mut postgres[0],
                &mut fake[0],
                held,
                RowOrder::Ordered,
            );
        }
        assert_statement_allow_error(
            &runtime,
            &mut postgres[1],
            &mut fake[1],
            query,
            RowOrder::Ordered,
        );
        for table in ["nested_a", "nested_b"] {
            for id in 1..=3 {
                assert_statement_allow_error(
                    &runtime,
                    &mut postgres[2],
                    &mut fake[2],
                    &format!("SELECT id FROM {table} WHERE id={id} FOR UPDATE NOWAIT"),
                    RowOrder::Ordered,
                );
            }
        }
        for session in 0..2 {
            assert_statement(
                &runtime,
                &mut postgres[session],
                &mut fake[session],
                "ROLLBACK",
                RowOrder::Ordered,
            );
        }
    }
}

#[test]
fn releases_implicit_nested_select_locks() {
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
        "CREATE TABLE implicit_locks(id INT PRIMARY KEY)",
        "INSERT INTO implicit_locks VALUES(1),(2)",
        "CREATE VIEW implicit_view AS SELECT * FROM implicit_locks FOR UPDATE",
        "SELECT * FROM (SELECT * FROM implicit_locks FOR UPDATE) x",
        "SELECT * FROM implicit_locks FOR UPDATE NOWAIT",
        "SELECT * FROM implicit_view",
        "SELECT * FROM implicit_locks FOR UPDATE NOWAIT",
        "WITH x AS (SELECT * FROM implicit_locks FOR UPDATE) SELECT * FROM x LIMIT 1",
        "SELECT * FROM implicit_locks FOR UPDATE NOWAIT",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}
