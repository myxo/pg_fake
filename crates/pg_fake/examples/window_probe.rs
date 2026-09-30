use std::{hint::black_box, time::Instant};

use pg_fake::Db;

fn main() {
    for rows in [100, 1_000] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE t (id integer PRIMARY KEY, bucket integer)")
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO t VALUES {}",
                (1..=rows)
                    .map(|id| format!("({id}, {})", id % 10))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .unwrap();
        for (case, sql) in [
            (
                "row_number",
                "SELECT id, row_number() OVER (ORDER BY id) FROM t ORDER BY id",
            ),
            (
                "five_windows",
                "SELECT id, lag(id, 1, -1) OVER ordered, lead(id, 1, -1) OVER ordered, first_value(id) OVER ordered, last_value(id) OVER ordered, nth_value(id, 2) OVER ordered FROM t WINDOW ordered AS (PARTITION BY bucket ORDER BY id) ORDER BY id",
            ),
            (
                "moving",
                "SELECT id, sum(id) OVER (PARTITION BY bucket ORDER BY id ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) FROM t ORDER BY id",
            ),
            (
                "grouped",
                "SELECT bucket, sum(id), count(*) FROM t GROUP BY bucket ORDER BY bucket",
            ),
        ] {
            let start = Instant::now();
            let prepared = session.prepare(sql).unwrap();
            let prepare = start.elapsed();
            let start = Instant::now();
            let first = session.query_prepared(&prepared, &[]).unwrap();
            let execute = start.elapsed();
            assert_eq!(first.rows.len(), if case == "grouped" { 10 } else { rows });
            println!(
                "rows={rows} case={case} prepare={:.3} first={:.3} us",
                prepare.as_secs_f64() * 1e6,
                execute.as_secs_f64() * 1e6
            );
            for reuse in [false, true] {
                let start = Instant::now();
                let iterations = 300;
                for _ in 0..iterations {
                    if reuse {
                        black_box(session.query_prepared(&prepared, &[]).unwrap());
                    } else {
                        black_box(session.execute(sql).unwrap());
                    }
                }
                println!(
                    "rows={rows} case={case} prepared={reuse}: {:.3} us",
                    start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations)
                );
            }
        }
    }
}
