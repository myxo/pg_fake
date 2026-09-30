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
fn matches_grouping_key_equality_and_fallback() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for (data_type, values) in [
        ("integer", "(1, 1), (2, 1), (3, 2), (4, NULL), (5, NULL)"),
        ("bigint", "(1, 1), (2, 1), (3, 2), (4, NULL), (5, NULL)"),
        ("numeric", "(1, 1), (2, 1.0), (3, 2), (4, 2.00), (5, NULL)"),
        (
            "double precision",
            "(1, 0), (2, '-0'), (3, 'NaN'), (4, 'NaN'), (5, NULL)",
        ),
        (
            "bpchar",
            "(1, 'x'), (2, 'x '), (3, 'y'), (4, NULL), (5, NULL)",
        ),
        (
            "text",
            "(1, 'x'), (2, 'x '), (3, 'x'), (4, NULL), (5, NULL)",
        ),
        (
            "jsonb",
            "(1, '1'), (2, '1.0'), (3, '{\"a\":1,\"b\":2}'), (4, '{\"b\":2,\"a\":1}'), (5, NULL)",
        ),
        (
            "integer[]",
            "(1, '{1,NULL}'), (2, '{1,NULL}'), (3, '{2}'), (4, NULL), (5, NULL)",
        ),
        (
            "date",
            "(1, '2024-01-01'), (2, '2024-01-01'), (3, '2024-01-02'), (4, NULL), (5, NULL)",
        ),
    ] {
        for sql in [
            format!("CREATE TABLE grouping_keys (id integer, k {data_type})"),
            format!("INSERT INTO grouping_keys VALUES {values}"),
            "SELECT count(*), min(id) FROM grouping_keys GROUP BY k ORDER BY min(id)".into(),
            "SELECT count(*), min(id) FROM grouping_keys GROUP BY k, id % 2 ORDER BY min(id)"
                .into(),
            "SELECT l.id, r.id FROM grouping_keys l JOIN grouping_keys r ON l.k = r.k ORDER BY l.id, r.id".into(),
            "DROP TABLE grouping_keys".into(),
        ] {
            assert_statement(&runtime, &mut postgres, &mut fake, &sql, RowOrder::Ordered);
        }
    }
    for sql in [
        "CREATE TABLE mixed_grouping (id integer, k bpchar, n numeric)",
        "INSERT INTO mixed_grouping VALUES (1, 'x', NULL), (2, 'x ', NULL), (3, 'x', 1), (4, 'x ', 1.0), (5, NULL, 2), (6, NULL, 2)",
        "SELECT count(*), min(id) FROM mixed_grouping GROUP BY k, n ORDER BY min(id)",
        "SELECT count(*), min(id) FROM mixed_grouping GROUP BY CASE WHEN id < 3 THEN 1::smallint ELSE 1::bigint END ORDER BY min(id)",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT count(*), min(id) FROM mixed_grouping GROUP BY k, n",
        RowOrder::Unordered,
    );
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT count(*) FROM mixed_grouping GROUP BY k::json",
        RowOrder::Unordered,
    );
    for (name, data_type) in [("hash_bpchar", "bpchar"), ("hash_jsonb", "jsonb")] {
        let create = format!("CREATE TABLE {name} (id integer, k {data_type}, n numeric)");
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &create,
            RowOrder::Ordered,
        );
        let values = (0..160)
            .flat_map(|group| {
                (0..2).map(move |copy| {
                    let key = if data_type == "bpchar" {
                        format!("g{group}{}", " ".repeat(2 - copy))
                    } else {
                        format!("{group}.{}", "0".repeat(copy + 1))
                    };
                    let numeric = if group < 140 {
                        "NULL"
                    } else if copy == 0 {
                        "1"
                    } else {
                        "1.0"
                    };
                    format!("({}, '{key}', {numeric})", group * 2 + copy)
                })
            })
            .collect::<Vec<_>>()
            .join(",");
        let insert =
            format!("INSERT INTO {name} VALUES {values}, (1000, NULL, NULL), (1001, NULL, NULL)");
        assert_statement(
            &runtime,
            &mut postgres,
            &mut fake,
            &insert,
            RowOrder::Ordered,
        );
        if data_type == "bpchar" {
            let query =
                format!("SELECT k, count(*), min(id) FROM {name} GROUP BY k ORDER BY min(id)");
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &query,
                RowOrder::Ordered,
            );
        }
        for keys in ["k", "k, n"] {
            let query =
                format!("SELECT count(*), min(id) FROM {name} GROUP BY {keys} ORDER BY min(id)");
            assert_statement(
                &runtime,
                &mut postgres,
                &mut fake,
                &query,
                RowOrder::Ordered,
            );
        }
    }
}
