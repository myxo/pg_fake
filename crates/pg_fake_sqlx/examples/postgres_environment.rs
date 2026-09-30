use sqlx::{Connection, Row};
use sqlx_postgres::PgConnection;

fn main() {
    let database_url = dotenvy::var("PG_FAKE_DATABASE_URL")
        .expect("PG_FAKE_DATABASE_URL must identify the benchmark PostgreSQL server");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        let row = sqlx::query("SELECT version(), current_setting('server_version_num'), CASE WHEN inet_server_addr() IS NULL THEN 'Unix socket' ELSE 'TCP' END, current_setting('work_mem'), current_setting('shared_buffers'), current_setting('jit'), current_setting('max_parallel_workers_per_gather'), current_setting('fsync'), current_setting('synchronous_commit'), current_setting('TimeZone')")
            .fetch_one(&mut connection).await.unwrap();
        for (index, name) in ["version", "server_version_num", "transport", "work_mem", "shared_buffers", "jit", "max_parallel_workers_per_gather", "fsync", "synchronous_commit", "TimeZone"].into_iter().enumerate() {
            println!("{name}: {}", row.get::<String, _>(index));
        }
    });
}
