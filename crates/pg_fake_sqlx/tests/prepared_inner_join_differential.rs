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
            let mut values = (0..12)
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
                .collect::<Vec<_>>();
            values.push("(2, NULL)".to_owned());
            let values = values.join(", ");
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &format!("INSERT INTO {table} VALUES {values}"),
                RowOrder::Unordered,
            );
        }
        for sql in [
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id",
            "SELECT l.bucket, r.bucket FROM integer_join_left l JOIN integer_join_right r ON l.bucket = r.bucket",
            "SELECT l.id, r.id FROM integer_join_left l INNER JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON r.id = l.id WHERE 2 = l.id",
            "SELECT l.bucket, r.bucket FROM integer_join_left l JOIN integer_join_right r ON l.bucket = r.bucket WHERE l.bucket = 0",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 99",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2 AND r.id = 2",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2 AND r.id = 1",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE r.bucket = 0 AND l.id = 2",
            "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE r.id = 2",
            "SELECT l.id, r.bucket FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE r.bucket = 0",
            "SELECT r.id, l.bucket FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE 2 = r.id",
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
                let sql = "SELECT r.id, l.bucket FROM integer_join_left l INNER JOIN integer_join_right r ON l.id = r.id WHERE r.id = $1";
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
                assert_eq!(actual, expected, "seed={seed}, right key={key:?}");
            }
            let sql = "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = $1 AND r.bucket = $2";
            let mut expected: Vec<(Option<i32>, Option<i32>)> = sqlx::query_as(sql)
                .bind(Some(2_i32))
                .bind(None::<i32>)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let mut actual: Vec<(Option<i32>, Option<i32>)> = sqlx::query_as(sql)
                .bind(Some(2_i32))
                .bind(None::<i32>)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            expected.sort();
            actual.sort();
            assert_eq!(actual, expected, "seed={seed}, right filter NULL");
        });
    }
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT l.id, r.id FROM integer_join_left l JOIN integer_join_right r ON l.id = r.id WHERE l.id = 2 ORDER BY l.id, r.id",
        RowOrder::Ordered,
    );
    for sql in [
        "CREATE TABLE indexed_join_left (id integer PRIMARY KEY)",
        "CREATE TABLE indexed_join_right (id integer PRIMARY KEY)",
        "INSERT INTO indexed_join_left VALUES (1), (2), (3)",
        "INSERT INTO indexed_join_right VALUES (1), (2), (4)",
        "SELECT l.id, r.id FROM indexed_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE l.id = 2",
        "SELECT l.id, r.id FROM indexed_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE l.id = 3",
        "SELECT l.id, r.id FROM integer_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE l.id = 2",
        "SELECT l.id, r.id FROM indexed_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE r.id = 2",
        "SELECT l.id, r.id FROM indexed_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE r.id = 4",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    runtime.block_on(async {
        let sql = "SELECT l.id, r.id FROM indexed_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE l.id = $1";
        for key in [Some(1_i32), Some(2), Some(3), None] {
            let expected: Vec<(i32, i32)> = sqlx::query_as(sql)
                .bind(key)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, i32)> = sqlx::query_as(sql)
                .bind(key)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected, "indexed key={key:?}");
            let sql = "SELECT r.id, l.id FROM indexed_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE r.id = $1";
            let expected: Vec<(i32, i32)> = sqlx::query_as(sql)
                .bind(key)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(i32, i32)> = sqlx::query_as(sql)
                .bind(key)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected, "indexed right key={key:?}");
        }
        let sql = "SELECT l.id, r.id FROM integer_join_left l JOIN indexed_join_right r ON l.id = r.id WHERE l.id = $1";
        for key in [Some(2_i32), Some(3), None] {
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
            assert_eq!(actual, expected, "right-only index key={key:?}");
        }
    });
    for sql in [
        "CREATE TABLE right_filter_left (id integer PRIMARY KEY, name text)",
        "CREATE TABLE right_filter_right (id integer PRIMARY KEY, name text)",
        "CREATE TABLE right_filter_offkey_right (id integer, tag integer UNIQUE, name text)",
        "INSERT INTO right_filter_left VALUES (1, 'left')",
        "INSERT INTO right_filter_right VALUES (1, 'right')",
        "INSERT INTO right_filter_offkey_right VALUES (1, 9, 'off-key')",
        "SELECT r.name, l.name FROM right_filter_left l JOIN right_filter_right r ON l.id = r.id WHERE r.id = 1",
        "SELECT r.name, l.name, r.tag FROM right_filter_left l JOIN right_filter_offkey_right r ON l.id = r.id WHERE r.tag = 9",
        "SELECT r.name, l.name, r.tag FROM right_filter_left l JOIN right_filter_offkey_right r ON l.id = r.id WHERE r.tag = 10",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    runtime.block_on(async {
        let sql = "SELECT r.name, l.name, r.tag FROM right_filter_left l JOIN right_filter_offkey_right r ON l.id = r.id WHERE r.tag = $1";
        for tag in [Some(9_i32), None] {
            let expected: Vec<(String, String, i32)> = sqlx::query_as(sql)
                .bind(tag)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let actual: Vec<(String, String, i32)> = sqlx::query_as(sql)
                .bind(tag)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual, expected, "right off-key tag={tag:?}");
        }
    });
}

#[test]
fn compares_off_key_filters_with_unique_right_join_key() {
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
        "CREATE TABLE offkey_left (id integer, tag integer)",
        "CREATE TABLE offkey_right (id integer PRIMARY KEY, name text)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }
    let left = (1..=100)
        .map(|id| format!("({id}, {})", if id == 1 { 9 } else { id % 2 }))
        .chain(["(1, 9)".to_owned(), "(NULL, 9)".to_owned()])
        .collect::<Vec<_>>()
        .join(", ");
    let right = (1..=100)
        .map(|id| format!("({id}, 'name-{id}')"))
        .collect::<Vec<_>>()
        .join(", ");
    for sql in [
        format!("INSERT INTO offkey_left VALUES {left}"),
        format!("INSERT INTO offkey_right VALUES {right}"),
    ] {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &sql,
            RowOrder::Unordered,
        );
    }
    for tag in [9, 0, 7] {
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &format!(
                "SELECT l.id, r.name FROM offkey_left l JOIN offkey_right r ON l.id = r.id WHERE l.tag = {tag}"
            ),
            RowOrder::Unordered,
        );
    }
    runtime.block_on(async {
        let sql = "SELECT l.id, r.name FROM offkey_left l JOIN offkey_right r ON l.id = r.id WHERE l.tag = $1";
        for tag in [Some(9_i32), Some(0), None] {
            let mut expected: Vec<(i32, String)> = sqlx::query_as(sql)
                .bind(tag)
                .fetch_all(&mut postgres)
                .await
                .unwrap();
            let mut actual: Vec<(i32, String)> = sqlx::query_as(sql)
                .bind(tag)
                .fetch_all(&mut fake)
                .await
                .unwrap();
            expected.sort();
            actual.sort();
            assert_eq!(actual, expected, "tag={tag:?}");
        }
    });
}
