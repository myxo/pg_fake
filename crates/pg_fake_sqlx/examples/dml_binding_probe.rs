use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Executor;
use std::{hint::black_box, time::Instant};

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for update in [false, true] {
        let sql = if update {
            "UPDATE t SET value = $1 WHERE id = 0"
        } else {
            "INSERT INTO t VALUES ($1, $1)"
        };
        let inline = |value| {
            if update {
                format!("UPDATE t SET value = {value} WHERE id = 0")
            } else {
                format!("INSERT INTO t VALUES ({value}, {value})")
            }
        };
        for bound in [false, true] {
            let db = Db::create();
            let mut session = db.create_session();
            session
                .execute("CREATE TABLE t (id integer PRIMARY KEY, value integer)")
                .unwrap();
            if update {
                session.execute("INSERT INTO t VALUES (0, 0)").unwrap();
            }
            let start = Instant::now();
            let prepared = session.prepare(sql).unwrap();
            let prepare = start.elapsed();
            let start = Instant::now();
            if bound {
                session
                    .execute_prepared(&prepared, &[pg_fake::value::Value::Int4(0)])
                    .unwrap();
            } else {
                session.execute(&inline(0)).unwrap();
            }
            println!(
                "native update={update} bound={bound} prepare={:.3} first={:.3} us",
                prepare.as_secs_f64() * 1e6,
                start.elapsed().as_secs_f64() * 1e6
            );
            let start = Instant::now();
            for value in 1..=1_000 {
                if bound {
                    black_box(
                        session
                            .execute_prepared(&prepared, &[pg_fake::value::Value::Int4(value)])
                            .unwrap(),
                    );
                } else {
                    black_box(session.execute(&inline(value)).unwrap());
                }
            }
            println!(
                "native update={update} bound={bound}: {:.3} us",
                start.elapsed().as_secs_f64() * 1e6 / 1_000.0
            );
            runtime.block_on(async {
                let mut connection = PgFakeConnection::new(Db::create());
                connection
                    .execute("CREATE TABLE t (id integer PRIMARY KEY, value integer)")
                    .await
                    .unwrap();
                if update {
                    connection
                        .execute("INSERT INTO t VALUES (0, 0)")
                        .await
                        .unwrap();
                }
                if bound {
                    sqlx::query(sql)
                        .bind(0i32)
                        .execute(&mut connection)
                        .await
                        .unwrap();
                } else {
                    sqlx::query(&inline(0))
                        .execute(&mut connection)
                        .await
                        .unwrap();
                }
                let start = Instant::now();
                for value in 1..=1_000i32 {
                    if bound {
                        black_box(
                            sqlx::query(sql)
                                .bind(value)
                                .execute(&mut connection)
                                .await
                                .unwrap(),
                        );
                    } else {
                        black_box(
                            sqlx::query(&inline(value))
                                .execute(&mut connection)
                                .await
                                .unwrap(),
                        );
                    }
                }
                println!(
                    "SQLx update={update} bound={bound} persistent=true: {:.3} us",
                    start.elapsed().as_secs_f64() * 1e6 / 1_000.0
                );
            });
        }
    }
}
