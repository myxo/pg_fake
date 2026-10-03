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
fn compares_generated_integer_inner_joins() {
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
        "CREATE TABLE integer_join_left (id integer, bucket integer)",
        "CREATE TABLE integer_join_right (id integer, bucket integer)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT l.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2",
        RowOrder::Unordered,
    );
    for seed in 0..8 {
        for table in ["integer_join_left", "integer_join_right"] {
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("DELETE FROM {table}"),
                RowOrder::Unordered,
            );
            let values = (0..12)
                .map(|index| {
                    let id = if (index + seed) % 7 == 0 {
                        "NULL".to_owned()
                    } else {
                        ((index * 3 + seed) % 5).to_string()
                    };
                    let bucket = if (index + seed) % 6 == 0 {
                        "NULL".to_owned()
                    } else {
                        ((index + seed) % 3).to_string()
                    };
                    format!("({id}, {bucket})")
                })
                .collect::<Vec<_>>()
                .join(", ");
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("INSERT INTO {table} VALUES {values}"),
                RowOrder::Unordered,
            );
        }
        for sql in [
            "SELECT l.id, r.id FROM integer_join_left l INNER JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON r.id = l.id WHERE 2 = l.id",
            "SELECT l.bucket, r.bucket FROM integer_join_left l JOIN integer_join_right r ON l.bucket = r.bucket WHERE l.bucket = 0",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 99",
        ] {
            assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
        }
        runtime.block_on(async {
            for key in [Some(0_i32), Some(2), None] {
                let sql = "SELECT l.id, r.id FROM integer_join_left l INNER JOIN integer_join_right r ON l.id = r.id WHERE l.id = $1";
                let mut expected: Vec<(Option<i32>, Option<i32>)> = sqlx::query_as(sql)
                    .bind(key)
                    .fetch_all(&mut postgres)
                    .await
                    .unwrap();
                let mut actual: Vec<(Option<i32>, Option<i32>)> = sqlx::query_as(sql)
                    .bind(key)
                    .fetch_all(&mut fake)
                    .await
                    .unwrap();
                expected.sort();
                actual.sort();
                assert_eq!(actual, expected, "seed={seed}, key={key:?}");
            }
        });
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2 ORDER BY l.id, r.id",
        RowOrder::Ordered,
    );
}
