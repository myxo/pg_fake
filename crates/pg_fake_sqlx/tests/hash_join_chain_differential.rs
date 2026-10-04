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
}
