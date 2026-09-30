use pg_fake::Db;
use std::{hint::black_box, time::Instant};

fn main() {
    for size in [1_000, 10_000] {
        for width in [0, 8] {
            let db = Db::create();
            let mut session = db.create_session();
            let extra = (0..width)
                .map(|index| format!(", payload{index} text DEFAULT '{}'", "x".repeat(256)))
                .collect::<String>();
            session
                .execute(&format!(
                    "CREATE TABLE t (id integer PRIMARY KEY, price integer{extra})"
                ))
                .unwrap();
            session
                .execute(&format!(
                    "INSERT INTO t (id, price) VALUES {}",
                    (1..=size)
                        .map(|id| format!("({id}, {})", id % 13))
                        .collect::<Vec<_>>()
                        .join(",")
                ))
                .unwrap();
            let prepared = session
                .prepare("SELECT id, price FROM t ORDER BY price DESC, id LIMIT 5 OFFSET 3")
                .unwrap();
            assert_eq!(
                session.query_prepared(&prepared, &[]).unwrap().rows.len(),
                5
            );
            let started = Instant::now();
            for _ in 0..100 {
                black_box(session.query_prepared(&prepared, &[]).unwrap());
            }
            println!(
                "rows={size} extra_columns={width}: {:.3} us",
                started.elapsed().as_secs_f64() * 1e6 / 100.0
            );
        }
    }
}
