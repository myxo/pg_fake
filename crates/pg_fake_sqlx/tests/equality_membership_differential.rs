use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Connection, Executor, Statement, TypeInfo};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;
use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

fn assert_query(
    runtime: &tokio::runtime::Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
) {
    let expected = runtime.block_on(postgres.prepare(sql)).unwrap();
    let actual = runtime.block_on(fake.prepare(sql)).unwrap();
    assert_eq!(actual.columns().len(), expected.columns().len(), "{sql}");
    for (actual, expected) in actual.columns().iter().zip(expected.columns()) {
        assert_eq!(actual.name(), expected.name(), "{sql}");
        assert_eq!(
            actual.type_info().name(),
            expected.type_info().name(),
            "{sql}"
        );
        assert_eq!(
            actual.type_info().base.unwrap().map_to_oid(),
            expected.type_info().oid().unwrap().0,
            "{sql}"
        );
    }
    assert_statement(runtime, postgres, fake, sql, RowOrder::Ordered);
}

#[test]
fn compares_distinct_and_set_membership_results() {
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
        "SELECT DISTINCT x FROM (VALUES (1), (1), (2), (NULL), (NULL)) AS source(x) ORDER BY x NULLS FIRST",
        "SELECT DISTINCT ON (bucket) bucket, id FROM (VALUES (1, 10), (1, 20), (2, 30)) AS source(bucket, id) ORDER BY bucket, id DESC",
        "VALUES (1), (1), (2), (NULL), (NULL) UNION VALUES (2), (3), (NULL) ORDER BY 1 NULLS FIRST",
        "VALUES (1), (1), (2), (NULL), (NULL) INTERSECT VALUES (1), (NULL) ORDER BY 1 NULLS FIRST",
        "VALUES (1), (1), (2), (NULL), (NULL) INTERSECT ALL VALUES (1), (NULL), (NULL) ORDER BY 1 NULLS FIRST",
        "VALUES (1), (1), (2), (NULL), (NULL) EXCEPT VALUES (1), (NULL) ORDER BY 1 NULLS FIRST",
        "VALUES (1), (1), (2), (NULL), (NULL) EXCEPT ALL VALUES (1), (NULL) ORDER BY 1 NULLS FIRST",
        "SELECT 1 WHERE false UNION SELECT 2 WHERE false",
        "SELECT DISTINCT x FROM (VALUES (1.0::numeric), (1.00::numeric), (NULL::numeric)) AS source(x) ORDER BY x NULLS FIRST",
        "VALUES (1::numeric), (1.00::numeric), (NULL::numeric) UNION VALUES (2::numeric), (NULL::numeric) ORDER BY 1 NULLS FIRST",
        "SELECT DISTINCT x FROM (VALUES (1::smallint), (1::integer), (2::bigint)) AS source(x) ORDER BY x",
        "VALUES (1::smallint), (2::smallint) UNION VALUES (1::bigint), (3::bigint) ORDER BY 1",
        "SELECT DISTINCT x FROM (VALUES (0.0::double precision), (-0.0::double precision), ('NaN'::double precision), ('NaN'::double precision)) AS source(x) ORDER BY x",
        "SELECT DISTINCT x FROM (VALUES ('a'::char(3)), ('a '::char(3)), ('b'::char(3))) AS source(x) ORDER BY x",
        "VALUES (ARRAY[1,2]), (ARRAY[1,2]), (ARRAY[2,3]) UNION VALUES (ARRAY[2,3]) ORDER BY 1",
        "VALUES ('{\"a\":1}'::jsonb), ('{\"a\":1.0}'::jsonb) UNION VALUES ('{\"b\":2}'::jsonb) ORDER BY 1",
    ] {
        assert_query(&runtime, &mut postgres, &mut fake, sql);
    }
    let left = (0..40)
        .chain(10..25)
        .map(|value| format!("({value})"))
        .collect::<Vec<_>>()
        .join(",");
    let right = (20..60)
        .chain(30..45)
        .map(|value| format!("({value})"))
        .collect::<Vec<_>>()
        .join(",");
    let distinct = format!("SELECT DISTINCT id FROM (VALUES {left}) AS source(id) ORDER BY id");
    assert_query(&runtime, &mut postgres, &mut fake, &distinct);
    for operator in [
        "UNION",
        "INTERSECT",
        "INTERSECT ALL",
        "EXCEPT",
        "EXCEPT ALL",
    ] {
        let sql = format!("VALUES {left} {operator} VALUES {right} ORDER BY 1");
        assert_query(&runtime, &mut postgres, &mut fake, &sql);
    }
    let numeric = (0..25)
        .chain([1, 1, 2])
        .map(|value| format!("({value}::numeric)"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT DISTINCT id FROM (VALUES {numeric}) AS source(id) ORDER BY id");
    assert_query(&runtime, &mut postgres, &mut fake, &sql);
    let jsonb = (0..25)
        .map(|value| format!("('{{\"n\":{value}}}'::jsonb)"))
        .chain(["('{\"n\":1.0}'::jsonb)".to_owned()])
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT DISTINCT doc FROM (VALUES {jsonb}) AS source(doc) ORDER BY doc");
    assert_query(&runtime, &mut postgres, &mut fake, &sql);
    let bpchar = (0..25)
        .map(|value| format!("('{value}'::char(4))"))
        .chain(["('1 '::char(4))".to_owned()])
        .collect::<Vec<_>>()
        .join(",");
    let sql =
        format!("SELECT DISTINCT label FROM (VALUES {bpchar}) AS source(label) ORDER BY label");
    assert_query(&runtime, &mut postgres, &mut fake, &sql);
    let mixed_integers = (0..25)
        .map(|value| {
            if value % 2 == 0 {
                format!("({value}::smallint)")
            } else {
                format!("({value}::bigint)")
            }
        })
        .chain(["(1::integer)".to_owned()])
        .collect::<Vec<_>>()
        .join(",");
    let sql =
        format!("SELECT DISTINCT id FROM (VALUES {mixed_integers}) AS source(id) ORDER BY id");
    assert_query(&runtime, &mut postgres, &mut fake, &sql);
    let distinct_on = (0..25)
        .flat_map(|bucket| [format!("({bucket}, 1)"), format!("({bucket}, 2)")])
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT ON (bucket) bucket, id FROM (VALUES {distinct_on}) AS source(bucket, id) ORDER BY bucket, id DESC"
    );
    assert_query(&runtime, &mut postgres, &mut fake, &sql);
    for sql in [
        "SELECT '{}'::json UNION SELECT '{}'::json",
        "SELECT DISTINCT '{}'::json",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}
