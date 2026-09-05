use pg_fake_sqlx::{Db, PgFakeConnection, PgFakeDatabaseError};
use sqlx::{AssertSqlSafe, Column, Connection, Row, TypeInfo};
use sqlx_postgres::{PgConnection, PgDatabaseError};

mod common;

fn get_sqlstate(error: sqlx::Error) -> String {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .expect("database errors must expose SQLSTATE")
        .into_owned()
}

async fn execute_both(postgres: &mut PgConnection, fake: &mut PgFakeConnection, sql: &str) {
    let expected = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(&mut *postgres)
        .await
        .map(|result| result.rows_affected())
        .map_err(get_sqlstate);
    let actual = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(&mut *fake)
        .await
        .map(|result| result.rows_affected())
        .map_err(get_sqlstate);
    assert_eq!(actual, expected, "SQL: {sql}");
}

async fn query_rows_postgres(connection: &mut PgConnection, sql: &str) -> Vec<(i64, i64, String)> {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .fetch_all(connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

async fn query_rows_fake(connection: &mut PgFakeConnection, sql: &str) -> Vec<(i64, i64, String)> {
    sqlx::raw_sql(AssertSqlSafe(sql))
        .fetch_all(connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

async fn compare_raise_error(postgres: &mut PgConnection, fake: &mut PgFakeConnection, sql: &str) {
    let expected = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(postgres)
        .await
        .unwrap_err();
    let actual = sqlx::raw_sql(AssertSqlSafe(sql))
        .execute(fake)
        .await
        .unwrap_err();
    let expected = expected.as_database_error().unwrap();
    let actual = actual.as_database_error().unwrap();
    assert_eq!(actual.code(), expected.code());
    assert_eq!(actual.message(), expected.message());
    assert_eq!(
        actual.downcast_ref::<PgFakeDatabaseError>().hint(),
        expected.downcast_ref::<PgDatabaseError>().hint()
    );
}

#[test]
fn trigger_and_do_behavior_matches_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let suffix = std::process::id();
        let table = format!("procedural_items_{suffix}");
        let function = format!("procedural_normalize_{suffix}");
        sqlx::raw_sql(AssertSqlSafe(
            format!("DROP TABLE IF EXISTS {table} CASCADE").as_str(),
        ))
        .execute(&mut postgres)
        .await
        .unwrap();
        sqlx::raw_sql(AssertSqlSafe(
            format!("DROP FUNCTION IF EXISTS {function}()").as_str(),
        ))
        .execute(&mut postgres)
        .await
        .unwrap();

        for sql in [
            format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL, label TEXT)"),
            format!(r#"CREATE FUNCTION {function}() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.label IS NULL THEN
                        RETURN NULL;
                    ELSIF NEW.value IS NULL THEN
                        NEW.value := 1;
                    ELSE
                        NEW.value = NEW.value + 1;
                    END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER normalize BEFORE INSERT OR UPDATE ON {table} FOR EACH ROW EXECUTE FUNCTION {function}()"),
            format!("INSERT INTO {table} VALUES (1, NULL, 'one'), (2, 2, NULL), (3, 3, 'three')"),
            format!("UPDATE {table} SET value = value + 1"),
            format!(r#"DO $$
                DECLARE
                    affected BIGINT;
                    selected BIGINT;
                    message TEXT := 'do';
                BEGIN
                    INSERT INTO {table} VALUES (4, 4, message);
                    GET DIAGNOSTICS affected = ROW_COUNT;
                    selected := '1';
                    message = 'do';
                    SELECT affected, message INTO selected, message;
                    IF selected = 1 AND message IS NOT NULL THEN
                        UPDATE {table} SET label = 'updated' WHERE id = 4;
                    ELSIF selected IS NULL THEN
                        RAISE EXCEPTION 'missing %', selected USING HINT = 'unexpected';
                    ELSE
                        RAISE EXCEPTION 'bad %', selected USING HINT = 'unexpected';
                    END IF;
                END;
                $$"#),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }

        execute_both(
            &mut postgres,
            &mut fake,
            r#"DO $$ DECLARE x BIGINT := 9; selected BIGINT;
               BEGIN
                   SELECT source.z INTO selected FROM (SELECT x AS z) source;
                   IF selected <> 9 THEN RAISE EXCEPTION 'unexpected derived local'; END IF;
                   WITH source AS (SELECT x AS z) SELECT z INTO selected FROM source;
                   IF selected <> 9 THEN RAISE EXCEPTION 'unexpected CTE local'; END IF;
                   SELECT id AS x INTO selected
                       FROM (VALUES (2), (1)) source(id) ORDER BY x;
                   IF selected <> 1 THEN RAISE EXCEPTION 'unexpected alias ordering'; END IF;
               END; $$"#,
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!(
                r#"DO $$ DECLARE value BIGINT := 9; selected BIGINT;
                   BEGIN
                       WITH changed AS (
                           UPDATE {table} SET value = value + 1 RETURNING id
                       ) SELECT id INTO selected FROM changed;
                   END; $$"#
            ),
        )
        .await;

        let trigger_sequence = format!("procedural_trigger_sequence_{suffix}");
        let trigger_rows = format!("procedural_trigger_rows_{suffix}");
        let trigger_function = format!("procedural_trigger_allocate_{suffix}");
        for sql in [
            format!("CREATE SEQUENCE {trigger_sequence}"),
            format!("CREATE TABLE {trigger_rows} (id BIGINT DEFAULT nextval('{trigger_sequence}'), allocated BIGINT)"),
            format!(r#"CREATE FUNCTION {trigger_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.allocated := nextval('{trigger_sequence}'); RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER allocate_value BEFORE INSERT ON {trigger_rows} FOR EACH ROW EXECUTE FUNCTION {trigger_function}()"),
            format!("INSERT INTO {trigger_rows}(allocated) VALUES (0), (0)"),
            format!("WITH source AS (SELECT 3 AS id), inserted AS (INSERT INTO {trigger_rows}(id, allocated) SELECT id, 0 FROM source RETURNING id) SELECT * FROM inserted"),
            format!("WITH inserted AS (INSERT INTO {trigger_rows}(id, allocated) VALUES ((SELECT 4), 0) RETURNING id) SELECT * FROM inserted"),
            format!("WITH first_insert AS (INSERT INTO {trigger_rows}(id, allocated) VALUES (5, 0) RETURNING id), second_insert AS (INSERT INTO {trigger_rows}(id, allocated) VALUES (6, 0) RETURNING id) SELECT * FROM first_insert, second_insert"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, allocated, '' FROM {trigger_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('{trigger_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let query = format!("SELECT id, value, label FROM {table} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &query).await,
            query_rows_postgres(&mut postgres, &query).await
        );

        execute_both(&mut postgres, &mut fake, "BEGIN").await;
        let raise = format!(r#"DO $$
            DECLARE affected BIGINT := 7;
            BEGIN
                INSERT INTO {table} VALUES (5, 5, 'pending');
                RAISE EXCEPTION 'failed % %%', affected USING HINT = 'rolled back';
            END;
            $$"#);
        compare_raise_error(&mut postgres, &mut fake, &raise).await;
        execute_both(&mut postgres, &mut fake, "ROLLBACK").await;
        let query = format!("SELECT id, value, label FROM {table} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &query).await,
            query_rows_postgres(&mut postgres, &query).await
        );

        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP FUNCTION {function}()"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP FUNCTION {function}() CASCADE"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP TABLE {trigger_rows}"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP FUNCTION {trigger_function}()"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP SEQUENCE {trigger_sequence}"),
        )
        .await;
        execute_both(&mut postgres, &mut fake, &format!("DROP TABLE {table}")).await;
    });
}

#[test]
fn procedural_catalog_and_edge_behavior_matches_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let suffix = std::process::id();
        let source = format!("procedural_edge_source_{suffix}");
        let results = format!("procedural_edge_results_{suffix}");
        let sequence = format!("procedural_edge_sequence_{suffix}");
        let function = format!("procedural_edge_function_{suffix}");
        for sql in [
            format!("CREATE TABLE public.{source} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL, label TEXT)"),
            format!("CREATE TABLE public.{results} (value BIGINT, missing TEXT)"),
            format!("CREATE SEQUENCE public.{sequence}"),
            format!(r#"CREATE FUNCTION public.{function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER edge_trigger BEFORE INSERT OR UPDATE ON public.{source} FOR EACH ROW EXECUTE FUNCTION public.{function}()"),
            format!("INSERT INTO public.{source} VALUES (1, 1, 'one'), (2, 2, 'two')"),
            format!(r#"DO $$
                DECLARE text_value TEXT := '7'; number_value BIGINT := text_value; missing TEXT;
                BEGIN
                    number_value = number_value + 1;
                    SELECT nextval('public.{sequence}'), 'extra' INTO number_value FROM public.{source} ORDER BY id;
                    SELECT 9 INTO number_value, missing;
                    IF EXISTS (SELECT 1 FROM public.{source} WHERE id = 2)
                       AND number_value = 9 AND missing IS NULL THEN
                        INSERT INTO public.{results} VALUES (number_value, missing);
                    END IF;
                END;
                $$"#),
            r#"DO $$
                DECLARE first_time TEXT; second_time TEXT;
                BEGIN
                    SELECT statement_timestamp()::TEXT INTO first_time;
                    SELECT statement_timestamp()::TEXT INTO second_time;
                    IF first_time <> second_time THEN
                        RAISE EXCEPTION 'statement timestamp changed';
                    END IF;
                END;
                $$"#
                .to_owned(),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }

        let sql = format!("SELECT value, 0::BIGINT, COALESCE(missing, '') FROM public.{results}");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('public.{sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        for sql in [
            "DO $$ BEGIN SELECT 1; END; $$".to_owned(),
            format!("DO $$ BEGIN INSERT INTO public.{source} VALUES (3, 3, 'three') RETURNING id; END; $$"),
            "DO $$ BEGIN IF FALSE THEN RAISE EXCEPTION '%'; END IF; END; $$".to_owned(),
            format!(r#"DO $$ DECLARE id BIGINT := 9; selected BIGINT;
                BEGIN SELECT id INTO selected FROM public.{source}; END; $$"#),
            "DO LANGUAGE missing_procedural_language_84723 $$ BEGIN END; $$".to_owned(),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }

        execute_both(&mut postgres, &mut fake, "BEGIN").await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!(r#"CREATE OR REPLACE FUNCTION public.{function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value + 10; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
        )
        .await;
        execute_both(&mut postgres, &mut fake, "ROLLBACK").await;

        let sql = format!(
            "INSERT INTO public.{source} VALUES ($1, $2, $3) RETURNING id, value, label"
        );
        let expected = sqlx::query(AssertSqlSafe(sql.clone()))
            .bind(4_i64)
            .bind(4_i64)
            .bind("four")
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual = sqlx::query(AssertSqlSafe(sql))
            .bind(4_i64)
            .bind(4_i64)
            .bind("four")
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!(
            actual
                .columns()
                .iter()
                .map(|column| (column.name(), column.type_info().name()))
                .collect::<Vec<_>>(),
            expected
                .columns()
                .iter()
                .map(|column| (column.name(), column.type_info().name()))
                .collect::<Vec<_>>()
        );
        assert_eq!(actual.get::<i64, _>(0), expected.get::<i64, _>(0));
        assert_eq!(actual.get::<i64, _>(1), expected.get::<i64, _>(1));
        assert_eq!(actual.get::<String, _>(2), expected.get::<String, _>(2));

        execute_both(
            &mut postgres,
            &mut fake,
            &format!(r#"CREATE OR REPLACE FUNCTION public.{function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value + 10; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
        )
        .await;
        let sql = format!(
            "INSERT INTO public.{source} VALUES ($1, $2, $3) RETURNING id, value, label"
        );
        let expected = sqlx::query(AssertSqlSafe(sql.clone()))
            .bind(5_i64)
            .bind(5_i64)
            .bind("five")
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual = sqlx::query(AssertSqlSafe(sql))
            .bind(5_i64)
            .bind(5_i64)
            .bind("five")
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!(
            actual
                .columns()
                .iter()
                .map(|column| (column.name(), column.type_info().name()))
                .collect::<Vec<_>>(),
            expected
                .columns()
                .iter()
                .map(|column| (column.name(), column.type_info().name()))
                .collect::<Vec<_>>()
        );
        assert_eq!(actual.get::<i64, _>(0), expected.get::<i64, _>(0));
        assert_eq!(actual.get::<i64, _>(1), expected.get::<i64, _>(1));
        assert_eq!(actual.get::<String, _>(2), expected.get::<String, _>(2));

        let parents = format!("procedural_edge_parents_{suffix}");
        let children = format!("procedural_edge_children_{suffix}");
        let cascade_function = format!("procedural_edge_cascade_{suffix}");
        for sql in [
            format!("CREATE TABLE public.{parents} (id BIGINT PRIMARY KEY)"),
            format!("CREATE TABLE public.{children} (id BIGINT PRIMARY KEY, parent_id BIGINT REFERENCES public.{parents}(id) ON UPDATE CASCADE ON DELETE SET NULL, changes BIGINT NOT NULL)"),
            format!(r#"CREATE FUNCTION public.{cascade_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.changes := NEW.changes + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER cascade_change BEFORE UPDATE ON public.{children} FOR EACH ROW EXECUTE FUNCTION public.{cascade_function}()"),
            format!("INSERT INTO public.{parents} VALUES (1), (3)"),
            format!("INSERT INTO public.{children} VALUES (1, 1, 0), (2, 3, 0)"),
            format!("UPDATE public.{parents} SET id = 2 WHERE id = 1"),
            format!("DELETE FROM public.{parents} WHERE id = 3"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, COALESCE(parent_id, -1), changes::TEXT FROM public.{children} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        execute_both(&mut postgres, &mut fake, "BEGIN").await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP FUNCTION public.{function}() CASCADE"),
        )
        .await;
        execute_both(&mut postgres, &mut fake, "ROLLBACK").await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("ALTER TRIGGER edge_trigger ON public.{source} RENAME TO renamed_edge_trigger"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP TRIGGER renamed_edge_trigger ON public.{source}"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP FUNCTION public.{function}()"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP TABLE public.{children}, public.{parents}"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP FUNCTION public.{cascade_function}()"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP TABLE public.{results}, public.{source}"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP SEQUENCE public.{sequence}"),
        )
        .await;
    });
}

#[test]
fn trigger_catalog_changes_and_errors_match_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let suffix = std::process::id();
        let table = format!("procedural_catalog_changes_{suffix}");
        let function = format!("procedural_catalog_function_{suffix}");
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL)"),
        )
        .await;

        let insert = format!("INSERT INTO {table} VALUES ($1, $2) RETURNING id, value");
        for (id, value, expected_value) in [(1_i64, 2_i64, 2_i64)] {
            let expected = sqlx::query(AssertSqlSafe(insert.clone()))
                .bind(id)
                .bind(value)
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual = sqlx::query(AssertSqlSafe(insert.clone()))
                .bind(id)
                .bind(value)
                .fetch_one(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual.get::<i64, _>(1), expected_value);
            assert_eq!(actual.get::<i64, _>(1), expected.get::<i64, _>(1));
            assert_eq!(
                actual
                    .columns()
                    .iter()
                    .map(|column| (column.name(), column.type_info().name()))
                    .collect::<Vec<_>>(),
                expected
                    .columns()
                    .iter()
                    .map(|column| (column.name(), column.type_info().name()))
                    .collect::<Vec<_>>()
            );
        }

        for sql in [
            format!(r#"CREATE FUNCTION {function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER active_trigger BEFORE INSERT OR UPDATE ON {table} FOR EACH ROW EXECUTE FUNCTION {function}()"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        for (id, expected_value) in [(2_i64, 3_i64)] {
            let expected = sqlx::query(AssertSqlSafe(insert.clone()))
                .bind(id)
                .bind(2_i64)
                .fetch_one(&mut postgres)
                .await
                .unwrap();
            let actual = sqlx::query(AssertSqlSafe(insert.clone()))
                .bind(id)
                .bind(2_i64)
                .fetch_one(&mut fake)
                .await
                .unwrap();
            assert_eq!(actual.get::<i64, _>(1), expected_value);
            assert_eq!(actual.get::<i64, _>(1), expected.get::<i64, _>(1));
        }
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("ALTER TRIGGER active_trigger ON {table} RENAME TO renamed_trigger"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("DROP TRIGGER renamed_trigger ON {table}"),
        )
        .await;
        let expected = sqlx::query(AssertSqlSafe(insert.clone()))
            .bind(3_i64)
            .bind(2_i64)
            .fetch_one(&mut postgres)
            .await
            .unwrap();
        let actual = sqlx::query(AssertSqlSafe(insert.clone()))
            .bind(3_i64)
            .bind(2_i64)
            .fetch_one(&mut fake)
            .await
            .unwrap();
        assert_eq!(actual.get::<i64, _>(1), expected.get::<i64, _>(1));

        let ordering_table = format!("procedural_ordering_{suffix}");
        let add_function = format!("procedural_add_{suffix}");
        let double_function = format!("procedural_double_{suffix}");
        for sql in [
            format!("CREATE TABLE {ordering_table} (id BIGINT PRIMARY KEY, value BIGINT)"),
            format!(r#"CREATE FUNCTION {add_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!(r#"CREATE FUNCTION {double_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value * 2; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER a_add BEFORE INSERT ON {ordering_table} FOR EACH ROW EXECUTE FUNCTION {add_function}()"),
            format!("CREATE TRIGGER z_double BEFORE INSERT ON {ordering_table} FOR EACH ROW EXECUTE FUNCTION {double_function}()"),
            format!("INSERT INTO {ordering_table} VALUES (1, 2)"),
            format!("ALTER TRIGGER a_add ON {ordering_table} RENAME TO zz_add"),
            format!("INSERT INTO {ordering_table} VALUES (2, 2)"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let ordering_query = format!("SELECT id, value, '' FROM {ordering_table} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &ordering_query).await,
            query_rows_postgres(&mut postgres, &ordering_query).await
        );

        let skip_function = format!("procedural_skip_update_{suffix}");
        execute_both(
            &mut postgres,
            &mut fake,
            &format!(r#"CREATE FUNCTION {skip_function}() RETURNS TRIGGER AS $$
                BEGIN IF NEW.value < 0 THEN RETURN NULL; END IF; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("CREATE TRIGGER skip_update BEFORE UPDATE ON {table} FOR EACH ROW EXECUTE FUNCTION {skip_function}()"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("UPDATE {table} SET value = -1 WHERE id = 1"),
        )
        .await;
        let unchanged = format!("SELECT id, value, '' FROM {table} WHERE id = 1");
        assert_eq!(
            query_rows_fake(&mut fake, &unchanged).await,
            query_rows_postgres(&mut postgres, &unchanged).await
        );

        let shape_table = format!("procedural_shape_{suffix}");
        let renamed_shape_table = format!("procedural_shape_renamed_{suffix}");
        let shape_function = format!("procedural_shape_fn_{suffix}");
        for sql in [
            format!("CREATE TABLE {shape_table} (id BIGINT PRIMARY KEY, value BIGINT)"),
            format!(r#"CREATE FUNCTION {shape_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER shape_trigger BEFORE INSERT ON {shape_table} FOR EACH ROW EXECUTE FUNCTION {shape_function}()"),
            format!("ALTER TABLE {shape_table} RENAME TO {renamed_shape_table}"),
            format!("INSERT INTO {renamed_shape_table} VALUES (1, 1)"),
            format!("ALTER TABLE {renamed_shape_table} RENAME COLUMN value TO renamed_value"),
            format!("INSERT INTO {renamed_shape_table} VALUES (2, 1)"),
            format!(r#"CREATE OR REPLACE FUNCTION {shape_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.renamed_value := NEW.renamed_value + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("INSERT INTO {renamed_shape_table} VALUES (2, 1)"),
            format!("ALTER TABLE {renamed_shape_table} DROP COLUMN renamed_value"),
            format!("INSERT INTO {renamed_shape_table} VALUES (3)"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }

        let rewrite_table = format!("procedural_rewrite_unique_{suffix}");
        let rewrite_function = format!("procedural_rewrite_unique_fn_{suffix}");
        for sql in [
            format!("CREATE TABLE {rewrite_table} (id BIGINT PRIMARY KEY)"),
            format!(r#"CREATE FUNCTION {rewrite_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.id := 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER rewrite_unique BEFORE INSERT ON {rewrite_table} FOR EACH ROW EXECUTE FUNCTION {rewrite_function}()"),
            format!("INSERT INTO {rewrite_table} VALUES (1)"),
            format!("INSERT INTO {rewrite_table} VALUES (2) ON CONFLICT (id) DO NOTHING"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let rewritten = format!("SELECT id, 0::BIGINT, '' FROM {rewrite_table} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &rewritten).await,
            query_rows_postgres(&mut postgres, &rewritten).await
        );

        for sql in [
            format!("CREATE TRIGGER duplicate_event BEFORE INSERT OR INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION {function}()"),
            "DO LANGUAGE sql $$ SELECT 1 $$".to_owned(),
            "DO $$ BEGIN RETURN 1; END; $$".to_owned(),
            "DO $$ BEGIN IF FALSE THEN missing := 1; END IF; END; $$".to_owned(),
            "DO $$ BEGIN RAISE EXCEPTION 'failed' USING HINT = NULL; END; $$".to_owned(),
            "DO $$ DECLARE value BIGINT := 9223372036854775807.5::NUMERIC; BEGIN END; $$".to_owned(),
            "DO $$ DECLARE x BIGINT := 9; y BIGINT; BEGIN SELECT x INTO y FROM (SELECT 1 AS x) source; END; $$".to_owned(),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }

        execute_both(
            &mut postgres,
            &mut fake,
            &format!(
                "DROP TABLE {rewrite_table}, {renamed_shape_table}, {ordering_table}, {table}"
            ),
        )
        .await;
    });
}

#[test]
fn procedural_evaluation_order_matches_postgres() {
    let server = common::start_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut postgres = PgConnection::connect(&server.url).await.unwrap();
        let mut fake = PgFakeConnection::new(Db::create());
        let suffix = std::process::id();
        let ordered_sequence = format!("procedural_ordered_sequence_{suffix}");
        let ordered_rows = format!("procedural_ordered_rows_{suffix}");
        let ordered_function = format!("procedural_ordered_function_{suffix}");
        let offset_rows = format!("procedural_offset_rows_{suffix}");
        let preserve_function = format!("procedural_preserve_function_{suffix}");
        let validation_rows = format!("procedural_validation_rows_{suffix}");
        let validation_function = format!("procedural_validation_function_{suffix}");
        let unique_sequence = format!("procedural_unique_sequence_{suffix}");
        let unique_rows = format!("procedural_unique_rows_{suffix}");
        let unique_function = format!("procedural_unique_function_{suffix}");

        for sql in [
            format!("CREATE SEQUENCE {ordered_sequence}"),
            format!("CREATE TABLE {ordered_rows} (id BIGINT, allocated BIGINT)"),
            format!(r#"CREATE FUNCTION {ordered_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.allocated := nextval('{ordered_sequence}'); RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER allocate_ordered BEFORE INSERT ON {ordered_rows} FOR EACH ROW EXECUTE FUNCTION {ordered_function}()"),
            format!("INSERT INTO {ordered_rows} SELECT nextval('{ordered_sequence}'), 0 FROM (VALUES (2), (1)) AS source(position) ORDER BY position LIMIT 2"),
            format!("CREATE TABLE {offset_rows} (value BIGINT)"),
            format!(r#"CREATE FUNCTION {preserve_function}() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER preserve_offset BEFORE INSERT ON {offset_rows} FOR EACH ROW EXECUTE FUNCTION {preserve_function}()"),
            format!("CREATE TABLE {validation_rows} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL)"),
            format!(r#"CREATE FUNCTION {validation_function}() RETURNS TRIGGER AS $$
                BEGIN
                    IF NEW.id = 2 THEN NEW.value := 1 / 0; END IF;
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER validate_order BEFORE INSERT ON {validation_rows} FOR EACH ROW EXECUTE FUNCTION {validation_function}()"),
            format!("CREATE SEQUENCE {unique_sequence}"),
            format!("CREATE TABLE {unique_rows} (id BIGINT PRIMARY KEY, allocated BIGINT)"),
            format!("INSERT INTO {unique_rows} VALUES (1, 0)"),
            format!(r#"CREATE FUNCTION {unique_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.allocated := nextval('{unique_sequence}'); RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER allocate_unique BEFORE INSERT ON {unique_rows} FOR EACH ROW EXECUTE FUNCTION {unique_function}()"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }

        let sql = format!("SELECT id, allocated, '' FROM {ordered_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("INSERT INTO {offset_rows} SELECT 1 / (id - 1) FROM (VALUES (1), (2)) AS source(id) ORDER BY id OFFSET 1"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("INSERT INTO {validation_rows} VALUES (1, NULL), (2, 0)"),
        )
        .await;
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("INSERT INTO {unique_rows} VALUES (1, 0), (2, 0)"),
        )
        .await;
        let sql = format!("SELECT nextval('{unique_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let aggregate_sequence = format!("procedural_aggregate_sequence_{suffix}");
        let aggregate_source = format!("procedural_aggregate_source_{suffix}");
        let aggregate_rows = format!("procedural_aggregate_rows_{suffix}");
        let aggregate_function = format!("procedural_aggregate_function_{suffix}");
        for sql in [
            format!("CREATE SEQUENCE {aggregate_sequence}"),
            format!("CREATE TABLE {aggregate_source} (id BIGINT)"),
            format!("INSERT INTO {aggregate_source} VALUES (1), (2), (1)"),
            format!("CREATE TABLE {aggregate_rows} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL)"),
            format!("INSERT INTO {aggregate_rows} VALUES (1, 0)"),
            format!(r#"CREATE FUNCTION {aggregate_function}() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER preserve_aggregate BEFORE INSERT ON {aggregate_rows} FOR EACH ROW EXECUTE FUNCTION {aggregate_function}()"),
            format!("INSERT INTO {aggregate_rows} SELECT id, sum(nextval('{aggregate_sequence}'))::BIGINT FROM {aggregate_source} GROUP BY id HAVING count(*) > 1 ON CONFLICT (id) DO UPDATE SET value = excluded.value"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, value, '' FROM {aggregate_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('{aggregate_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let group_sequence = format!("procedural_group_sequence_{suffix}");
        let group_source = format!("procedural_group_source_{suffix}");
        let group_rows = format!("procedural_group_rows_{suffix}");
        let group_function = format!("procedural_group_function_{suffix}");
        for sql in [
            format!("CREATE SEQUENCE {group_sequence}"),
            format!("CREATE TABLE {group_source} (id BIGINT)"),
            format!("INSERT INTO {group_source} VALUES (1), (2), (3), (4)"),
            format!("CREATE TABLE {group_rows} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL)"),
            format!("INSERT INTO {group_rows} VALUES (1, 0)"),
            format!(r#"CREATE FUNCTION {group_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.value := nextval('{group_sequence}'); RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER allocate_group BEFORE INSERT ON {group_rows} FOR EACH ROW EXECUTE FUNCTION {group_function}()"),
            format!("INSERT INTO {group_rows} SELECT id, nextval('{group_sequence}') AS z FROM {group_source} GROUP BY id HAVING nextval('{group_sequence}') % 2 = 1 ORDER BY z ON CONFLICT (id) DO UPDATE SET value = excluded.value"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, value, '' FROM {group_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let self_rows = format!("procedural_self_rows_{suffix}");
        let fk_parents = format!("procedural_fk_parents_{suffix}");
        let fk_rows = format!("procedural_fk_rows_{suffix}");
        let fk_sequence = format!("procedural_fk_sequence_{suffix}");
        let fk_function = format!("procedural_fk_function_{suffix}");
        for sql in [
            format!("CREATE TABLE {self_rows} (id BIGINT PRIMARY KEY, parent_id BIGINT REFERENCES {self_rows}(id))"),
            format!("INSERT INTO {self_rows} VALUES (1, 2), (2, NULL)"),
            format!("CREATE TABLE {fk_parents} (id BIGINT PRIMARY KEY)"),
            format!("INSERT INTO {fk_parents} VALUES (2)"),
            format!("CREATE SEQUENCE {fk_sequence}"),
            format!("CREATE TABLE {fk_rows} (id BIGINT PRIMARY KEY, parent_id BIGINT REFERENCES {fk_parents}(id), allocated BIGINT)"),
            format!(r#"CREATE FUNCTION {fk_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.allocated := nextval('{fk_sequence}'); RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER allocate_fk BEFORE INSERT ON {fk_rows} FOR EACH ROW EXECUTE FUNCTION {fk_function}()"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, COALESCE(parent_id, 0), '' FROM {self_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("INSERT INTO {fk_rows} VALUES (1, 99, 0), (2, 2, 0)"),
        )
        .await;
        let sql = format!("SELECT nextval('{fk_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let volatile_sequence = format!("procedural_volatile_sequence_{suffix}");
        let volatile_rows = format!("procedural_volatile_rows_{suffix}");
        let volatile_function = format!("procedural_volatile_function_{suffix}");
        for sql in [
            format!("CREATE SEQUENCE {volatile_sequence}"),
            format!("CREATE TABLE {volatile_rows} (id BIGINT, allocated BIGINT)"),
            format!(r#"CREATE FUNCTION {volatile_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.allocated := nextval('{volatile_sequence}'); RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER allocate_volatile BEFORE INSERT ON {volatile_rows} FOR EACH ROW EXECUTE FUNCTION {volatile_function}()"),
            format!("WITH source AS (SELECT nextval('{volatile_sequence}') AS id), inserted AS (INSERT INTO {volatile_rows} SELECT id + 10, 0 FROM source RETURNING id) SELECT * FROM inserted"),
            format!("WITH inserted AS (INSERT INTO {volatile_rows} VALUES ((SELECT nextval('{volatile_sequence}') + 30), 0) RETURNING id) SELECT * FROM inserted"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, allocated, '' FROM {volatile_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('{volatile_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        execute_both(
            &mut postgres,
            &mut fake,
            &format!("WITH first_value AS (SELECT nextval('{volatile_sequence}') AS value), second_value AS (SELECT nextval('{volatile_sequence}') AS value), inserted AS (INSERT INTO {volatile_rows} SELECT first_value.value, second_value.value FROM first_value CROSS JOIN second_value RETURNING id) SELECT * FROM inserted"),
        )
        .await;
        let sql = format!("SELECT id, allocated, '' FROM {volatile_rows} ORDER BY id");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('{volatile_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let scalar_sequence = format!("procedural_scalar_sequence_{suffix}");
        let scalar_rows = format!("procedural_scalar_rows_{suffix}");
        let scalar_function = format!("procedural_scalar_function_{suffix}");
        for sql in [
            format!("CREATE SEQUENCE {scalar_sequence}"),
            format!("CREATE TABLE {scalar_rows} (id BIGINT, allocated BIGINT)"),
            format!(r#"CREATE FUNCTION {scalar_function}() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER preserve_scalar BEFORE INSERT ON {scalar_rows} FOR EACH ROW EXECUTE FUNCTION {scalar_function}()"),
            format!("WITH earlier AS (SELECT (SELECT nextval('{scalar_sequence}')) AS value), inserted AS (INSERT INTO {scalar_rows} VALUES ((SELECT nextval('{scalar_sequence}')), 0) RETURNING id) SELECT earlier.value, inserted.id FROM earlier CROSS JOIN inserted"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, allocated, '' FROM {scalar_rows}");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('{scalar_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        let nested_sequence = format!("procedural_nested_sequence_{suffix}");
        let nested_rows = format!("procedural_nested_rows_{suffix}");
        for sql in [
            format!("CREATE SEQUENCE {nested_sequence}"),
            format!("CREATE TABLE {nested_rows} (id BIGINT, allocated BIGINT)"),
            format!("CREATE TRIGGER preserve_scalar BEFORE INSERT ON {nested_rows} FOR EACH ROW EXECUTE FUNCTION {scalar_function}()"),
            format!("WITH inserted AS (INSERT INTO {nested_rows} VALUES ((SELECT nextval('{nested_sequence}') + (SELECT nextval('{nested_sequence}'))), 0) RETURNING id) SELECT * FROM inserted"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, allocated, '' FROM {nested_rows}");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
        let sql = format!("SELECT nextval('{nested_sequence}'), 0::BIGINT, ''");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );

        execute_both(
            &mut postgres,
            &mut fake,
            r#"DO $$ DECLARE x BIGINT := 9; grouped BIGINT;
               BEGIN
                   SELECT count(*) AS x INTO grouped
                   FROM (VALUES (2), (1)) AS source(id) GROUP BY x + 0;
                   IF grouped <> 2 THEN RAISE EXCEPTION 'unexpected count %', grouped; END IF;
               END; $$"#,
        )
        .await;

        let update_rows = format!("procedural_update_rows_{suffix}");
        let update_function = format!("procedural_update_function_{suffix}");
        let update_source = format!("procedural_update_source_{suffix}");
        for sql in [
            format!("CREATE TABLE {update_rows} (id BIGINT PRIMARY KEY, value BIGINT, changes BIGINT)"),
            format!("CREATE TABLE {update_source} (id BIGINT PRIMARY KEY)"),
            format!("INSERT INTO {update_rows} VALUES (1, 1, 0)"),
            format!(r#"CREATE FUNCTION {update_function}() RETURNS TRIGGER AS $$
                BEGIN NEW.changes := NEW.changes + 1; RETURN NEW; END;
                $$ LANGUAGE plpgsql"#),
            format!("CREATE TRIGGER track_update BEFORE UPDATE ON {update_rows} FOR EACH ROW EXECUTE FUNCTION {update_function}()"),
            format!("WITH source AS (SELECT 1 AS id), updated AS (UPDATE {update_rows} SET value = value + 1 FROM source WHERE {update_rows}.id = source.id RETURNING {update_rows}.id) SELECT * FROM updated"),
            format!("WITH source AS (INSERT INTO {update_source} VALUES (1) RETURNING id), updated AS (UPDATE {update_rows} SET value = value + 1 FROM source WHERE {update_rows}.id = source.id RETURNING {update_rows}.id) SELECT * FROM updated"),
        ] {
            execute_both(&mut postgres, &mut fake, &sql).await;
        }
        let sql = format!("SELECT id, value, changes::TEXT FROM {update_rows}");
        assert_eq!(
            query_rows_fake(&mut fake, &sql).await,
            query_rows_postgres(&mut postgres, &sql).await
        );
    });
}
