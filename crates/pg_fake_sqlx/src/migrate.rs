use std::time::{Duration, Instant};

use futures_core::future::BoxFuture;
use sqlx::{Connection, Executor, Row};
use sqlx_core::migrate::{AppliedMigration, Migrate, MigrateError, Migration};

use crate::PgFakeConnection;

impl Migrate for PgFakeConnection {
    fn ensure_migrations_table(&mut self) -> BoxFuture<'_, Result<(), MigrateError>> {
        Box::pin(async move {
            self.execute(
                "CREATE TABLE IF NOT EXISTS _sqlx_migrations (\
                 version BIGINT PRIMARY KEY, \
                 description TEXT NOT NULL, \
                 installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), \
                 success BOOLEAN NOT NULL, \
                 checksum BYTEA NOT NULL, \
                 execution_time BIGINT NOT NULL)",
            )
            .await?;
            Ok(())
        })
    }

    fn dirty_version(&mut self) -> BoxFuture<'_, Result<Option<i64>, MigrateError>> {
        Box::pin(async move {
            let row = sqlx::query(
                "SELECT version FROM _sqlx_migrations \
                 WHERE success = false ORDER BY version LIMIT 1",
            )
            .fetch_optional(self)
            .await?;
            Ok(row.map(|row| row.get(0)))
        })
    }

    fn list_applied_migrations(
        &mut self,
    ) -> BoxFuture<'_, Result<Vec<AppliedMigration>, MigrateError>> {
        Box::pin(async move {
            let rows =
                sqlx::query("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
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

    fn apply<'e: 'm, 'm>(
        &'e mut self,
        migration: &'m Migration,
    ) -> BoxFuture<'m, Result<Duration, MigrateError>> {
        Box::pin(async move {
            let start = Instant::now();
            if migration.no_tx {
                execute_migration(self, migration).await?;
            } else {
                let mut transaction = self.begin().await?;
                execute_migration(&mut transaction, migration).await?;
                transaction.commit().await?;
            }
            let elapsed = start.elapsed();
            #[allow(clippy::cast_possible_truncation)]
            sqlx::query("UPDATE _sqlx_migrations SET execution_time = $1 WHERE version = $2")
                .bind(elapsed.as_nanos() as i64)
                .bind(migration.version)
                .execute(self)
                .await?;
            Ok(elapsed)
        })
    }

    fn revert<'e: 'm, 'm>(
        &'e mut self,
        migration: &'m Migration,
    ) -> BoxFuture<'m, Result<Duration, MigrateError>> {
        Box::pin(async move {
            let start = Instant::now();
            if migration.no_tx {
                revert_migration(self, migration).await?;
            } else {
                let mut transaction = self.begin().await?;
                revert_migration(&mut transaction, migration).await?;
                transaction.commit().await?;
            }
            Ok(start.elapsed())
        })
    }
}

async fn execute_migration(
    connection: &mut PgFakeConnection,
    migration: &Migration,
) -> Result<(), MigrateError> {
    connection
        .execute(&*migration.sql)
        .await
        .map_err(|error| MigrateError::ExecuteMigration(error, migration.version))?;
    insert_migration(connection, migration).await
}

async fn insert_migration(
    connection: &mut PgFakeConnection,
    migration: &Migration,
) -> Result<(), MigrateError> {
    sqlx::query(
        "INSERT INTO _sqlx_migrations \
         (version, description, success, checksum, execution_time) \
         VALUES ($1, $2, TRUE, $3, -1)",
    )
    .bind(migration.version)
    .bind(migration.description.as_ref())
    .bind(migration.checksum.as_ref())
    .execute(connection)
    .await?;
    Ok(())
}

async fn revert_migration(
    connection: &mut PgFakeConnection,
    migration: &Migration,
) -> Result<(), MigrateError> {
    connection
        .execute(&*migration.sql)
        .await
        .map_err(|error| MigrateError::ExecuteMigration(error, migration.version))?;
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = $1")
        .bind(migration.version)
        .execute(connection)
        .await?;
    Ok(())
}
