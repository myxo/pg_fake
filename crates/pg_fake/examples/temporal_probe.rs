use std::{hint::black_box, time::Instant};

use pg_fake::Db;

fn main() {
    for rows in [10, 100, 1_000] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE t (id integer PRIMARY KEY)")
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO t VALUES {}",
                (1..=rows)
                    .map(|id| format!("({id})"))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .unwrap();
        for ordered in [false, true] {
            let sql = format!(
                "SELECT id, to_char(date_trunc('minute', to_timestamp(id)), 'YYYY-MM-DD HH24:MI:SS'), floor(id::numeric / 7) FROM t{}",
                if ordered { " ORDER BY id" } else { "" }
            );
            let start = Instant::now();
            let prepared = session.prepare(&sql).unwrap();
            let prepare = start.elapsed();
            let start = Instant::now();
            let first = session.query_prepared(&prepared, &[]).unwrap();
            let execute = start.elapsed();
            assert_eq!(first.rows.len(), rows);
            println!(
                "rows={rows} ordered={ordered} prepare={:.3} first={:.3} us",
                prepare.as_secs_f64() * 1e6,
                execute.as_secs_f64() * 1e6
            );
            for reuse in [false, true] {
                let start = Instant::now();
                let iterations = 1_000;
                for _ in 0..iterations {
                    if reuse {
                        black_box(session.query_prepared(&prepared, &[]).unwrap());
                    } else {
                        black_box(session.execute(&sql).unwrap());
                    }
                }
                println!(
                    "rows={rows} ordered={ordered} prepared={reuse}: {:.3} us",
                    start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations)
                );
            }
        }
    }
}
