use std::{hint::black_box, time::Instant};

use pg_fake::{Db, value::Value};

fn main() {
    for settings in [0, 32] {
        for workload in ["insert", "update", "transaction_insert", "prepared_read"] {
            let db = Db::create();
            let mut session = db.create_session();
            session.execute("CREATE TABLE t (id integer PRIMARY KEY, n integer); INSERT INTO t VALUES (0, 0)").unwrap();
            for i in 0..settings {
                session
                    .execute(&format!(
                        "SELECT set_config('probe.setting{i}', 'value', false)"
                    ))
                    .unwrap();
            }
            let prepared = session.prepare("SELECT n FROM t WHERE id = $1").unwrap();
            let start = Instant::now();
            let iterations = 10_000;
            for i in 1..=iterations {
                match workload {
                    "insert" => {
                        black_box(
                            session
                                .execute(&format!("INSERT INTO t VALUES ({i}, 0)"))
                                .unwrap(),
                        );
                    }
                    "update" => {
                        black_box(
                            session
                                .execute("UPDATE t SET n = n + 1 WHERE id = 0")
                                .unwrap(),
                        );
                    }
                    "transaction_insert" => {
                        black_box(
                            session
                                .execute(&format!("BEGIN; INSERT INTO t VALUES ({i}, 0); COMMIT"))
                                .unwrap(),
                        );
                    }
                    "prepared_read" => {
                        black_box(
                            session
                                .query_prepared(&prepared, &[Value::Int4(0)])
                                .unwrap(),
                        );
                    }
                    _ => unreachable!(),
                }
            }
            println!(
                "{workload} settings={settings}: {:.3} us",
                start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations)
            );
        }
    }
}
