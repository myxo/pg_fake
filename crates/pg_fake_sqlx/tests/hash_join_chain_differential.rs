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
fn compares_multi_table_hash_join_chains() {
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
        "CREATE TABLE hash_chain_a (id integer, name text)",
        "CREATE TABLE hash_chain_b (id integer, name text)",
        "CREATE TABLE hash_chain_c (id integer, name text)",
        "CREATE TABLE hash_chain_d (id integer, name text)",
        "INSERT INTO hash_chain_a VALUES (1,'a1'), (2,'a2'), (3,'a3'), (NULL,'an')",
        "INSERT INTO hash_chain_b VALUES (1,'b1'), (1,'bx'), (2,'b2'), (4,'b4'), (NULL,'bn')",
        "INSERT INTO hash_chain_c VALUES (1,'c1'), (1,'cx'), (2,'c2'), (5,'c5'), (NULL,'cn')",
        "INSERT INTO hash_chain_d VALUES (1,'d1'), (2,'d2'), (3,'d3'), (NULL,'dn')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for sql in [
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id",
        "SELECT c.name,a.name,b.name FROM hash_chain_a a JOIN hash_chain_b b ON b.id=a.id JOIN hash_chain_c c ON c.id=a.id",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a LEFT JOIN hash_chain_b b ON a.id=b.id LEFT JOIN hash_chain_c c ON b.id=c.id",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id LEFT JOIN hash_chain_c c ON b.id=c.id",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a LEFT JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id WHERE a.id=1",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id WHERE a.id=99",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a LEFT JOIN hash_chain_b b ON a.id=b.id LEFT JOIN hash_chain_c c ON b.id=c.id WHERE a.id=3",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a LEFT JOIN hash_chain_b b ON a.id=b.id LEFT JOIN hash_chain_c c ON b.id=c.id WHERE a.id IS NULL",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id WHERE b.name='b1'",
        "SELECT a.name,b.name,c.name,d.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id JOIN hash_chain_d d ON a.id=d.id",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_a c ON a.id=c.id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id ORDER BY a.name,b.name,c.name",
        RowOrder::Ordered,
    );
    runtime.block_on(async {
        let sql = "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id WHERE a.id=$1";
        for key in [Some(1_i32), Some(2), Some(3), None] {
            let mut expected: Vec<(String, String, String)> = sqlx::query_as(sql)
                .bind(key)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let mut actual: Vec<(String, String, String)> = sqlx::query_as(sql)
                .bind(key)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            expected.sort();
            actual.sort();
            assert_eq!(actual, expected, "key={key:?}");
        }
    });
    for sql in [
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id",
        "COMMIT",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "DELETE FROM hash_chain_c",
        RowOrder::Unordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT a.name,b.name,c.name FROM hash_chain_a a JOIN hash_chain_b b ON a.id=b.id JOIN hash_chain_c c ON b.id=c.id",
        RowOrder::Unordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT a.name,b.name,c.name FROM hash_chain_a a LEFT JOIN hash_chain_b b ON a.id=b.id LEFT JOIN hash_chain_c c ON b.id=c.id",
        RowOrder::Unordered,
    );
    for sql in [
        "CREATE TABLE hash_probe_a (id integer, name text)",
        "CREATE TABLE hash_probe_b (id integer PRIMARY KEY, name text)",
        "CREATE TABLE hash_probe_c (id integer PRIMARY KEY, name text)",
        "INSERT INTO hash_probe_a VALUES (1,'a1'), (1,'ax'), (2,'a2'), (3,'a3'), (NULL,'an')",
        "INSERT INTO hash_probe_b VALUES (1,'b1'), (2,'b2'), (4,'b4')",
        "INSERT INTO hash_probe_c VALUES (1,'c1'), (4,'c4')",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    for sql in [
        "SELECT a.name,b.name,c.name FROM hash_probe_a a JOIN hash_probe_b b ON a.id=b.id JOIN hash_probe_c c ON b.id=c.id WHERE a.id=1",
        "SELECT a.name,b.name,c.name FROM hash_probe_a a JOIN hash_probe_b b ON a.id=b.id JOIN hash_probe_c c ON b.id=c.id WHERE a.id=2",
        "SELECT a.name,b.name,c.name FROM hash_probe_a a JOIN hash_probe_b b ON a.id=b.id JOIN hash_probe_c c ON a.id=c.id WHERE a.id=1",
        "SELECT a.name,b.name,c.name FROM hash_probe_a a LEFT JOIN hash_probe_b b ON a.id=b.id LEFT JOIN hash_probe_c c ON b.id=c.id WHERE a.id=3",
        "SELECT a.name,b.name,c.name FROM hash_probe_a a JOIN hash_probe_b b ON a.id=b.id JOIN hash_probe_c c ON b.id=c.id WHERE b.name='b1'",
        "SELECT a.name,b.name,c.name FROM hash_probe_a a JOIN hash_probe_b b ON a.id=b.id JOIN hash_probe_c c ON b.id=c.id WHERE c.name='c1'",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        RowOrder::Unordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT a.name,b.name,c.name FROM hash_probe_a a JOIN hash_probe_b b ON a.id=b.id JOIN hash_probe_c c ON b.id=c.id WHERE a.id=1",
        RowOrder::Unordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "COMMIT",
        RowOrder::Unordered,
    );
    for data_type in ["smallint", "bigint"] {
        for name in ["a", "b", "c"] {
            let key = if name == "a" { "" } else { " PRIMARY KEY" };
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("CREATE TABLE hash_{data_type}_{name} (id {data_type}{key}, name text)"),
                RowOrder::Unordered,
            );
            let values = match name {
                "a" => "(1,'one'), (1,'another'), (2,'two'), (3,'three'), (NULL,'missing')",
                "b" => "(1,'one'), (2,'two'), (4,'four')",
                _ => "(1,'one'), (4,'four')",
            };
            let large_value = if data_type == "bigint" {
                ", (5000000000,'large')"
            } else {
                ""
            };
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("INSERT INTO hash_{data_type}_{name} VALUES {values}{large_value}"),
                RowOrder::Unordered,
            );
        }
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "SELECT c.name,a.name,b.name FROM hash_{data_type}_a a JOIN hash_{data_type}_b b ON b.id=a.id JOIN hash_{data_type}_c c ON c.id=b.id"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "SELECT a.name,b.name,c.name FROM hash_{data_type}_a a JOIN hash_{data_type}_b b ON a.id=b.id JOIN hash_{data_type}_c c ON b.id=c.id WHERE a.id=1"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "SELECT a.name,b.name,c.name FROM hash_{data_type}_a a JOIN hash_{data_type}_b b ON a.id=b.id JOIN hash_{data_type}_c c ON b.id=c.id WHERE a.id IS NULL"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "SELECT a.name,b.name,c.name FROM hash_{data_type}_a a JOIN hash_{data_type}_b b ON a.id=b.id JOIN hash_{data_type}_c c ON b.id=c.id WHERE a.id=2"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "SELECT a.name,b.name,c.name FROM hash_{data_type}_a a JOIN hash_{data_type}_b b ON a.id=b.id JOIN hash_{data_type}_c c ON b.id=c.id WHERE a.id=3"
            ),
            RowOrder::Unordered,
        );
        if data_type == "bigint" {
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                "SELECT a.name,b.name,c.name FROM hash_bigint_a a JOIN hash_bigint_b b ON a.id=b.id JOIN hash_bigint_c c ON b.id=c.id WHERE a.id=5000000000",
                RowOrder::Unordered,
            );
        }
    }
    for sql in [
        "CREATE TABLE hash_mixed_a (id smallint, name text)",
        "CREATE TABLE hash_mixed_b (id smallint, next_id bigint, name text)",
        "CREATE TABLE hash_mixed_c (id bigint, name text)",
        "INSERT INTO hash_mixed_a VALUES (1,'a1'), (1,'a2'), (2,'a3'), (NULL,'an')",
        "INSERT INTO hash_mixed_b VALUES (1,5000000000,'b1'), (2,2,'b2'), (NULL,3,'bn')",
        "INSERT INTO hash_mixed_c VALUES (5000000000,'c1'), (5000000000,'c2'), (2,'c3'), (NULL,'cn')",
        "SELECT a.name,b.name,c.name FROM hash_mixed_a a JOIN hash_mixed_b b ON a.id=b.id JOIN hash_mixed_c c ON b.next_id=c.id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
}

#[test]
fn compares_prepared_unfiltered_left_joins() {
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
        "CREATE TABLE prepared_left_l (id integer, name text)",
        "CREATE TABLE prepared_left_r (id integer, name text)",
        "INSERT INTO prepared_left_l VALUES (1,'a1'), (1,'a2'), (2,'a3'), (3,'a4'), (NULL,'an')",
        "INSERT INTO prepared_left_r VALUES (1,'b1'), (1,'b2'), (4,'b4'), (NULL,'bn')",
        "SELECT l.name,r.name FROM prepared_left_l l LEFT JOIN prepared_left_r r ON l.id=r.id",
        "SELECT r.name,l.name FROM prepared_left_l l LEFT OUTER JOIN prepared_left_r r ON r.id=l.id",
        "SELECT l.name,r.name FROM prepared_left_l l LEFT JOIN prepared_left_r r ON l.id=r.id WHERE r.id IS NULL",
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "SELECT l.name,r.name FROM prepared_left_l l LEFT JOIN prepared_left_r r ON l.id=r.id",
        "COMMIT",
        "DELETE FROM prepared_left_r",
        "SELECT l.name,r.name FROM prepared_left_l l LEFT JOIN prepared_left_r r ON l.id=r.id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
}

#[test]
fn compares_prepared_wide_integer_joins() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for (suffix, data_type, matched_key) in [
        ("small", "smallint", "123"),
        ("big", "bigint", "3000000000"),
    ] {
        let left = format!("prepared_wide_{suffix}_l");
        let right = format!("prepared_wide_{suffix}_r");
        for sql in [
            format!("CREATE TABLE {left} (id {data_type}, name text)"),
            format!("CREATE TABLE {right} (id {data_type}, name text)"),
            format!(
                "INSERT INTO {left} VALUES ({matched_key},'a1'), ({matched_key},'a2'), (2,'a3'), (NULL,'an')"
            ),
            format!(
                "INSERT INTO {right} VALUES ({matched_key},'b1'), ({matched_key},'b2'), (4,'b4'), (NULL,'bn')"
            ),
            format!("SELECT l.name,r.name FROM {left} l LEFT JOIN {right} r ON l.id=r.id"),
            format!("SELECT l.id,r.id FROM {left} l LEFT JOIN {right} r ON l.id=r.id"),
            format!("SELECT r.name,l.name FROM {left} l LEFT OUTER JOIN {right} r ON r.id=l.id"),
            format!("SELECT l.name,r.name FROM {left} l JOIN {right} r ON l.id=r.id"),
            format!(
                "SELECT l.name,r.name FROM {left} l LEFT JOIN {right} r ON l.id=r.id WHERE r.id IS NULL"
            ),
            "BEGIN ISOLATION LEVEL SERIALIZABLE".to_owned(),
            format!("SELECT l.name,r.name FROM {left} l LEFT JOIN {right} r ON l.id=r.id"),
            "COMMIT".to_owned(),
            format!("DELETE FROM {right}"),
            format!("SELECT l.id,r.id FROM {left} l LEFT JOIN {right} r ON l.id=r.id"),
        ] {
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &sql,
                RowOrder::Unordered,
            );
        }
    }
}

#[test]
fn compares_generated_indexed_membership_joins() {
    use chaos_theory::{check, make::int_in};
    use std::cell::RefCell;
    use uuid::Uuid;

    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    check(|src| {
        let mut postgres = postgres.borrow_mut();
        let mut fake = PgFakeConnection::new(Db::create());
        for sql in [
            "CREATE TEMP TABLE membership_source (id INTEGER PRIMARY KEY, hub_id UUID, user_id UUID, created_at INTEGER, deleted_at INTEGER)",
            "CREATE INDEX membership_source_hub ON membership_source (hub_id)",
            "CREATE INDEX membership_source_user ON membership_source (user_id)",
            "CREATE TEMP TABLE membership_hubs (id UUID PRIMARY KEY, name TEXT, deleted_at INTEGER)",
            "CREATE TEMP TABLE membership_users (id UUID PRIMARY KEY, hub_id UUID, name TEXT)",
        ] {
            assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
        }
        for id in 1..=5 {
            let uuid = Uuid::from_u128(id);
            let personal = Uuid::from_u128(src.any_of("personal", int_in(1_u64..=6)) as u128);
            let deleted = if src.any_of("deleted_hub", int_in(0..=3)) == 0 {
                "1"
            } else {
                "NULL"
            };
            for sql in [
                format!("INSERT INTO membership_hubs VALUES ('{uuid}', 'hub-{id}', {deleted})"),
                format!(
                    "INSERT INTO membership_users VALUES ('{uuid}', '{personal}', 'user-{id}')"
                ),
            ] {
                assert_statement(
                    &runtime,
                    &mut postgres,
                    &mut fake,
                    &sql,
                    RowOrder::Unordered,
                );
            }
        }
        for id in 0..src.any_of("rows", int_in(1..=24)) {
            let hub = Uuid::from_u128(src.any_of("hub", int_in(1_u64..=6)) as u128);
            let user = if src.any_of("null_user", int_in(0..=4)) == 0 {
                "NULL".to_owned()
            } else {
                format!(
                    "'{}'",
                    Uuid::from_u128(src.any_of("user", int_in(1_u64..=6)) as u128)
                )
            };
            let created = src.any_of("created", int_in(0..=3));
            let deleted = if src.any_of("deleted_membership", int_in(0..=3)) == 0 {
                "1"
            } else {
                "NULL"
            };
            let sql = format!(
                "INSERT INTO membership_source VALUES ({id}, '{hub}', {user}, {created}, {deleted})"
            );
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &sql,
                RowOrder::Unordered,
            );
        }
        runtime.block_on(async {
            for sql in [
                "SELECT m.id, h.name, u.name, (u.hub_id = h.id) AS personal FROM membership_source m JOIN membership_hubs h ON h.id = m.hub_id JOIN membership_users u ON m.user_id = u.id WHERE m.user_id = $1 AND m.deleted_at IS NULL AND h.deleted_at IS NULL ORDER BY m.created_at, m.id DESC",
                "SELECT m.id, h.name, personal.name, (u.hub_id = h.id) AS personal FROM membership_source m JOIN membership_hubs h ON m.hub_id = h.id JOIN membership_users u ON u.id = m.user_id JOIN membership_hubs personal ON personal.id = u.hub_id WHERE m.hub_id = $1 AND m.deleted_at IS NULL AND h.deleted_at IS NULL AND personal.deleted_at IS NULL ORDER BY m.created_at DESC, m.id",
            ] {
                for key in [Some(Uuid::from_u128(1)), Some(Uuid::from_u128(2)), Some(Uuid::from_u128(6)), None] {
                    type Row = (i32, Option<String>, Option<String>, Option<bool>);
                    let expected: Vec<Row> = sqlx::query_as(sql).bind(key).fetch_all(&mut *postgres).await.unwrap();
                    let actual: Vec<Row> = sqlx::query_as(sql).bind(key).fetch_all(&mut fake).await.unwrap();
                    assert_eq!(actual, expected, "{sql}: {key:?}");
                }
            }
        });
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            "DROP TABLE membership_source, membership_users, membership_hubs",
            RowOrder::Unordered,
        );
    });
}
