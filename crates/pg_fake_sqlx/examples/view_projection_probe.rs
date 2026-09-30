use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Executor;
use std::{hint::black_box, time::Instant};

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for rows in [10, 100, 1_000] {
        let setup = format!(
            "CREATE TABLE t (id integer, name text); INSERT INTO t VALUES {}; CREATE VIEW inner_view AS SELECT id, name FROM t WHERE id >= 2; CREATE VIEW outer_view AS SELECT id, name FROM inner_view WHERE id <= {rows}",
            (1..=rows)
                .map(|id| format!("({id}, 'name {id}')"))
                .collect::<Vec<_>>()
                .join(",")
        );
        let sql = "SELECT id, name FROM outer_view WHERE id = 5";
        let db = Db::create();
        let mut session = db.create_session();
        session.execute(&setup).unwrap();
        let start = Instant::now();
        let prepared = session.prepare(sql).unwrap();
        println!(
            "rows={rows} prepare={:.3} us",
            start.elapsed().as_secs_f64() * 1e6
        );
        assert_eq!(
            session.query_prepared(&prepared, &[]).unwrap().rows.len(),
            1
        );
        for reuse in [false, true] {
            let start = Instant::now();
            for _ in 0..1_000 {
                if reuse {
                    black_box(session.query_prepared(&prepared, &[]).unwrap());
                } else {
                    black_box(session.execute(sql).unwrap());
                }
            }
            println!(
                "rows={rows} prepared={reuse}: {:.3} us",
                start.elapsed().as_secs_f64() * 1e3
            );
        }
        runtime.block_on(async {
            let mut connection = PgFakeConnection::new(Db::create());
            connection.execute(setup.as_str()).await.unwrap();
            assert_eq!(
                sqlx::query(sql)
                    .fetch_all(&mut connection)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            let start = Instant::now();
            for _ in 0..1_000 {
                black_box(sqlx::query(sql).fetch_all(&mut connection).await.unwrap());
            }
            println!(
                "rows={rows} sqlx: {:.3} us",
                start.elapsed().as_secs_f64() * 1e3
            );
        });
    }
}
