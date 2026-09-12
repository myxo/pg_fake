
use super::*;

#[test]
fn evicts_least_recently_used_statements_within_the_byte_limit() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut connection = PgFakeConnection::new(Db::create()).set_statement_cache_limit_bytes(16);
    runtime.block_on(async {
        sqlx::query("SELECT 1")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("SELECT 2")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("SELECT 1")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("SELECT 3")
            .execute(&mut connection)
            .await
            .unwrap();
    });

    let state = connection.state.lock().unwrap();
    assert_eq!(state.statement_cache_bytes, 16);
    assert_eq!(
        state
            .statements
            .keys()
            .map(|(sql, _)| sql.as_str())
            .collect::<Vec<_>>(),
        ["SELECT 1", "SELECT 3"]
    );
}
