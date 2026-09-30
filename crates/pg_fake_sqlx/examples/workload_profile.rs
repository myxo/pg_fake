use pg_fake_sqlx::Db;
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn main() {
    let workload = std::env::args().nth(1).expect("pass a workload name");
    let db = Db::create();
    let mut session = db.create_session();
    session.execute("CREATE TABLE t (id integer PRIMARY KEY, bucket integer, label text, payload jsonb, tags integer[], amount integer)").unwrap();
    session
        .execute(&format!(
            "INSERT INTO t VALUES {}",
            (1..=100)
                .map(|id| format!(
                    "({id}, {}, 'label {id}', '{{\"bucket\":{}}}', ARRAY[{}], {id})",
                    id % 10,
                    id % 10,
                    id % 10
                ))
                .collect::<Vec<_>>()
                .join(",")
        ))
        .unwrap();
    if workload == "view" {
        session.execute("CREATE VIEW inner_view AS SELECT id, label FROM t WHERE id >= 25; CREATE VIEW outer_view AS SELECT id, label FROM inner_view WHERE id <= 75").unwrap();
    }
    let sql = match workload.as_str() {
        "union" => "SELECT id FROM t UNION SELECT id FROM t WHERE id > 50 ORDER BY id",
        "distinct" => "SELECT DISTINCT bucket FROM t ORDER BY bucket",
        "temporal" => {
            "SELECT id, to_char(date_trunc('minute', to_timestamp(id)), 'YYYY-MM-DD HH24:MI:SS'), floor(id::numeric / 7) FROM t ORDER BY id"
        }
        "window" => {
            "SELECT id, lag(id, 1, -1) OVER ordered, lead(id, 1, -1) OVER ordered, first_value(id) OVER ordered, last_value(id) OVER ordered, nth_value(id, 2) OVER ordered FROM t WINDOW ordered AS (PARTITION BY bucket ORDER BY id) ORDER BY id"
        }
        "view" => "SELECT id, label FROM outer_view WHERE id = 50",
        "jsonb" => "SELECT id FROM t WHERE payload @> '{\"bucket\":3}'::jsonb ORDER BY id",
        "array" => "SELECT id FROM t WHERE tags @> ARRAY[3] ORDER BY id",
        "grouped" => {
            "SELECT bucket, string_agg(label, ',' ORDER BY id) FROM t GROUP BY bucket ORDER BY bucket"
        }
        "subquery" => {
            "SELECT outer_row.id, (SELECT max(inner_row.id) FROM t AS inner_row WHERE inner_row.bucket = outer_row.bucket) FROM t AS outer_row ORDER BY id"
        }
        "cte" => {
            "WITH x AS MATERIALIZED (SELECT id, bucket FROM t) SELECT id FROM x WHERE bucket = 3 ORDER BY id"
        }
        "write" => "UPDATE t SET amount = amount + 1 WHERE id = 1 RETURNING id, amount",
        "ddl" => {
            "CREATE TABLE transient (id integer); INSERT INTO transient VALUES (1); ALTER TABLE transient ADD COLUMN amount integer DEFAULT 7; DROP TABLE transient"
        }
        "lock" => "SELECT pg_advisory_lock(17); SELECT pg_advisory_unlock(17)",
        _ => panic!("unknown workload"),
    };
    let prepared = if matches!(workload.as_str(), "ddl" | "lock") {
        None
    } else {
        Some(session.prepare(sql).unwrap())
    };
    let mut execute = || {
        if let Some(prepared) = &prepared {
            black_box(session.execute_prepared_statement(prepared, &[]).unwrap());
        } else {
            black_box(session.execute(sql).unwrap());
        }
    };
    execute();
    println!("ready: {workload}");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        execute();
    }
}
