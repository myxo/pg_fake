use std::{borrow::Cow, collections::BTreeMap, ops::Deref, str::FromStr};

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use pg_fake::{
    CatalogColumnInspection, CatalogConstraintInspection, CatalogFunctionInspection,
    CatalogIndexInspection, CatalogInspection, CatalogSequenceInspection, CatalogTableInspection,
    CatalogTriggerInspection, CatalogViewInspection,
};
use pg_fake_sqlx::{Db, PgFakeConnection, PgFakeDatabaseError};
use serde_json::json;
use sqlx::{Acquire, Column, Connection, Executor, Row, Statement as _, TypeInfo};
use sqlx_core::migrate::{
    Migrate, MigrateError, Migration, MigrationType, Migrator as SqlxMigrator,
};
use sqlx_postgres::{PgConnection, PgDatabaseError, types::PgInterval as PostgresInterval};
use uuid::Uuid;

mod common;
#[path = "common/differential.rs"]
mod differential;

struct Migrator {
    inner: SqlxMigrator,
}

impl Migrator {
    fn with_migrations(mut migrations: Vec<Migration>) -> Self {
        migrations.sort_by_key(|migration| migration.version);
        Self {
            inner: SqlxMigrator {
                migrations: Cow::Owned(migrations),
                ..SqlxMigrator::DEFAULT
            },
        }
    }

    fn set_ignore_missing(&mut self, ignore_missing: bool) {
        self.inner.set_ignore_missing(ignore_missing);
    }

    async fn run<'a, A>(&self, connection: A) -> Result<(), MigrateError>
    where
        A: Acquire<'a>,
        <A::Connection as Deref>::Target: Migrate,
    {
        self.inner.run(connection).await
    }

    async fn run_to<C>(&self, target: i64, connection: &mut C) -> Result<(), MigrateError>
    where
        C: Migrate,
    {
        self.run_direct(Some(target), connection, false).await
    }

    async fn run_direct<C>(
        &self,
        target: Option<i64>,
        connection: &mut C,
        skip: bool,
    ) -> Result<(), MigrateError>
    where
        C: Migrate,
    {
        assert!(!skip);
        let migrations = self
            .inner
            .migrations
            .iter()
            .filter(|migration| target.is_none_or(|target| migration.version <= target))
            .cloned()
            .collect();
        SqlxMigrator {
            migrations: Cow::Owned(migrations),
            ignore_missing: self.inner.ignore_missing,
            locking: self.inner.locking,
            no_tx: self.inner.no_tx,
        }
        .run_direct(connection)
        .await
    }

    async fn undo<'a, A>(&self, connection: A, target: i64) -> Result<(), MigrateError>
    where
        A: Acquire<'a>,
        <A::Connection as Deref>::Target: Migrate,
    {
        self.inner.undo(connection, target).await
    }
}

use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, fake_statement_outcome,
    postgres_statement_outcome, start_isolated_postgres_server,
};

struct Scenario {
    name: &'static str,
    migrations: &'static [(&'static str, &'static str)],
    observations: &'static [&'static [&'static str]],
}

const SCHEMA_EVOLUTION: Scenario = Scenario {
    name: "schema_evolution",
    migrations: &[
        (
            "create core",
            include_str!("migrations/schema_evolution/001_create_core.sql"),
        ),
        (
            "evolve catalog",
            include_str!("migrations/schema_evolution/002_evolve_catalog.sql"),
        ),
    ],
    observations: &[
        &[
            "SELECT * FROM public.accounts ORDER BY id",
            "SELECT * FROM public.entries ORDER BY id",
        ],
        &[
            "SELECT * FROM public.accounts ORDER BY id",
            "SELECT * FROM public.account_entries ORDER BY id",
            "SELECT * FROM public.active_entries ORDER BY entry_id",
            "SELECT nextval('public.entry_number_seq')",
        ],
    ],
};

const PROCEDURAL_TRIGGERS: Scenario = Scenario {
    name: "procedural_triggers",
    migrations: &[
        (
            "create triggers",
            include_str!("migrations/procedural_triggers/001_create_triggers.sql"),
        ),
        (
            "exercise triggers",
            include_str!("migrations/procedural_triggers/002_exercise_triggers.sql"),
        ),
        (
            "validate and retire",
            include_str!("migrations/procedural_triggers/003_validate_and_retire.sql"),
        ),
    ],
    observations: &[
        &["SELECT id, value, compatible, inserted_by_trigger FROM public.records ORDER BY id"],
        &[
            "SELECT id, value, compatible, inserted_by_trigger, updated_at > created_at FROM public.records ORDER BY id",
        ],
        &[
            "SELECT id, value, compatible, inserted_by_trigger, updated_at > created_at FROM public.records ORDER BY id",
        ],
    ],
};

const DATA_RECONCILIATION: Scenario = Scenario {
    name: "data_reconciliation",
    migrations: &[
        (
            "create sources",
            include_str!("migrations/data_reconciliation/001_create_sources.sql"),
        ),
        (
            "reconcile records",
            include_str!("migrations/data_reconciliation/002_reconcile_records.sql"),
        ),
        (
            "validate relationship",
            include_str!("migrations/data_reconciliation/003_validate_relationship.sql"),
        ),
    ],
    observations: &[
        &[
            "SELECT * FROM public.identities ORDER BY id",
            "SELECT * FROM public.imported_records ORDER BY id",
        ],
        &[
            "SELECT * FROM public.imported_records ORDER BY id",
            "SELECT * FROM public.reconciliation_log ORDER BY imported_id",
        ],
        &[
            "SELECT * FROM public.imported_records ORDER BY id",
            "SELECT * FROM public.reconciliation_log ORDER BY imported_id",
        ],
    ],
};

fn create_migrator(scenario: &Scenario) -> Migrator {
    let migrations = scenario
        .migrations
        .iter()
        .enumerate()
        .map(|(index, (description, sql))| {
            Migration::new(
                index as i64 + 1,
                Cow::Borrowed(*description),
                MigrationType::Simple,
                Cow::Borrowed(*sql),
                false,
            )
        })
        .collect();
    Migrator::with_migrations(migrations)
}

fn get_migration_sqlstate(error: MigrateError) -> String {
    let error = match error {
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => error,
        error => panic!("expected migration execution error, got {error}"),
    };
    error
        .as_database_error()
        .and_then(|error| error.code())
        .expect("migration database errors must expose SQLSTATE")
        .into_owned()
}

fn get_fake_migration_error(error: MigrateError) -> (String, String, Option<String>) {
    let error = match error {
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => error,
        error => panic!("expected migration execution error, got {error}"),
    };
    let database_error = error.as_database_error().unwrap();
    (
        database_error.code().unwrap().into_owned(),
        database_error.message().to_owned(),
        database_error
            .downcast_ref::<PgFakeDatabaseError>()
            .hint()
            .map(str::to_owned),
    )
}

fn get_postgres_migration_error(error: MigrateError) -> (String, String, Option<String>) {
    let error = match error {
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => error,
        error => panic!("expected migration execution error, got {error}"),
    };
    let database_error = error.as_database_error().unwrap();
    (
        database_error.code().unwrap().into_owned(),
        database_error.message().to_owned(),
        database_error
            .downcast_ref::<PgDatabaseError>()
            .hint()
            .map(str::to_owned),
    )
}

fn diagnose_fake_migration(
    runtime: &tokio::runtime::Runtime,
    scenario: &Scenario,
    version: i64,
) -> String {
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = create_migrator(scenario);
    runtime
        .block_on(migrator.run_direct(Some(version - 1), &mut fake, false))
        .expect("preceding migration prefix must succeed during diagnosis");
    runtime.block_on(fake.execute("BEGIN")).unwrap();
    let statements = pg_fake::parser::parse(scenario.migrations[version as usize - 1].1).unwrap();
    for (index, statement) in statements.into_iter().enumerate() {
        let sql = statement.to_string();
        if let Err(error) = runtime.block_on(sqlx::raw_sql(sql.as_str()).execute(&mut fake)) {
            return format!("statement {} `{sql}`: {error}", index + 1);
        }
    }
    "no individually failing statement".to_owned()
}

async fn diagnose_fake_migration_on_connection_async(
    connection: &mut PgFakeConnection,
    scenario: &str,
    version: i64,
    sql: &str,
) -> String {
    connection.execute("BEGIN").await.unwrap();
    let statements = pg_fake::parser::parse(sql).unwrap();
    for (index, statement) in statements.into_iter().enumerate() {
        let statement_sql = statement.to_string();
        if let Err(error) = sqlx::raw_sql(statement_sql.as_str())
            .execute(&mut *connection)
            .await
        {
            connection.execute("ROLLBACK").await.unwrap();
            return format!(
                "{scenario}/{version} statement {} `{statement_sql}`: {error}",
                index + 1
            );
        }
    }
    connection.execute("ROLLBACK").await.unwrap();
    format!("{scenario}/{version}: no individually failing statement")
}

fn diagnose_fake_migration_on_connection(
    runtime: &tokio::runtime::Runtime,
    connection: &mut PgFakeConnection,
    scenario: &str,
    version: i64,
    sql: &str,
) -> String {
    let diagnosis = runtime.block_on(diagnose_fake_migration_on_connection_async(
        connection, scenario, version, sql,
    ));
    assert!(
        diagnosis.contains(" statement "),
        "expected a statement-indexed diagnostic: {diagnosis}"
    );
    diagnosis
}

fn assert_migration_metadata(
    runtime: &tokio::runtime::Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
) {
    assert_statement(
        runtime,
        postgres,
        fake,
        "SELECT version, description, success, checksum FROM _sqlx_migrations ORDER BY version",
        RowOrder::Ordered,
    );
}

fn assert_query_metadata(
    runtime: &tokio::runtime::Runtime,
    postgres: &mut PgConnection,
    fake: &mut PgFakeConnection,
    sql: &str,
) {
    runtime
        .block_on(postgres.clear_cached_statements())
        .unwrap();
    runtime.block_on(fake.clear_cached_statements()).unwrap();
    let expected = runtime.block_on(postgres.prepare(sql)).unwrap();
    let actual = runtime.block_on(fake.prepare(sql)).unwrap();
    let expected = expected
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let actual = actual
        .columns()
        .iter()
        .map(|column| {
            (
                column.name().to_owned(),
                column.type_info().name().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "metadata for SQL: {sql}");
}

fn collect_postgres_bookkeeping(
    runtime: &tokio::runtime::Runtime,
    connection: &mut PgConnection,
) -> Vec<(i64, Vec<u8>, i64)> {
    runtime
        .block_on(
            sqlx::query(
                "SELECT version, checksum, execution_time FROM _sqlx_migrations ORDER BY version",
            )
            .fetch_all(connection),
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

fn collect_fake_bookkeeping(
    runtime: &tokio::runtime::Runtime,
    connection: &mut PgFakeConnection,
) -> Vec<(i64, Vec<u8>, i64)> {
    runtime
        .block_on(
            sqlx::query(
                "SELECT version, checksum, execution_time FROM _sqlx_migrations ORDER BY version",
            )
            .fetch_all(connection),
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

fn split_catalog_names(value: String) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split(',').map(str::to_owned).collect()
    }
}

fn collect_postgres_catalog(
    runtime: &tokio::runtime::Runtime,
    connection: &mut PgConnection,
) -> CatalogInspection {
    let mut tables = BTreeMap::<String, CatalogTableInspection>::new();
    let columns = runtime
        .block_on(
            sqlx::query(
                "SELECT c.relname, a.attname, t.typname, a.atttypmod, \
                 NOT a.attnotnull, pg_get_expr(d.adbin, d.adrelid) \
                 FROM pg_catalog.pg_class AS c \
                 JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
                 JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid \
                 JOIN pg_catalog.pg_type AS t ON t.oid = a.atttypid \
                 LEFT JOIN pg_catalog.pg_attrdef AS d \
                   ON d.adrelid = c.oid AND d.adnum = a.attnum \
                 WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') \
                   AND c.relname <> '_sqlx_migrations' \
                   AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY c.relname, a.attnum",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap();
    for row in columns {
        let table_name = row.get::<String, _>(0);
        let table = tables
            .entry(table_name.clone())
            .or_insert_with(|| CatalogTableInspection {
                schema: "public".into(),
                name: table_name,
                columns: Vec::new(),
                constraints: Vec::new(),
                indexes: Vec::new(),
                triggers: Vec::new(),
            });
        table.columns.push(CatalogColumnInspection {
            name: row.get(1),
            type_name: row.get(2),
            typmod: row.get(3),
            nullable: row.get(4),
            default: row.get(5),
        });
    }

    let constraints = runtime
        .block_on(
            sqlx::query(
                "SELECT table_class.relname, constraint_row.conname, \
                   CASE constraint_row.contype \
                     WHEN 'p' THEN 'PRIMARY KEY' WHEN 'u' THEN 'UNIQUE' \
                     WHEN 'c' THEN 'CHECK' WHEN 'f' THEN 'FOREIGN KEY' END, \
                   COALESCE((SELECT string_agg(attribute_row.attname, ',' ORDER BY key_row.ordinality) \
                     FROM unnest(constraint_row.conkey) WITH ORDINALITY AS key_row(attnum, ordinality) \
                     JOIN pg_catalog.pg_attribute AS attribute_row \
                       ON attribute_row.attrelid = constraint_row.conrelid \
                      AND attribute_row.attnum = key_row.attnum), ''), \
                   CASE WHEN constraint_row.confrelid = 0 THEN NULL \
                     ELSE foreign_namespace.nspname || '.' || foreign_class.relname END, \
                   COALESCE((SELECT string_agg(attribute_row.attname, ',' ORDER BY key_row.ordinality) \
                     FROM unnest(constraint_row.confkey) WITH ORDINALITY AS key_row(attnum, ordinality) \
                     JOIN pg_catalog.pg_attribute AS attribute_row \
                       ON attribute_row.attrelid = constraint_row.confrelid \
                      AND attribute_row.attnum = key_row.attnum), ''), \
                   CASE constraint_row.confupdtype WHEN 'a' THEN 'NoAction' \
                     WHEN 'r' THEN 'Restrict' WHEN 'c' THEN 'Cascade' \
                     WHEN 'n' THEN 'SetNull' WHEN 'd' THEN 'SetDefault' END, \
                   CASE constraint_row.confdeltype WHEN 'a' THEN 'NoAction' \
                     WHEN 'r' THEN 'Restrict' WHEN 'c' THEN 'Cascade' \
                     WHEN 'n' THEN 'SetNull' WHEN 'd' THEN 'SetDefault' END, \
                   constraint_row.convalidated, \
                   pg_get_expr(constraint_row.conbin, constraint_row.conrelid) \
                 FROM pg_catalog.pg_constraint AS constraint_row \
                 JOIN pg_catalog.pg_class AS table_class ON table_class.oid = constraint_row.conrelid \
                 JOIN pg_catalog.pg_namespace AS table_namespace ON table_namespace.oid = table_class.relnamespace \
                 LEFT JOIN pg_catalog.pg_class AS foreign_class ON foreign_class.oid = constraint_row.confrelid \
                 LEFT JOIN pg_catalog.pg_namespace AS foreign_namespace ON foreign_namespace.oid = foreign_class.relnamespace \
                 WHERE table_namespace.nspname = 'public' \
                   AND table_class.relname <> '_sqlx_migrations' \
                   AND constraint_row.contype IN ('p', 'u', 'c', 'f') \
                 ORDER BY table_class.relname, constraint_row.conname",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap();
    for row in constraints {
        let table = tables
            .get_mut(&row.get::<String, _>(0))
            .expect("constraint table must have columns");
        let kind = row.get::<String, _>(2);
        table.constraints.push(CatalogConstraintInspection {
            name: row.get(1),
            kind: kind.clone(),
            columns: split_catalog_names(row.get(3)),
            referenced_relation: row.get(4),
            referenced_columns: split_catalog_names(row.get(5)),
            on_update: (kind == "FOREIGN KEY").then(|| row.get(6)),
            on_delete: (kind == "FOREIGN KEY").then(|| row.get(7)),
            validated: row.get(8),
            predicate: row.get(9),
        });
    }

    let indexes = runtime
        .block_on(
            sqlx::query(
                "SELECT table_class.relname, index_class.relname, index_row.indisunique, \
                   COALESCE((SELECT string_agg(attribute_row.attname || ':' || \
                       CASE WHEN (index_row.indoption[key_row.ordinality::integer - 1] & 1) = 1 \
                            THEN 'desc' ELSE 'asc' END, ',' ORDER BY key_row.ordinality) \
                     FROM unnest(index_row.indkey) WITH ORDINALITY AS key_row(attnum, ordinality) \
                     JOIN pg_catalog.pg_attribute AS attribute_row \
                       ON attribute_row.attrelid = index_row.indrelid \
                      AND attribute_row.attnum = key_row.attnum \
                     WHERE key_row.ordinality <= index_row.indnkeyatts), ''), \
                   COALESCE((SELECT string_agg(attribute_row.attname, ',' ORDER BY key_row.ordinality) \
                     FROM unnest(index_row.indkey) WITH ORDINALITY AS key_row(attnum, ordinality) \
                     JOIN pg_catalog.pg_attribute AS attribute_row \
                       ON attribute_row.attrelid = index_row.indrelid \
                      AND attribute_row.attnum = key_row.attnum \
                     WHERE key_row.ordinality > index_row.indnkeyatts), ''), \
                   pg_get_expr(index_row.indpred, index_row.indrelid) \
                 FROM pg_catalog.pg_index AS index_row \
                 JOIN pg_catalog.pg_class AS table_class ON table_class.oid = index_row.indrelid \
                 JOIN pg_catalog.pg_class AS index_class ON index_class.oid = index_row.indexrelid \
                 JOIN pg_catalog.pg_namespace AS table_namespace ON table_namespace.oid = table_class.relnamespace \
                 WHERE table_namespace.nspname = 'public' \
                   AND table_class.relname <> '_sqlx_migrations' \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint AS constraint_row \
                     WHERE constraint_row.conindid = index_row.indexrelid) \
                 ORDER BY table_class.relname, index_class.relname",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap();
    for row in indexes {
        let keys = split_catalog_names(row.get(3))
            .into_iter()
            .map(|key| {
                let (name, direction) = key
                    .rsplit_once(':')
                    .expect("catalog index key includes its direction");
                (name.to_owned(), direction == "desc")
            })
            .collect();
        tables
            .get_mut(&row.get::<String, _>(0))
            .expect("index table must have columns")
            .indexes
            .push(CatalogIndexInspection {
                name: row.get(1),
                unique: row.get(2),
                keys,
                included_columns: split_catalog_names(row.get(4)),
                predicate: row.get(5),
            });
    }

    let triggers = runtime
        .block_on(
            sqlx::query(
                "SELECT table_class.relname, trigger_row.tgname, \
                   CASE WHEN (trigger_row.tgtype::integer & 2) <> 0 THEN 'BEFORE' \
                        WHEN (trigger_row.tgtype::integer & 64) <> 0 THEN 'INSTEAD OF' ELSE 'AFTER' END, \
                   concat_ws(',', \
                     CASE WHEN (trigger_row.tgtype::integer & 4) <> 0 THEN 'INSERT' END, \
                     CASE WHEN (trigger_row.tgtype::integer & 16) <> 0 THEN 'UPDATE' END, \
                     CASE WHEN (trigger_row.tgtype::integer & 8) <> 0 THEN 'DELETE' END, \
                     CASE WHEN (trigger_row.tgtype::integer & 32) <> 0 THEN 'TRUNCATE' END), \
                   CASE WHEN (trigger_row.tgtype::integer & 1) <> 0 \
                        THEN 'FOR EACH ROW' ELSE 'FOR EACH STATEMENT' END, \
                   function_namespace.nspname || '.' || function_row.proname \
                 FROM pg_catalog.pg_trigger AS trigger_row \
                 JOIN pg_catalog.pg_class AS table_class ON table_class.oid = trigger_row.tgrelid \
                 JOIN pg_catalog.pg_namespace AS table_namespace ON table_namespace.oid = table_class.relnamespace \
                 JOIN pg_catalog.pg_proc AS function_row ON function_row.oid = trigger_row.tgfoid \
                 JOIN pg_catalog.pg_namespace AS function_namespace ON function_namespace.oid = function_row.pronamespace \
                 WHERE table_namespace.nspname = 'public' AND NOT trigger_row.tgisinternal \
                 ORDER BY table_class.relname, trigger_row.tgname",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap();
    for row in triggers {
        tables
            .get_mut(&row.get::<String, _>(0))
            .expect("trigger table must have columns")
            .triggers
            .push(CatalogTriggerInspection {
                name: row.get(1),
                timing: row.get(2),
                events: split_catalog_names(row.get(3)),
                level: row.get(4),
                function: row.get(5),
            });
    }

    let mut sequences = runtime
        .block_on(
            sqlx::query(
                "SELECT sequence_class.relname, type_row.typname, sequence_row.seqincrement, \
                   sequence_row.seqmin, sequence_row.seqmax, sequence_row.seqstart, \
                   sequence_row.seqcycle, sequence_row.seqcache, \
                   CASE WHEN owner_class.oid IS NULL THEN NULL \
                        ELSE owner_namespace.nspname || '.' || owner_class.relname END, \
                   owner_attribute.attname \
                 FROM pg_catalog.pg_sequence AS sequence_row \
                 JOIN pg_catalog.pg_class AS sequence_class ON sequence_class.oid = sequence_row.seqrelid \
                 JOIN pg_catalog.pg_namespace AS sequence_namespace ON sequence_namespace.oid = sequence_class.relnamespace \
                 JOIN pg_catalog.pg_type AS type_row ON type_row.oid = sequence_row.seqtypid \
                 LEFT JOIN pg_catalog.pg_depend AS dependency \
                   ON dependency.classid = 'pg_catalog.pg_class'::regclass \
                  AND dependency.objid = sequence_class.oid AND dependency.deptype = 'a' \
                 LEFT JOIN pg_catalog.pg_class AS owner_class ON owner_class.oid = dependency.refobjid \
                 LEFT JOIN pg_catalog.pg_namespace AS owner_namespace ON owner_namespace.oid = owner_class.relnamespace \
                 LEFT JOIN pg_catalog.pg_attribute AS owner_attribute \
                   ON owner_attribute.attrelid = owner_class.oid \
                  AND owner_attribute.attnum = dependency.refobjsubid \
                 WHERE sequence_namespace.nspname = 'public' \
                 ORDER BY sequence_class.relname",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap()
        .into_iter()
        .map(|row| CatalogSequenceInspection {
            schema: "public".into(),
            name: row.get(0),
            type_name: row.get(1),
            increment: row.get(2),
            minimum: row.get(3),
            maximum: row.get(4),
            start: row.get(5),
            cycle: row.get(6),
            cache: row.get(7),
            owner: row
                .get::<Option<String>, _>(8)
                .zip(row.get::<Option<String>, _>(9)),
            last_value: 0,
            is_called: false,
        })
        .collect::<Vec<_>>();
    for sequence in &mut sequences {
        let row = runtime
            .block_on(
                sqlx::query(&format!(
                    "SELECT last_value, is_called FROM public.{}",
                    sequence.name
                ))
                .fetch_one(&mut *connection),
            )
            .unwrap();
        sequence.last_value = row.get(0);
        sequence.is_called = row.get(1);
    }

    let mut views = BTreeMap::<String, CatalogViewInspection>::new();
    let view_columns = runtime
        .block_on(
            sqlx::query(
                "SELECT view_class.relname, attribute_row.attname, type_row.typname, \
                   attribute_row.atttypmod, pg_get_viewdef(view_class.oid), \
                   obj_description(view_class.oid, 'pg_class') \
                 FROM pg_catalog.pg_class AS view_class \
                 JOIN pg_catalog.pg_namespace AS view_namespace ON view_namespace.oid = view_class.relnamespace \
                 JOIN pg_catalog.pg_attribute AS attribute_row ON attribute_row.attrelid = view_class.oid \
                 JOIN pg_catalog.pg_type AS type_row ON type_row.oid = attribute_row.atttypid \
                 WHERE view_namespace.nspname = 'public' AND view_class.relkind = 'v' \
                   AND attribute_row.attnum > 0 AND NOT attribute_row.attisdropped \
                 ORDER BY view_class.relname, attribute_row.attnum",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap();
    for row in view_columns {
        let view_name = row.get::<String, _>(0);
        views
            .entry(view_name.clone())
            .or_insert_with(|| CatalogViewInspection {
                schema: "public".into(),
                name: view_name,
                columns: Vec::new(),
                definition: row.get(4),
                comment: row.get(5),
                dependencies: Vec::new(),
            })
            .columns
            .push((row.get(1), row.get(2), row.get(3)));
    }
    let view_dependencies = runtime
        .block_on(
            sqlx::query(
                "SELECT DISTINCT view_class.relname, \
                   CASE referenced_class.relkind WHEN 'r' THEN 'table:' WHEN 'v' THEN 'view:' \
                        WHEN 'S' THEN 'sequence:' END || \
                   referenced_namespace.nspname || '.' || referenced_class.relname \
                 FROM pg_catalog.pg_class AS view_class \
                 JOIN pg_catalog.pg_namespace AS view_namespace ON view_namespace.oid = view_class.relnamespace \
                 JOIN pg_catalog.pg_rewrite AS rewrite_row ON rewrite_row.ev_class = view_class.oid \
                 JOIN pg_catalog.pg_depend AS dependency \
                   ON dependency.classid = 'pg_catalog.pg_rewrite'::regclass \
                  AND dependency.objid = rewrite_row.oid \
                  AND dependency.refclassid = 'pg_catalog.pg_class'::regclass \
                 JOIN pg_catalog.pg_class AS referenced_class ON referenced_class.oid = dependency.refobjid \
                 JOIN pg_catalog.pg_namespace AS referenced_namespace ON referenced_namespace.oid = referenced_class.relnamespace \
                 WHERE view_namespace.nspname = 'public' AND view_class.relkind = 'v' \
                   AND referenced_namespace.nspname = 'public' \
                   AND referenced_class.oid <> view_class.oid \
                 ORDER BY view_class.relname, 2",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap();
    for row in view_dependencies {
        views
            .get_mut(&row.get::<String, _>(0))
            .expect("view dependency owner must exist")
            .dependencies
            .push(row.get(1));
    }

    let functions = runtime
        .block_on(
            sqlx::query(
                "SELECT function_namespace.nspname, function_row.proname, \
                   function_row.pronargs::integer, return_type.typname, language_row.lanname \
                 FROM pg_catalog.pg_proc AS function_row \
                 JOIN pg_catalog.pg_namespace AS function_namespace ON function_namespace.oid = function_row.pronamespace \
                 JOIN pg_catalog.pg_type AS return_type ON return_type.oid = function_row.prorettype \
                 JOIN pg_catalog.pg_language AS language_row ON language_row.oid = function_row.prolang \
                 WHERE function_namespace.nspname = 'public' AND function_row.prokind = 'f' \
                 ORDER BY function_row.proname",
            )
            .fetch_all(&mut *connection),
        )
        .unwrap()
        .into_iter()
        .map(|row| CatalogFunctionInspection {
            schema: row.get(0),
            name: row.get(1),
            argument_count: usize::try_from(row.get::<i32, _>(2)).unwrap(),
            return_type: Some(row.get(3)),
            language: Some(row.get(4)),
        })
        .collect();

    CatalogInspection {
        tables: tables.into_values().collect(),
        sequences,
        views: views.into_values().collect(),
        functions,
    }
}

fn normalize_catalog_expression(expression: &str) -> String {
    let mut normalized = expression.to_ascii_lowercase().replace("public.", "");
    for cast in [
        "::regclass",
        "::jsonb",
        "::numeric",
        "::smallint",
        "::integer",
        "::bigint",
        "::text",
        "::boolean",
    ] {
        normalized = normalized.replace(cast, "");
    }
    normalized.retain(|character| !character.is_whitespace() && !matches!(character, '(' | ')'));
    normalized = normalized.replace("=anyarray[", "in");
    normalized.replace(']', "")
}

fn normalize_catalog(mut catalog: CatalogInspection) -> CatalogInspection {
    catalog
        .tables
        .retain(|table| table.name != "_sqlx_migrations");
    for table in &mut catalog.tables {
        for column in &mut table.columns {
            column.default = column.default.as_deref().map(normalize_catalog_expression);
        }
        for constraint in &mut table.constraints {
            if constraint.kind == "CHECK" {
                constraint.columns.clear();
            }
            constraint.predicate = constraint
                .predicate
                .as_deref()
                .map(normalize_catalog_expression);
        }
        for index in &mut table.indexes {
            index.predicate = index.predicate.as_deref().map(normalize_catalog_expression);
        }
        for trigger in &mut table.triggers {
            trigger.timing.make_ascii_uppercase();
            for event in &mut trigger.events {
                event.make_ascii_uppercase();
            }
            trigger.level.make_ascii_uppercase();
        }
    }
    for view in &mut catalog.views {
        assert!(
            !view.definition.trim().is_empty(),
            "stored view definition must be observable"
        );
        view.definition.clear();
        view.dependencies.sort();
        view.dependencies.dedup();
    }
    for function in &mut catalog.functions {
        function.return_type = function
            .return_type
            .as_ref()
            .map(|value| value.to_ascii_lowercase());
        function.language = function
            .language
            .as_ref()
            .map(|value| value.to_ascii_lowercase());
    }
    catalog
}

fn assert_catalog_semantics(
    runtime: &tokio::runtime::Runtime,
    postgres: &mut PgConnection,
    database: &Db,
    scenario: &Scenario,
    version: i64,
) {
    let expected = normalize_catalog(collect_postgres_catalog(runtime, postgres));
    let actual = normalize_catalog(database.inspect_catalog());
    assert_eq!(
        actual, expected,
        "semantic catalog mismatch after {}/{}",
        scenario.name, version
    );
}

fn apply_and_compare_scenario(scenario: &Scenario) {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let database = Db::create();
    let mut fake = PgFakeConnection::new(database.clone());
    let migrator = create_migrator(scenario);

    for (index, observations) in scenario.observations.iter().enumerate() {
        let version = index as i64 + 1;
        runtime
            .block_on(migrator.run_to(version, &mut postgres))
            .unwrap_or_else(|error| {
                panic!("PostgreSQL failed {}/{}: {error}", scenario.name, version)
            });
        runtime
            .block_on(migrator.run_to(version, &mut fake))
            .unwrap_or_else(|error| {
                let diagnosis = diagnose_fake_migration(&runtime, scenario, version);
                panic!(
                    "pg_fake failed {}/{}: {error}; first blocker: {diagnosis}",
                    scenario.name, version
                )
            });
        for sql in *observations {
            assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
            assert_query_metadata(&runtime, &mut postgres, &mut fake, sql);
        }
        assert_migration_metadata(&runtime, &mut postgres, &mut fake);
        assert_catalog_semantics(&runtime, &mut postgres, &database, scenario, version);
    }

    let stable_observations = scenario
        .observations
        .last()
        .expect("migration scenarios have observations")
        .iter()
        .copied()
        .filter(|sql| !sql.contains("nextval("))
        .collect::<Vec<_>>();
    let postgres_rows_before = stable_observations
        .iter()
        .map(|sql| postgres_statement_outcome(&runtime, &mut postgres, sql))
        .collect::<Vec<_>>();
    let fake_rows_before = stable_observations
        .iter()
        .map(|sql| fake_statement_outcome(&runtime, &mut fake, sql))
        .collect::<Vec<_>>();
    let postgres_catalog_before =
        normalize_catalog(collect_postgres_catalog(&runtime, &mut postgres));
    let fake_catalog_before = normalize_catalog(database.inspect_catalog());
    let postgres_before = collect_postgres_bookkeeping(&runtime, &mut postgres);
    let fake_before = collect_fake_bookkeeping(&runtime, &mut fake);
    runtime.block_on(migrator.run(&mut postgres)).unwrap();
    runtime.block_on(migrator.run(&mut fake)).unwrap();
    let postgres_rows_after = stable_observations
        .iter()
        .map(|sql| postgres_statement_outcome(&runtime, &mut postgres, sql))
        .collect::<Vec<_>>();
    let fake_rows_after = stable_observations
        .iter()
        .map(|sql| fake_statement_outcome(&runtime, &mut fake, sql))
        .collect::<Vec<_>>();
    let postgres_catalog_after =
        normalize_catalog(collect_postgres_catalog(&runtime, &mut postgres));
    let fake_catalog_after = normalize_catalog(database.inspect_catalog());
    let postgres_after = collect_postgres_bookkeeping(&runtime, &mut postgres);
    let fake_after = collect_fake_bookkeeping(&runtime, &mut fake);
    assert_eq!(postgres_rows_after, postgres_rows_before);
    assert_eq!(fake_rows_after, fake_rows_before);
    assert_eq!(postgres_catalog_after, postgres_catalog_before);
    assert_eq!(fake_catalog_after, fake_catalog_before);
    assert_eq!(postgres_after, postgres_before);
    assert_eq!(fake_after, fake_before);
}

#[test]
fn applies_schema_evolution_through_sqlx_migrations() {
    apply_and_compare_scenario(&SCHEMA_EVOLUTION);
}

#[test]
fn migrated_schema_round_trips_required_typed_parameters_and_results() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let migrator = create_migrator(&SCHEMA_EVOLUTION);
        migrator.run(&mut postgres).await.unwrap();
        migrator.run(&mut fake).await.unwrap();

        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000020").unwrap();
        let balance = BigDecimal::from_str("1234.50").unwrap();
        let opened_on = NaiveDate::from_ymd_opt(2025, 6, 7).unwrap();
        let updated_at = DateTime::parse_from_rfc3339("2025-06-07T08:09:10+03:00")
            .unwrap()
            .with_timezone(&Utc);
        let metadata = json!({"amount": {"currency": "EUR", "value": 1234.5}});
        let sql = "INSERT INTO public.accounts (id, code, original_name, active, priority, \
                   revision, balance, payload, opened_on, updated_at, retention, metadata, \
                   normalized_name) \
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                   RETURNING id, code, original_name, active, priority, revision, balance, \
                   payload, opened_on, updated_at, retention, metadata, normalized_name";

        let postgres_row = sqlx::query(sql)
            .bind(id)
            .bind("EUR")
            .bind("Typed account")
            .bind(false)
            .bind(7_i16)
            .bind(11_i32)
            .bind(balance.clone())
            .bind(vec![0_u8, 127, 255])
            .bind(opened_on)
            .bind(updated_at)
            .bind(PostgresInterval {
                months: 1,
                days: 2,
                microseconds: 3_000_000,
            })
            .bind(sqlx::types::Json(metadata.clone()))
            .bind("typed account")
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let fake_row = sqlx::query(sql)
            .bind(id)
            .bind("EUR")
            .bind("Typed account")
            .bind(false)
            .bind(7_i16)
            .bind(11_i32)
            .bind(balance.clone())
            .bind(vec![0_u8, 127, 255])
            .bind(opened_on)
            .bind(updated_at)
            .bind(pg_fake::value::PgInterval {
                months: 1,
                days: 2,
                micros: 3_000_000,
            })
            .bind(sqlx::types::Json(metadata.clone()))
            .bind("typed account")
            .fetch_one(&mut fake)
            .await
            .unwrap();

        assert_eq!(postgres_row.get::<Uuid, _>(0), fake_row.get::<Uuid, _>(0));
        assert_eq!(
            postgres_row.get::<String, _>(1),
            fake_row.get::<String, _>(1)
        );
        assert_eq!(
            postgres_row.get::<String, _>(2),
            fake_row.get::<String, _>(2)
        );
        assert_eq!(postgres_row.get::<bool, _>(3), fake_row.get::<bool, _>(3));
        assert_eq!(postgres_row.get::<i16, _>(4), fake_row.get::<i16, _>(4));
        assert_eq!(postgres_row.get::<i32, _>(5), fake_row.get::<i32, _>(5));
        assert_eq!(
            postgres_row.get::<BigDecimal, _>(6),
            fake_row.get::<BigDecimal, _>(6)
        );
        assert_eq!(
            postgres_row.get::<Vec<u8>, _>(7),
            fake_row.get::<Vec<u8>, _>(7)
        );
        assert_eq!(
            postgres_row.get::<NaiveDate, _>(8),
            fake_row.get::<NaiveDate, _>(8)
        );
        assert_eq!(
            postgres_row.get::<DateTime<Utc>, _>(9),
            fake_row.get::<DateTime<Utc>, _>(9)
        );
        let expected_interval = postgres_row.get::<PostgresInterval, _>(10);
        let actual_interval = fake_row.get::<pg_fake::value::PgInterval, _>(10);
        assert_eq!(expected_interval.months, actual_interval.months);
        assert_eq!(expected_interval.days, actual_interval.days);
        assert_eq!(expected_interval.microseconds, actual_interval.micros);
        assert_eq!(
            postgres_row.get::<sqlx::types::Json<serde_json::Value>, _>(11),
            fake_row.get::<sqlx::types::Json<serde_json::Value>, _>(11)
        );
        assert_eq!(
            postgres_row.get::<String, _>(12),
            fake_row.get::<String, _>(12)
        );

        for (target, expected_count) in [(id, 1), (Uuid::nil(), 0)] {
            let postgres_count =
                sqlx::query("UPDATE public.accounts SET revision = revision + 1 WHERE id = $1")
                    .bind(target)
                    .execute(&mut postgres)
                    .await
                    .unwrap()
                    .rows_affected();
            let fake_count =
                sqlx::query("UPDATE public.accounts SET revision = revision + 1 WHERE id = $1")
                    .bind(target)
                    .execute(&mut fake)
                    .await
                    .unwrap()
                    .rows_affected();
            assert_eq!(postgres_count, expected_count);
            assert_eq!(fake_count, postgres_count);
        }

        let entry_id = Uuid::parse_str("10000000-0000-0000-0000-000000000009").unwrap();
        let postgres_position: i64 = sqlx::query_scalar(
            "UPDATE public.account_entries SET imported_position = $1 WHERE id = $2 \
             RETURNING imported_position",
        )
        .bind(901_i64)
        .bind(entry_id)
        .fetch_one(&mut postgres)
        .await
        .unwrap();
        let fake_position: i64 = sqlx::query_scalar(
            "UPDATE public.account_entries SET imported_position = $1 WHERE id = $2 \
             RETURNING imported_position",
        )
        .bind(901_i64)
        .bind(entry_id)
        .fetch_one(&mut fake)
        .await
        .unwrap();
        assert_eq!(fake_position, postgres_position);
        assert_eq!(fake_position, 901);
    });
}

#[test]
fn sqlx_migrator_reverts_versions() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = Migrator::with_migrations(vec![
        Migration::new(
            1,
            Cow::Borrowed("reversible marker"),
            MigrationType::ReversibleUp,
            "CREATE TABLE public.reversible_marker (id INTEGER)".into(),
            false,
        ),
        Migration::new(
            1,
            Cow::Borrowed("reversible marker"),
            MigrationType::ReversibleDown,
            "DROP TABLE public.reversible_marker".into(),
            false,
        ),
    ]);

    runtime.block_on(migrator.run_to(1, &mut postgres)).unwrap();
    runtime.block_on(migrator.run_to(1, &mut fake)).unwrap();
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT * FROM public.reversible_marker",
        RowOrder::Ordered,
    );
    runtime.block_on(migrator.undo(&mut postgres, 0)).unwrap();
    runtime.block_on(migrator.undo(&mut fake, 0)).unwrap();
    assert_statement_allow_error(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT * FROM public.reversible_marker",
        RowOrder::Ordered,
    );

    assert_migration_metadata(&runtime, &mut postgres, &mut fake);
    let versions: i64 = runtime
        .block_on(sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations").fetch_one(&mut fake))
        .unwrap();
    assert_eq!(versions, 0);
}

#[test]
fn backfills_non_empty_legacy_schema_prefix() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = create_migrator(&SCHEMA_EVOLUTION);
    runtime
        .block_on(migrator.run_direct(Some(1), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(1), &mut fake, false))
        .unwrap();
    let legacy = "INSERT INTO public.entries VALUES \
                  ('10000000-0000-0000-0000-000000000004', \
                   '00000000-0000-0000-0000-000000000001', 30, 'legacy', true)";
    runtime
        .block_on(sqlx::raw_sql(legacy).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(legacy).execute(&mut fake))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(2), &mut postgres, false))
        .unwrap();
    if let Err(error) = runtime.block_on(migrator.run_direct(Some(2), &mut fake, false)) {
        let diagnosis = diagnose_fake_migration_on_connection(
            &runtime,
            &mut fake,
            SCHEMA_EVOLUTION.name,
            2,
            SCHEMA_EVOLUTION.migrations[1].1,
        );
        panic!(
            "pg_fake failed non-empty {}/2: {error}; first blocker: {diagnosis}",
            SCHEMA_EVOLUTION.name
        );
    }
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, imported_position, entry_number, label \
         FROM public.account_entries ORDER BY id",
        RowOrder::Ordered,
    );
}

#[test]
fn schema_evolution_catalog_behavior_matches_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = create_migrator(&SCHEMA_EVOLUTION);
    runtime
        .block_on(migrator.run_direct(None, &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(None, &mut fake, false))
        .unwrap();

    for sql in [
        "SELECT * FROM public.entries",
        "SELECT * FROM public.discarded_table",
        "SELECT * FROM public.discarded_view",
        "SELECT nextval('public.discarded_sequence')",
        "INSERT INTO public.account_entries \
         (id, account_id, imported_position, entry_number, label, active) VALUES \
         ('10000000-0000-0000-0000-000000000010', \
          '00000000-0000-0000-0000-000000000001', -1, 10, 'invalid check', false)",
        "INSERT INTO public.account_entries \
         (id, account_id, imported_position, entry_number, label, active) VALUES \
         ('10000000-0000-0000-0000-000000000011', \
          '00000000-0000-0000-0000-000000000099', 400, 11, 'invalid foreign key', false)",
        "INSERT INTO public.account_entries \
         (id, account_id, imported_position, entry_number, label, active) VALUES \
         ('10000000-0000-0000-0000-000000000012', \
          '00000000-0000-0000-0000-000000000001', 400, 1, 'duplicate active', true) \
         ON CONFLICT (entry_number) WHERE active DO NOTHING RETURNING id",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Unordered);
    }

    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "INSERT INTO public.account_entries \
         (id, account_id, imported_position, entry_number, label, active) VALUES \
         ('10000000-0000-0000-0000-000000000013', \
          '00000000-0000-0000-0000-000000000001', 400, 1, 'first', false) \
         RETURNING label",
        RowOrder::Ordered,
    );
}

#[test]
fn applies_procedural_triggers_through_sqlx_migrations() {
    apply_and_compare_scenario(&PROCEDURAL_TRIGGERS);
}

#[test]
fn applies_data_reconciliation_through_sqlx_migrations() {
    apply_and_compare_scenario(&DATA_RECONCILIATION);
}

#[test]
fn rolls_back_failed_sqlx_migration_catalog_and_rows() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let base = create_migrator(&PROCEDURAL_TRIGGERS);
        base.run_direct(Some(2), &mut postgres, false)
            .await
            .unwrap();
        base.run_direct(Some(2), &mut fake, false).await.unwrap();
        let failure_sql = "CREATE TABLE public.rolled_back_table (id BIGINT); \
                           INSERT INTO public.records (id, value, compatible) VALUES (1, 1, true)";
        let mut failing = Migrator::with_migrations(vec![Migration::new(
            99,
            Cow::Borrowed("atomic failure"),
            MigrationType::Simple,
            failure_sql.into(),
            false,
        )]);
        failing.set_ignore_missing(true);
        let expected = failing
            .run_direct(None, &mut postgres, false)
            .await
            .unwrap_err();
        let actual = failing
            .run_direct(None, &mut fake, false)
            .await
            .unwrap_err();
        let actual = get_migration_sqlstate(actual);
        let diagnosis = diagnose_fake_migration_on_connection_async(
            &mut fake,
            "atomic_failure",
            99,
            failure_sql,
        )
        .await;
        assert!(
            diagnosis.contains("atomic_failure/99 statement 2"),
            "expected the second statement to fail: {diagnosis}"
        );
        assert_eq!(
            actual,
            get_migration_sqlstate(expected),
            "first blocker: {diagnosis}"
        );
        let expected = postgres
            .execute("SELECT * FROM public.rolled_back_table")
            .await
            .unwrap_err();
        let actual = fake
            .execute("SELECT * FROM public.rolled_back_table")
            .await
            .unwrap_err();
        assert_eq!(
            actual.as_database_error().unwrap().code(),
            expected.as_database_error().unwrap().code()
        );
        let postgres_versions: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let fake_versions: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!((postgres_versions, fake_versions), (2, 2));
    });
}

#[test]
fn rejects_legacy_currency_and_reapplies_unchanged_migration() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = create_migrator(&DATA_RECONCILIATION);
    runtime
        .block_on(migrator.run_direct(Some(1), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(1), &mut fake, false))
        .unwrap();
    let legacy = "INSERT INTO public.imported_records VALUES \
                  (15, 'alpha', '{\"amount\":{\"currency\":\"EUR\",\"value\":\"7\"}}', \
                   NULL, NULL, 'pending')";
    runtime
        .block_on(sqlx::raw_sql(legacy).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(legacy).execute(&mut fake))
        .unwrap();
    let expected = runtime
        .block_on(migrator.run_direct(Some(2), &mut postgres, false))
        .unwrap_err();
    let actual = runtime
        .block_on(migrator.run_direct(Some(2), &mut fake, false))
        .unwrap_err();
    let actual = get_fake_migration_error(actual);
    let diagnosis = diagnose_fake_migration_on_connection(
        &runtime,
        &mut fake,
        DATA_RECONCILIATION.name,
        2,
        DATA_RECONCILIATION.migrations[1].1,
    );
    assert_eq!(
        actual,
        get_postgres_migration_error(expected),
        "first blocker: {diagnosis}"
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, match_state FROM public.imported_records ORDER BY id",
        RowOrder::Ordered,
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT count(*) FROM public.reconciliation_log",
        RowOrder::Ordered,
    );
    let repair = "UPDATE public.imported_records \
                  SET payload = '{\"amount\":{\"currency\":\"USD\",\"value\":\"7\"}}' \
                  WHERE id = 15";
    runtime
        .block_on(sqlx::raw_sql(repair).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(repair).execute(&mut fake))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(2), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(2), &mut fake, false))
        .unwrap();
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT * FROM public.imported_records ORDER BY id",
        RowOrder::Ordered,
    );
}

#[test]
fn rejects_invalid_foreign_key_and_reapplies_unchanged_migration() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = create_migrator(&DATA_RECONCILIATION);
    runtime
        .block_on(migrator.run_direct(Some(2), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(2), &mut fake, false))
        .unwrap();
    let invalid = "UPDATE public.imported_records SET identity_id = 999 WHERE id = 12";
    runtime
        .block_on(sqlx::raw_sql(invalid).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(invalid).execute(&mut fake))
        .unwrap();
    let expected = runtime
        .block_on(migrator.run_direct(Some(3), &mut postgres, false))
        .unwrap_err();
    let actual = runtime
        .block_on(migrator.run_direct(Some(3), &mut fake, false))
        .unwrap_err();
    let actual = get_migration_sqlstate(actual);
    let diagnosis = diagnose_fake_migration_on_connection(
        &runtime,
        &mut fake,
        DATA_RECONCILIATION.name,
        3,
        DATA_RECONCILIATION.migrations[2].1,
    );
    assert_eq!(
        actual,
        get_migration_sqlstate(expected),
        "first blocker: {diagnosis}"
    );
    let repair = "UPDATE public.imported_records SET identity_id = NULL WHERE id = 12";
    runtime
        .block_on(sqlx::raw_sql(repair).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(repair).execute(&mut fake))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(3), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(3), &mut fake, false))
        .unwrap();
}

#[test]
fn rejects_incompatible_trigger_rows_and_restores_catalog() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let migrator = create_migrator(&PROCEDURAL_TRIGGERS);
    runtime
        .block_on(migrator.run_direct(Some(2), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(2), &mut fake, false))
        .unwrap();
    let invalid = "UPDATE public.records SET compatible = false WHERE id = 2";
    runtime
        .block_on(sqlx::raw_sql(invalid).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(invalid).execute(&mut fake))
        .unwrap();
    let expected = runtime
        .block_on(migrator.run_direct(Some(3), &mut postgres, false))
        .unwrap_err();
    let actual = runtime
        .block_on(migrator.run_direct(Some(3), &mut fake, false))
        .unwrap_err();
    let actual = get_fake_migration_error(actual);
    let diagnosis = diagnose_fake_migration_on_connection(
        &runtime,
        &mut fake,
        PROCEDURAL_TRIGGERS.name,
        3,
        PROCEDURAL_TRIGGERS.migrations[2].1,
    );
    assert_eq!(
        actual,
        get_postgres_migration_error(expected),
        "first blocker: {diagnosis}"
    );
    let repair = "UPDATE public.records SET compatible = true WHERE id = 2";
    runtime
        .block_on(sqlx::raw_sql(repair).execute(&mut postgres))
        .unwrap();
    runtime
        .block_on(sqlx::raw_sql(repair).execute(&mut fake))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(3), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(3), &mut fake, false))
        .unwrap();
}

#[test]
fn rolls_back_reconciliation_after_table_lock_timeout() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut postgres_holder = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let db = Db::create();
    let mut fake = PgFakeConnection::new(db.clone());
    let mut fake_holder = PgFakeConnection::new(db);
    let migrator = create_migrator(&DATA_RECONCILIATION);
    runtime
        .block_on(migrator.run_direct(Some(1), &mut postgres, false))
        .unwrap();
    runtime
        .block_on(migrator.run_direct(Some(1), &mut fake, false))
        .unwrap();
    runtime
        .block_on(
            postgres_holder
                .execute("BEGIN; LOCK TABLE public.imported_records IN ACCESS EXCLUSIVE MODE"),
        )
        .unwrap();
    runtime
        .block_on(
            fake_holder
                .execute("BEGIN; LOCK TABLE public.imported_records IN ACCESS EXCLUSIVE MODE"),
        )
        .unwrap();
    let expected = runtime
        .block_on(migrator.run_direct(Some(2), &mut postgres, false))
        .unwrap_err();
    let actual = runtime
        .block_on(migrator.run_direct(Some(2), &mut fake, false))
        .unwrap_err();
    let actual = get_migration_sqlstate(actual);
    let diagnosis = diagnose_fake_migration_on_connection(
        &runtime,
        &mut fake,
        DATA_RECONCILIATION.name,
        2,
        DATA_RECONCILIATION.migrations[1].1,
    );
    assert_eq!(
        actual,
        get_migration_sqlstate(expected),
        "first blocker: {diagnosis}"
    );
    runtime
        .block_on(postgres_holder.execute("ROLLBACK"))
        .unwrap();
    runtime.block_on(fake_holder.execute("ROLLBACK")).unwrap();
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        "SELECT id, match_state FROM public.imported_records ORDER BY id",
        RowOrder::Ordered,
    );
    assert_migration_metadata(&runtime, &mut postgres, &mut fake);
}
