use pg_fake::value::{BaseType, PgLsn, PgRegclass};
use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{Column, Executor, Row, Statement, TypeInfo};

#[tokio::test]
async fn round_trips_pg_lsn_and_regclass_codecs() {
    let mut connection = PgFakeConnection::new(Db::create());
    connection
        .execute("CREATE TABLE utility_values (position PG_LSN); INSERT INTO utility_values VALUES ('2B/1757980')")
        .await
        .unwrap();

    let row = sqlx::query("SELECT position FROM utility_values")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<PgLsn, _>(0), PgLsn(0x2b01757980));
    assert_eq!(row.columns()[0].type_info().name(), "PG_LSN");

    let bound = PgLsn(0x16ae7f8);
    let row = sqlx::query("SELECT $1::PG_LSN")
        .bind(bound)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<PgLsn, _>(0), bound);

    let relation = sqlx::query("SELECT 'utility_values'::regclass")
        .fetch_one(&mut connection)
        .await
        .unwrap()
        .get::<PgRegclass, _>(0);
    let row = sqlx::query("SELECT $1::regclass::text")
        .bind(relation)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>(0), "utility_values");
}

#[tokio::test]
async fn describes_catalog_metadata_through_sqlx() {
    let mut connection = PgFakeConnection::new(Db::create());
    connection
        .execute("CREATE TABLE described_item (id INTEGER NOT NULL, label VARCHAR(9))")
        .await
        .unwrap();
    let sql = "SELECT a.attname, a.atttypid, a.atttypmod \
               FROM pg_catalog.pg_attribute AS a \
               WHERE a.attrelid = 'described_item'::regclass ORDER BY a.attnum";
    let statement = connection.prepare(sql).await.unwrap();
    assert_eq!(statement.columns()[1].type_info().name(), "OID");
    let rows = sqlx::query(sql).fetch_all(&mut connection).await.unwrap();
    assert_eq!(rows[0].get::<u32, _>(1), BaseType::Int4.map_to_oid());
    assert_eq!(rows[1].get::<u32, _>(1), BaseType::Varchar.map_to_oid());
    assert_eq!(rows[1].get::<i32, _>(2), 13);
}
