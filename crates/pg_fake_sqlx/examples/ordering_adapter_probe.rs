use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Executor, Row};
use std::{hint::black_box, time::Instant};

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for page in [false, true] {
            let mut connection = PgFakeConnection::new(Db::create());
            connection
                .execute("CREATE TABLE t (id integer PRIMARY KEY, price integer, label text)")
                .await
                .unwrap();
            let insert = format!(
                "INSERT INTO t VALUES {}",
                (1..=100)
                    .map(|id| format!("({id}, {}, 'row {id}')", id % 13))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            connection.execute(insert.as_str()).await.unwrap();
            let sql = format!(
                "SELECT id, price, label FROM t WHERE id > $1 ORDER BY price DESC, id{}",
                if page { " LIMIT 5 OFFSET 3" } else { "" }
            );
            let first = sqlx::query(&sql)
                .bind(50i32)
                .fetch_all(&mut connection)
                .await
                .unwrap();
            assert_eq!(first.len(), if page { 5 } else { 50 });
            let started = Instant::now();
            for _ in 0..5_000 {
                let rows = sqlx::query(&sql)
                    .bind(50i32)
                    .fetch_all(&mut connection)
                    .await
                    .unwrap();
                for row in rows {
                    black_box((
                        row.get::<i32, _>(0),
                        row.get::<i32, _>(1),
                        row.get::<String, _>(2),
                    ));
                }
            }
            println!(
                "SQLx rows=100 page={page} persistent=true: {:.3} us",
                started.elapsed().as_secs_f64() * 1e6 / 5_000.0
            );
        }
    });
}
