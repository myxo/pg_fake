use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Executor, Row};
use std::{hint::black_box, time::Instant};

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for rows in [100, 1_000] {
            let mut connection = PgFakeConnection::new(Db::create());
            connection
                .execute("CREATE TABLE t (id integer PRIMARY KEY, bucket integer)")
                .await
                .unwrap();
            let insert = format!(
                "INSERT INTO t VALUES {}",
                (1..=rows)
                    .map(|id| format!("({id}, {})", id % 10))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            connection.execute(insert.as_str()).await.unwrap();
            for (case, sql) in [
                ("row_number", "SELECT id, row_number() OVER (ORDER BY id) FROM t ORDER BY id"),
                ("five_windows", "SELECT id, lag(id, 1, -1) OVER ordered, lead(id, 1, -1) OVER ordered, first_value(id) OVER ordered, last_value(id) OVER ordered, nth_value(id, 2) OVER ordered FROM t WINDOW ordered AS (PARTITION BY bucket ORDER BY id) ORDER BY id"),
                ("moving", "SELECT id, sum(id) OVER (PARTITION BY bucket ORDER BY id ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) FROM t ORDER BY id"),
            ] {
                let first = sqlx::query(sql).fetch_all(&mut connection).await.unwrap();
                assert_eq!(first.len(), rows);
                let started = Instant::now();
                for _ in 0..300 {
                    let output = sqlx::query(sql).fetch_all(&mut connection).await.unwrap();
                    for row in output {
                        black_box(row.get::<i32, _>(0));
                        if case == "five_windows" {
                            for column in 1..6 {
                                black_box(row.get::<Option<i32>, _>(column));
                            }
                        } else {
                            black_box(row.get::<i64, _>(1));
                        }
                    }
                }
                println!("SQLx rows={rows} case={case} persistent=true: {:.3} us", started.elapsed().as_secs_f64() * 1e6 / 300.0);
            }
        }
    });
}
