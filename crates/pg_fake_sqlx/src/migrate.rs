use std::time::{Duration, Instant};

use futures_core::future::BoxFuture;
use sqlx::{AssertSqlSafe, Connection, Executor, Row};
use sqlx_core::migrate::{AppliedMigration, Migrate, MigrateError, Migration};

use crate::PgFakeConnection;

impl Migrate for PgFakeConnection {
    fn create_schema_if_not_exists<'e>(
        &'e mut self,
        schema_name: &'e str,
    ) -> BoxFuture<'e, Result<(), MigrateError>> {
        Box::pin(async move {
            Err(MigrateError::CreateSchemasNotSupported(
                schema_name.to_owned(),
            ))
        })
    }

    fn ensure_migrations_table<'e>(
        &'e mut self,
        table_name: &'e str,
    ) -> BoxFuture<'e, Result<(), MigrateError>> {
        Box::pin(async move {
            self.execute(AssertSqlSafe(format!(
                "CREATE TABLE IF NOT EXISTS {table_name} (\
                 version BIGINT PRIMARY KEY, \
                 description TEXT NOT NULL, \
                 installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 success BOOLEAN NOT NULL, \
                 checksum BYTEA NOT NULL, \
                 execution_time BIGINT NOT NULL)"
            )))
            .await?;
            Ok(())
        })
    }

    fn dirty_version<'e>(
        &'e mut self,
        table_name: &'e str,
    ) -> BoxFuture<'e, Result<Option<i64>, MigrateError>> {
        Box::pin(async move {
            let row = sqlx::query(AssertSqlSafe(format!(
                "SELECT version FROM {table_name} \
                 WHERE success = false ORDER BY version LIMIT 1"
            )))
            .fetch_optional(self)
            .await?;
            Ok(row.map(|row| row.get(0)))
        })
    }

    fn list_applied_migrations<'e>(
        &'e mut self,
        table_name: &'e str,
    ) -> BoxFuture<'e, Result<Vec<AppliedMigration>, MigrateError>> {
        Box::pin(async move {
            let rows = sqlx::query(AssertSqlSafe(format!(
                "SELECT version, checksum FROM {table_name} ORDER BY version"
            )))
            .fetch_all(self)
            .await?;
            Ok(rows
                .into_iter()
                .map(|row| AppliedMigration {
                    version: row.get(0),
                    checksum: row.get::<Vec<u8>, _>(1).into(),
                })
                .collect())
        })
    }

    fn lock(&mut self) -> BoxFuture<'_, Result<(), MigrateError>> {
        Box::pin(async move {
            self.execute("SELECT pg_advisory_lock(0)").await?;
            Ok(())
        })
    }

    fn unlock(&mut self) -> BoxFuture<'_, Result<(), MigrateError>> {
        Box::pin(async move {
            self.execute("SELECT pg_advisory_unlock(0)").await?;
            Ok(())
        })
    }

    fn apply<'e>(
        &'e mut self,
        table_name: &'e str,
        migration: &'e Migration,
    ) -> BoxFuture<'e, Result<Duration, MigrateError>> {
        Box::pin(async move {
            let start = Instant::now();
            if migration.no_tx {
                execute_migration(self, table_name, migration).await?;
            } else {
                let mut transaction = self.begin().await?;
                execute_migration(&mut transaction, table_name, migration).await?;
                transaction.commit().await?;
            }
            let elapsed = start.elapsed();
            #[allow(clippy::cast_possible_truncation)]
            sqlx::query(AssertSqlSafe(format!(
                "UPDATE {table_name} SET execution_time = $1 WHERE version = $2"
            )))
            .bind(elapsed.as_nanos() as i64)
            .bind(migration.version)
            .execute(self)
            .await?;
            Ok(elapsed)
        })
    }

    fn revert<'e>(
        &'e mut self,
        table_name: &'e str,
        migration: &'e Migration,
    ) -> BoxFuture<'e, Result<Duration, MigrateError>> {
        Box::pin(async move {
            let start = Instant::now();
            if migration.no_tx {
                revert_migration(self, table_name, migration).await?;
            } else {
                let mut transaction = self.begin().await?;
                revert_migration(&mut transaction, table_name, migration).await?;
                transaction.commit().await?;
            }
            Ok(start.elapsed())
        })
    }

    fn skip<'e>(
        &'e mut self,
        table_name: &'e str,
        migration: &'e Migration,
    ) -> BoxFuture<'e, Result<(), MigrateError>> {
        Box::pin(async move { insert_migration(self, table_name, migration).await })
    }
}

async fn execute_migration(
    connection: &mut PgFakeConnection,
    table_name: &str,
    migration: &Migration,
) -> Result<(), MigrateError> {
    connection
        .execute(migration.sql.clone())
        .await
        .map_err(|error| MigrateError::ExecuteMigration(error, migration.version))?;
    insert_migration(connection, table_name, migration).await
}

async fn insert_migration(
    connection: &mut PgFakeConnection,
    table_name: &str,
    migration: &Migration,
) -> Result<(), MigrateError> {
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {table_name} \
         (version, description, success, checksum, execution_time) \
         VALUES ($1, $2, TRUE, $3, -1)"
    )))
    .bind(migration.version)
    .bind(migration.description.as_ref())
    .bind(migration.checksum.as_ref())
    .execute(connection)
    .await?;
    Ok(())
}

async fn revert_migration(
    connection: &mut PgFakeConnection,
    table_name: &str,
    migration: &Migration,
) -> Result<(), MigrateError> {
    connection
        .execute(migration.sql.clone())
        .await
        .map_err(|error| MigrateError::ExecuteMigration(error, migration.version))?;
    sqlx::query(AssertSqlSafe(format!(
        "DELETE FROM {table_name} WHERE version = $1"
    )))
    .bind(migration.version)
    .execute(connection)
    .await?;
    Ok(())
}
