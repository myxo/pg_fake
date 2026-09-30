use pg_fake::Db;
use std::{hint::black_box, time::Instant};

fn main() {
    for size in [10, 100, 1_000] {
        let db = Db::create();
        let mut session = db.create_session();
        session
            .execute("CREATE TABLE t (id integer PRIMARY KEY, price integer, label text)")
            .unwrap();
        session
            .execute(&format!(
                "INSERT INTO t VALUES {}",
                (1..=size)
                    .map(|id| format!("({id}, {}, 'row {id}')", id % 13))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
            .unwrap();
        for page in [false, true] {
            let sql = format!(
                "SELECT id, price, label FROM t ORDER BY price DESC, id{}",
                if page { " LIMIT 5 OFFSET 3" } else { "" }
            );
            let started = Instant::now();
            let prepared = session.prepare(&sql).unwrap();
            let prepare = started.elapsed();
            let started = Instant::now();
            let first = session.query_prepared(&prepared, &[]).unwrap();
            let execute = started.elapsed();
            assert_eq!(first.rows.len(), if page { 5 } else { size });
            println!(
                "rows={size} page={page} prepare={:.3} first={:.3} us",
                prepare.as_secs_f64() * 1e6,
                execute.as_secs_f64() * 1e6
            );
            for reuse in [false, true] {
                let started = Instant::now();
                for _ in 0..2_000 {
                    if reuse {
                        black_box(session.query_prepared(&prepared, &[]).unwrap());
                    } else {
                        black_box(session.execute(&sql).unwrap());
                    }
                }
                println!(
                    "rows={size} page={page} prepared={reuse}: {:.3} us",
                    started.elapsed().as_secs_f64() * 1e6 / 2_000.0
                );
            }
        }
    }
}
