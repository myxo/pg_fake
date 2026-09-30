use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Executor, Row};
use std::{hint::black_box, time::Instant};

fn main() {
    let profile = std::env::args().any(|argument| argument == "--profile");
    let small = std::env::args().any(|argument| argument == "--small");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for size in [100, 1_000, 10_000] {
        if small && size == 10_000 {
            continue;
        }
        for groups in [10, size] {
            if profile && (size != 1_000 || groups != size) {
                continue;
            }
            let iterations = if profile {
                10_000
            } else {
                if size == 10_000 { 30 } else { 300 }
            };
            let insert = format!(
                "INSERT INTO t VALUES {}",
                (1..=size)
                    .map(|id| format!("({id}, {})", id % groups))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let sql = "SELECT bucket, count(*) FROM t GROUP BY bucket ORDER BY bucket";
            let db = Db::create();
            let mut session = db.create_session();
            session
                .execute("CREATE TABLE t (id integer, bucket integer)")
                .unwrap();
            session.execute(&insert).unwrap();
            let start = Instant::now();
            let prepared = session.prepare(sql).unwrap();
            let prepare = start.elapsed();
            let start = Instant::now();
            let first = session.query_prepared(&prepared, &[]).unwrap();
            assert_eq!(first.rows.len(), groups);
            println!(
                "rows={size} groups={groups} prepare={:.3} first={:.3} us",
                prepare.as_secs_f64() * 1e6,
                start.elapsed().as_secs_f64() * 1e6
            );
            for reuse in [false, true] {
                let start = Instant::now();
                for _ in 0..iterations {
                    if reuse {
                        black_box(session.query_prepared(&prepared, &[]).unwrap());
                    } else {
                        black_box(session.execute(sql).unwrap());
                    }
                }
                println!(
                    "native rows={size} groups={groups} prepared={reuse}: {:.3} us",
                    start.elapsed().as_secs_f64() * 1e6 / iterations as f64
                );
            }
            if profile {
                continue;
            }
            runtime.block_on(async {
                let mut connection = PgFakeConnection::new(Db::create());
                connection
                    .execute("CREATE TABLE t (id integer, bucket integer)")
                    .await
                    .unwrap();
                connection.execute(insert.as_str()).await.unwrap();
                let first = sqlx::query(sql).fetch_all(&mut connection).await.unwrap();
                assert_eq!(first.len(), groups);
                let start = Instant::now();
                for _ in 0..iterations {
                    let output = sqlx::query(sql).fetch_all(&mut connection).await.unwrap();
                    for row in output {
                        black_box((row.get::<i32, _>(0), row.get::<i64, _>(1)));
                    }
                }
                println!(
                    "SQLx rows={size} groups={groups} persistent=true: {:.3} us",
                    start.elapsed().as_secs_f64() * 1e6 / iterations as f64
                );
            });
        }
    }
}
