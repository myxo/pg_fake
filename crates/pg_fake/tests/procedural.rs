use pg_fake::{
    api::{Db, StatementResult},
    error::SqlState,
    value::Value,
};

fn query_rows(session: &mut pg_fake::api::Session, sql: &str) -> Vec<Vec<Value>> {
    session.query(sql, &[]).unwrap().rows
}

#[test]
fn executes_before_insert_and_update_triggers() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                value BIGINT NOT NULL CHECK (value > 0),
                label TEXT
            );
            CREATE FUNCTION normalize_item() RETURNS TRIGGER AS $$
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
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER normalize_item
                BEFORE INSERT OR UPDATE ON items
                FOR EACH ROW EXECUTE FUNCTION normalize_item();
            "#,
        )
        .unwrap();

    let results = session
        .execute(
            "INSERT INTO items VALUES (1, NULL, 'first'), (2, 4, NULL), (3, 4, 'third') RETURNING id, value",
        )
        .unwrap();
    assert_eq!(
        results,
        vec![StatementResult::Query(pg_fake::api::QueryResult {
            columns: match &results[0] {
                StatementResult::Query(query) => query.columns.clone(),
                _ => unreachable!(),
            },
            rows: vec![
                vec![Value::Int4(1), Value::Int8(1)],
                vec![Value::Int4(3), Value::Int8(5)],
            ],
        })]
    );

    assert_eq!(
        session
            .execute("UPDATE items SET value = value + 1")
            .unwrap(),
        vec![StatementResult::Affected(2)]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT id, value FROM items ORDER BY id"),
        vec![
            vec![Value::Int4(1), Value::Int8(3)],
            vec![Value::Int4(3), Value::Int8(7)],
        ]
    );
}

#[test]
fn fires_triggers_in_name_order_and_before_conflict_update() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE counters (id INTEGER PRIMARY KEY, value BIGINT NOT NULL);
            CREATE FUNCTION add_one() RETURNS TRIGGER AS $$
            BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE FUNCTION double_value() RETURNS TRIGGER AS $$
            BEGIN NEW.value := NEW.value * 2; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER z_double BEFORE INSERT OR UPDATE ON counters
                FOR EACH ROW EXECUTE FUNCTION double_value();
            CREATE TRIGGER a_add BEFORE INSERT OR UPDATE ON counters
                FOR EACH ROW EXECUTE FUNCTION add_one();
            INSERT INTO counters VALUES (1, 2);
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT value FROM counters"),
        vec![vec![Value::Int8(6)]]
    );

    session
        .execute(
            "INSERT INTO counters VALUES (1, 3) ON CONFLICT (id) DO UPDATE SET value = excluded.value",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT value FROM counters"),
        vec![vec![Value::Int8(18)]]
    );

    session
        .execute(
            "ALTER TRIGGER a_add ON counters RENAME TO zz_add; \
             INSERT INTO counters VALUES (2, 2)",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT value FROM counters WHERE id = 2"),
        vec![vec![Value::Int8(5)]]
    );
}

#[test]
fn interleaves_defaults_and_before_triggers_for_each_inserted_row() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE SEQUENCE insert_order_sequence;
            CREATE TABLE insert_order (
                id BIGINT DEFAULT nextval('insert_order_sequence'),
                triggered BIGINT
            );
            CREATE FUNCTION allocate_triggered_value() RETURNS TRIGGER AS $$
            BEGIN NEW.triggered := nextval('insert_order_sequence'); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_triggered_value BEFORE INSERT ON insert_order
                FOR EACH ROW EXECUTE FUNCTION allocate_triggered_value();
            INSERT INTO insert_order(triggered) VALUES (0), (0);
            "#,
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, triggered FROM insert_order ORDER BY id"
        ),
        vec![
            vec![Value::Int8(1), Value::Int8(2)],
            vec![Value::Int8(3), Value::Int8(4)],
        ]
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE insert_select_order_sequence;
            CREATE TABLE insert_select_source (id BIGINT);
            INSERT INTO insert_select_source VALUES (1), (2);
            CREATE TABLE insert_select_order (id BIGINT, triggered BIGINT);
            CREATE FUNCTION allocate_insert_select_value() RETURNS TRIGGER AS $$
            BEGIN NEW.triggered := nextval('insert_select_order_sequence'); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_insert_select_value BEFORE INSERT ON insert_select_order
                FOR EACH ROW EXECUTE FUNCTION allocate_insert_select_value();
            INSERT INTO insert_select_order
                SELECT nextval('insert_select_order_sequence'), 0 FROM insert_select_source;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, triggered FROM insert_select_order ORDER BY id"
        ),
        vec![
            vec![Value::Int8(1), Value::Int8(2)],
            vec![Value::Int8(3), Value::Int8(4)],
        ]
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE ordered_insert_select_sequence;
            CREATE TABLE ordered_insert_select (id BIGINT, triggered BIGINT);
            CREATE FUNCTION allocate_ordered_insert_select_value() RETURNS TRIGGER AS $$
            BEGIN NEW.triggered := nextval('ordered_insert_select_sequence'); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_ordered_insert_select_value
                BEFORE INSERT ON ordered_insert_select
                FOR EACH ROW EXECUTE FUNCTION allocate_ordered_insert_select_value();
            INSERT INTO ordered_insert_select
                SELECT nextval('ordered_insert_select_sequence'), 0
                FROM (VALUES (2), (1)) AS source(position)
                ORDER BY source.position LIMIT 2;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, triggered FROM ordered_insert_select ORDER BY id"
        ),
        vec![
            vec![Value::Int8(1), Value::Int8(2)],
            vec![Value::Int8(3), Value::Int8(4)],
        ]
    );

    session
        .execute(
            r#"
            CREATE TABLE offset_insert_select (value BIGINT);
            CREATE FUNCTION preserve_offset_insert_select() RETURNS TRIGGER AS $$
            BEGIN RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER preserve_offset_insert_select
                BEFORE INSERT ON offset_insert_select
                FOR EACH ROW EXECUTE FUNCTION preserve_offset_insert_select();
            "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute(
                "INSERT INTO offset_insert_select \
                 SELECT 1 / (id - 1) FROM (VALUES (1), (2)) AS source(id) \
                 ORDER BY id OFFSET 1"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DivisionByZero
    );
}

#[test]
fn validates_each_inserted_row_before_running_the_next_trigger() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE trigger_validation_order (id BIGINT, value BIGINT NOT NULL);
            CREATE FUNCTION fail_second_trigger_row() RETURNS TRIGGER AS $$
            BEGIN
                IF NEW.id = 2 THEN
                    NEW.value := 1 / 0;
                END IF;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER fail_second_trigger_row
                BEFORE INSERT ON trigger_validation_order
                FOR EACH ROW EXECUTE FUNCTION fail_second_trigger_row();
            "#,
        )
        .unwrap();

    assert_eq!(
        session
            .execute("INSERT INTO trigger_validation_order VALUES (1, NULL), (2, 0)")
            .unwrap_err()
            .sqlstate,
        SqlState::NotNullViolation
    );

    session
        .execute(
            r#"
            CREATE TABLE trigger_unique_order (id BIGINT PRIMARY KEY, value BIGINT);
            INSERT INTO trigger_unique_order VALUES (1, 0);
            CREATE TRIGGER fail_second_trigger_row
                BEFORE INSERT ON trigger_unique_order
                FOR EACH ROW EXECUTE FUNCTION fail_second_trigger_row();
            "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO trigger_unique_order VALUES (1, 0), (2, 0)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE trigger_unique_sequence;
            CREATE TABLE trigger_unique_sequence_order (id BIGINT PRIMARY KEY, value BIGINT);
            INSERT INTO trigger_unique_sequence_order VALUES (1, 0);
            CREATE FUNCTION allocate_unique_trigger_value() RETURNS TRIGGER AS $$
            BEGIN NEW.value := nextval('trigger_unique_sequence'); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_unique_trigger_value
                BEFORE INSERT ON trigger_unique_sequence_order
                FOR EACH ROW EXECUTE FUNCTION allocate_unique_trigger_value();
            "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO trigger_unique_sequence_order VALUES (1, 0), (2, 0)")
            .unwrap_err()
            .sqlstate,
        SqlState::UniqueViolation
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('trigger_unique_sequence')"),
        vec![vec![Value::Int8(2)]]
    );

    session
        .execute(
            r#"
            CREATE TABLE later_self_reference (
                id BIGINT PRIMARY KEY,
                parent_id BIGINT REFERENCES later_self_reference(id)
            );
            INSERT INTO later_self_reference VALUES (1, 2), (2, NULL);
            CREATE TABLE trigger_fk_parent (id BIGINT PRIMARY KEY);
            INSERT INTO trigger_fk_parent VALUES (2);
            CREATE SEQUENCE trigger_fk_sequence;
            CREATE TABLE trigger_fk_child (
                id BIGINT PRIMARY KEY,
                parent_id BIGINT REFERENCES trigger_fk_parent(id),
                allocated BIGINT
            );
            CREATE FUNCTION allocate_trigger_fk_value() RETURNS TRIGGER AS $$
            BEGIN NEW.allocated := nextval('trigger_fk_sequence'); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_trigger_fk_value BEFORE INSERT ON trigger_fk_child
                FOR EACH ROW EXECUTE FUNCTION allocate_trigger_fk_value();
            "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO trigger_fk_child VALUES (1, 99, 0), (2, 2, 0)")
            .unwrap_err()
            .sqlstate,
        SqlState::ForeignKeyViolation
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('trigger_fk_sequence')"),
        vec![vec![Value::Int8(3)]]
    );
}

#[test]
fn prepares_materialized_trigger_inserts_once_before_locking() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE SEQUENCE prepared_trigger_sequence;
            CREATE TABLE prepared_trigger_rows (id BIGINT, allocated BIGINT);
            CREATE FUNCTION allocate_prepared_trigger_value() RETURNS TRIGGER AS $$
            BEGIN
                NEW.allocated := nextval('prepared_trigger_sequence');
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_prepared_trigger_value
                BEFORE INSERT ON prepared_trigger_rows
                FOR EACH ROW EXECUTE FUNCTION allocate_prepared_trigger_value();
            WITH source AS (SELECT 1 AS id), inserted AS (
                INSERT INTO prepared_trigger_rows
                    SELECT id, 0 FROM source RETURNING id
            ) SELECT * FROM inserted;
            WITH inserted AS (
                INSERT INTO prepared_trigger_rows
                    VALUES ((SELECT 2), 0) RETURNING id
            ) SELECT * FROM inserted;
            WITH first_insert AS (
                INSERT INTO prepared_trigger_rows VALUES (3, 0) RETURNING id
            ), second_insert AS (
                INSERT INTO prepared_trigger_rows VALUES (4, 0) RETURNING id
            ) SELECT * FROM first_insert, second_insert;
            "#,
        )
        .unwrap();

    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, allocated FROM prepared_trigger_rows ORDER BY id"
        ),
        vec![
            vec![Value::Int8(1), Value::Int8(1)],
            vec![Value::Int8(2), Value::Int8(2)],
            vec![Value::Int8(3), Value::Int8(3)],
            vec![Value::Int8(4), Value::Int8(4)],
        ]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('prepared_trigger_sequence')"),
        vec![vec![Value::Int8(5)]]
    );

    session
        .execute(
            r#"
            CREATE TABLE plain_cte_rows (id BIGINT);
            WITH plain_insert AS (
                INSERT INTO plain_cte_rows VALUES (1) RETURNING id
            ), triggered_insert AS (
                INSERT INTO prepared_trigger_rows VALUES (7, 0) RETURNING id
            ) SELECT * FROM plain_insert, triggered_insert;
            WITH first_insert AS (
                INSERT INTO prepared_trigger_rows SELECT 8, 0 RETURNING id
            ), second_insert AS (
                INSERT INTO prepared_trigger_rows VALUES (9, 0) RETURNING id
            ) SELECT * FROM first_insert, second_insert;
            WITH first_insert AS (
                INSERT INTO prepared_trigger_rows VALUES (10, 0) RETURNING id
            ), second_insert AS (
                INSERT INTO prepared_trigger_rows
                    SELECT id + 1, 0 FROM first_insert RETURNING id
            ) SELECT * FROM second_insert;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id FROM prepared_trigger_rows WHERE id >= 7 ORDER BY id"
        ),
        vec![
            vec![Value::Int8(7)],
            vec![Value::Int8(8)],
            vec![Value::Int8(9)],
            vec![Value::Int8(10)],
            vec![Value::Int8(11)],
        ]
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE volatile_cte_sequence;
            CREATE TABLE volatile_cte_rows (id BIGINT, allocated BIGINT);
            CREATE FUNCTION allocate_volatile_cte_value() RETURNS TRIGGER AS $$
            BEGIN NEW.allocated := nextval('volatile_cte_sequence'); RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER allocate_volatile_cte_value BEFORE INSERT ON volatile_cte_rows
                FOR EACH ROW EXECUTE FUNCTION allocate_volatile_cte_value();
            WITH source AS (SELECT nextval('volatile_cte_sequence') AS id), inserted AS (
                INSERT INTO volatile_cte_rows SELECT id + 10, 0 FROM source RETURNING id
            ) SELECT * FROM inserted;
            WITH RECURSIVE dependent AS (
                INSERT INTO volatile_cte_rows
                    SELECT id + 1, 0 FROM seed RETURNING id
            ), seed AS (
                INSERT INTO volatile_cte_rows VALUES (20, 0) RETURNING id
            ) SELECT * FROM dependent;
            WITH inserted AS (
                INSERT INTO volatile_cte_rows
                    VALUES ((SELECT nextval('volatile_cte_sequence') + 30), 0)
                    RETURNING id
            ) SELECT * FROM inserted;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT id, allocated FROM volatile_cte_rows"),
        vec![
            vec![Value::Int8(11), Value::Int8(2)],
            vec![Value::Int8(20), Value::Int8(3)],
            vec![Value::Int8(21), Value::Int8(4)],
            vec![Value::Int8(35), Value::Int8(6)],
        ]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('volatile_cte_sequence')"),
        vec![vec![Value::Int8(7)]]
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE distinct_cte_sequence;
            CREATE TABLE distinct_cte_rows (first_value BIGINT, second_value BIGINT);
            CREATE FUNCTION preserve_distinct_cte_values() RETURNS TRIGGER AS $$
            BEGIN RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER preserve_distinct_cte_values BEFORE INSERT ON distinct_cte_rows
                FOR EACH ROW EXECUTE FUNCTION preserve_distinct_cte_values();
            WITH first_value AS (
                SELECT nextval('distinct_cte_sequence') AS value
            ), second_value AS (
                SELECT nextval('distinct_cte_sequence') AS value
            ), inserted AS (
                INSERT INTO distinct_cte_rows
                    SELECT first_value.value, second_value.value
                    FROM first_value CROSS JOIN second_value
                    RETURNING first_value
            ) SELECT * FROM inserted;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT first_value, second_value FROM distinct_cte_rows"
        ),
        vec![vec![Value::Int8(1), Value::Int8(2)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('distinct_cte_sequence')"),
        vec![vec![Value::Int8(3)]]
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE scalar_occurrence_sequence;
            CREATE TABLE scalar_occurrence_rows (id BIGINT, allocated BIGINT);
            CREATE FUNCTION preserve_scalar_occurrence_values() RETURNS TRIGGER AS $$
            BEGIN RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER preserve_scalar_occurrence_values
                BEFORE INSERT ON scalar_occurrence_rows
                FOR EACH ROW EXECUTE FUNCTION preserve_scalar_occurrence_values();
            WITH RECURSIVE earlier AS (
                SELECT (SELECT nextval('scalar_occurrence_sequence')) AS value
            ), inserted AS (
                INSERT INTO scalar_occurrence_rows
                    VALUES ((SELECT nextval('scalar_occurrence_sequence')), 0)
                    RETURNING id
            ) SELECT earlier.value, inserted.id FROM earlier CROSS JOIN inserted;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, allocated FROM scalar_occurrence_rows"
        ),
        vec![vec![Value::Int8(2), Value::Int8(0)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('scalar_occurrence_sequence')"),
        vec![vec![Value::Int8(3)]]
    );

    session
        .execute(
            r#"
            CREATE SEQUENCE nested_scalar_sequence;
            CREATE TABLE nested_scalar_rows (id BIGINT, allocated BIGINT);
            CREATE TRIGGER preserve_scalar_occurrence_values
                BEFORE INSERT ON nested_scalar_rows
                FOR EACH ROW EXECUTE FUNCTION preserve_scalar_occurrence_values();
            WITH inserted AS (
                INSERT INTO nested_scalar_rows
                    VALUES ((SELECT nextval('nested_scalar_sequence')
                        + (SELECT nextval('nested_scalar_sequence'))), 0)
                    RETURNING id
            ) SELECT * FROM inserted;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT id, allocated FROM nested_scalar_rows"),
        vec![vec![Value::Int8(3), Value::Int8(0)]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('nested_scalar_sequence')"),
        vec![vec![Value::Int8(3)]]
    );
}

#[test]
fn preserves_function_identity_across_replacement_and_rollback() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE values_table (id INTEGER PRIMARY KEY, value BIGINT);
            CREATE FUNCTION set_value() RETURNS TRIGGER AS $$
            BEGIN NEW.value := 1; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER set_value BEFORE INSERT ON values_table
                FOR EACH ROW EXECUTE FUNCTION set_value();
            CREATE OR REPLACE FUNCTION set_value() RETURNS TRIGGER AS $$
            BEGIN NEW.value := 2; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            INSERT INTO values_table VALUES (1, 0);
            "#,
        )
        .unwrap();
    session
        .execute(
            r#"
            DO $$
            DECLARE x BIGINT := 9; grouped BIGINT;
            BEGIN
                SELECT count(*) AS x INTO grouped
                FROM (VALUES (2), (1)) AS source(id)
                GROUP BY x + 0;
                IF grouped <> 2 THEN
                    RAISE EXCEPTION 'unexpected grouped count %', grouped;
                END IF;
            END;
            $$
            "#,
        )
        .unwrap();
    session.execute("BEGIN").unwrap();
    session
        .execute(
            r#"CREATE OR REPLACE FUNCTION set_value() RETURNS TRIGGER AS $$
            BEGIN NEW.value := 3; RETURN NEW; END;
            $$ LANGUAGE plpgsql"#,
        )
        .unwrap();
    session.execute("ROLLBACK").unwrap();
    session
        .execute("INSERT INTO values_table VALUES (2, 0)")
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, value FROM values_table ORDER BY id"
        ),
        vec![
            vec![Value::Int4(1), Value::Int8(2)],
            vec![Value::Int4(2), Value::Int8(2)],
        ]
    );

    assert_eq!(
        session
            .execute("DROP FUNCTION set_value()")
            .unwrap_err()
            .sqlstate,
        SqlState::DependentObjectsStillExist
    );
    session
        .execute("DROP FUNCTION set_value() CASCADE")
        .unwrap();
    session
        .execute("INSERT INTO values_table VALUES (3, 9)")
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT value FROM values_table WHERE id = 3"),
        vec![vec![Value::Int8(9)]]
    );
}

#[test]
fn executes_do_blocks_with_locals_diagnostics_and_errors() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE TABLE events (id BIGINT, label TEXT)")
        .unwrap();
    session
        .execute(
            r#"
            DO $$
            DECLARE
                affected BIGINT;
                selected BIGINT;
                label TEXT := 'created';
            BEGIN
                INSERT INTO events VALUES (1, label), (2, label);
                GET DIAGNOSTICS affected = ROW_COUNT;
                selected := '2';
                label = 'created';
                SELECT affected, label INTO selected, label;
                IF selected = 2 AND label IS NOT NULL THEN
                    UPDATE events SET label = 'done';
                ELSE
                    RAISE EXCEPTION 'unexpected %', selected USING HINT = 'check block';
                END IF;
            END;
            $$;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT id, label FROM events ORDER BY id"),
        vec![
            vec![Value::Int8(1), Value::Text("done".into())],
            vec![Value::Int8(2), Value::Text("done".into())],
        ]
    );

    session.execute("BEGIN").unwrap();
    let error = session
        .execute(
            r#"DO $$
            DECLARE affected BIGINT := 7;
            BEGIN
                INSERT INTO events VALUES (3, 'pending');
                RAISE EXCEPTION 'failed % %%', affected USING HINT = 'rolled back';
            END;
            $$"#,
        )
        .unwrap_err();
    assert_eq!(error.sqlstate, SqlState::RaiseException);
    assert_eq!(error.message, "failed 7 %");
    assert_eq!(error.hint.as_deref(), Some("rolled back"));
    session.execute("ROLLBACK").unwrap();
    assert!(query_rows(&mut session, "SELECT id FROM events WHERE id = 3").is_empty());
}

#[test]
fn renames_and_drops_triggers_transactionally() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE items (value BIGINT);
            CREATE FUNCTION increment_value() RETURNS TRIGGER AS $$
            BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER old_name BEFORE INSERT ON items
                FOR EACH ROW EXECUTE FUNCTION increment_value();
            "#,
        )
        .unwrap();
    session.execute("BEGIN").unwrap();
    session
        .execute(
            "ALTER TRIGGER old_name ON items RENAME TO new_name; \
             DROP TRIGGER new_name ON items",
        )
        .unwrap();
    session.execute("ROLLBACK").unwrap();
    session
        .execute(
            "INSERT INTO items VALUES (1); \
             DROP TRIGGER IF EXISTS missing ON items; \
             DROP TRIGGER old_name ON items; \
             INSERT INTO items VALUES (1)",
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT value FROM items"),
        vec![vec![Value::Int8(2)], vec![Value::Int8(1)]]
    );
}

#[test]
fn preserves_procedural_catalog_dependencies_and_rejects_unsupported_forms() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE public.catalog_items (
                id BIGINT PRIMARY KEY,
                value BIGINT CHECK (value > 0)
            );
            CREATE FUNCTION public.catalog_function() RETURNS TRIGGER AS $$
            BEGIN NEW.value := NEW.value + 1; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER catalog_trigger BEFORE INSERT ON public.catalog_items
                FOR EACH ROW EXECUTE FUNCTION public.catalog_function();
            "#,
        )
        .unwrap();

    assert_eq!(
        session
            .execute(
                r#"CREATE FUNCTION public.catalog_function() RETURNS TRIGGER AS $$
                BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql"#
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DuplicateFunction
    );
    assert_eq!(
        session
            .execute(
                "CREATE TRIGGER catalog_trigger BEFORE INSERT ON public.catalog_items \
                 FOR EACH ROW EXECUTE FUNCTION public.catalog_function()"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::DuplicateObject
    );
    for sql in [
        r#"CREATE FUNCTION unsupported_option() RETURNS TRIGGER AS $$
           BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql VOLATILE"#,
        r#"CREATE FUNCTION quoted_new() RETURNS TRIGGER AS $$
           BEGIN RETURN "NEW"; END; $$ LANGUAGE plpgsql"#,
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            SqlState::FeatureNotSupported
        );
    }
    assert_eq!(
        session
            .execute(
                "CREATE TRIGGER duplicate_event BEFORE INSERT OR INSERT ON public.catalog_items \
                 FOR EACH ROW EXECUTE FUNCTION public.catalog_function()"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::SyntaxError
    );
    session
        .execute("DROP TRIGGER IF EXISTS absent ON absent_table")
        .unwrap();
    session
        .execute("CREATE TEMP TABLE temporary_trigger_items (id BIGINT)")
        .unwrap();
    assert_eq!(
        session
            .execute(
                "CREATE TRIGGER temporary_trigger BEFORE INSERT ON temporary_trigger_items \
                 FOR EACH ROW EXECUTE FUNCTION public.catalog_function()"
            )
            .unwrap_err()
            .sqlstate,
        SqlState::FeatureNotSupported
    );

    session.execute("BEGIN").unwrap();
    session
        .execute(
            r#"CREATE FUNCTION rolled_back_function() RETURNS TRIGGER AS $$
               BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql"#,
        )
        .unwrap();
    session.execute("ROLLBACK").unwrap();
    session
        .execute(
            r#"CREATE FUNCTION rolled_back_function() RETURNS TRIGGER AS $$
               BEGIN RETURN NEW; END; $$ LANGUAGE plpgsql"#,
        )
        .unwrap();

    session.execute("BEGIN").unwrap();
    session
        .execute("DROP FUNCTION public.catalog_function() CASCADE")
        .unwrap();
    session.execute("ROLLBACK").unwrap();
    session
        .execute(
            "ALTER TABLE public.catalog_items RENAME TO renamed_catalog_items; \
             INSERT INTO public.renamed_catalog_items VALUES (1, 1)",
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT value FROM public.renamed_catalog_items"
        ),
        vec![vec![Value::Int8(2)]]
    );

    session
        .execute("ALTER TABLE public.renamed_catalog_items RENAME COLUMN value TO renamed_value")
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO public.renamed_catalog_items VALUES (2, 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedColumn
    );
    session
        .execute(
            r#"CREATE OR REPLACE FUNCTION public.catalog_function() RETURNS TRIGGER AS $$
               BEGIN NEW.renamed_value := NEW.renamed_value + 1; RETURN NEW; END;
               $$ LANGUAGE plpgsql;
               INSERT INTO public.renamed_catalog_items VALUES (2, 1)"#,
        )
        .unwrap();

    session
        .execute(
            r#"
            CREATE TABLE constrained_items (id BIGINT, value BIGINT CHECK (value > 0));
            CREATE FUNCTION violate_check() RETURNS TRIGGER AS $$
            BEGIN NEW.value := -1; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER violate_check BEFORE INSERT ON constrained_items
                FOR EACH ROW EXECUTE FUNCTION violate_check();
            "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO constrained_items VALUES (1, 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::CheckViolation
    );
    assert!(query_rows(&mut session, "SELECT id FROM constrained_items").is_empty());

    session
        .execute(
            r#"
            CREATE FUNCTION missing_return() RETURNS TRIGGER AS $$
            BEGIN IF NEW.id > 0 THEN RETURN NEW; END IF; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER missing_return BEFORE INSERT ON constrained_items
                FOR EACH ROW EXECUTE FUNCTION missing_return();
            "#,
        )
        .unwrap();
    assert_eq!(
        session
            .execute("INSERT INTO constrained_items VALUES (0, 1)")
            .unwrap_err()
            .sqlstate,
        SqlState::FunctionExecutedNoReturnStatement
    );
}

#[test]
fn executes_do_assignment_query_and_select_into_edges() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE do_source (id BIGINT); \
             INSERT INTO do_source VALUES (1), (2); \
             CREATE TABLE do_results (first_value BIGINT, missing_value TEXT); \
             CREATE SEQUENCE do_sequence",
        )
        .unwrap();
    session
        .execute(
            r#"
            DO $$
            DECLARE
                text_value TEXT := '7';
                number_value BIGINT := text_value;
                missing_value TEXT := 'present';
            BEGIN
                number_value = number_value + 1;
                SELECT nextval('do_sequence'), 'unused', 'extra'
                    INTO number_value, missing_value
                    FROM do_source ORDER BY id;
                SELECT 9 INTO number_value, missing_value;
                IF EXISTS (SELECT 1 FROM do_source WHERE id = 2)
                   AND number_value = 9
                   AND missing_value IS NULL THEN
                    INSERT INTO do_results VALUES (number_value, missing_value);
                END IF;
            END;
            $$
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT first_value, missing_value FROM do_results"
        ),
        vec![vec![Value::Int8(9), Value::Null]]
    );
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('do_sequence')"),
        vec![vec![Value::Int8(2)]]
    );
    session
        .execute(
            r#"CREATE SEQUENCE ordered_do_sequence;
               DO $$ DECLARE selected BIGINT; allocated BIGINT;
               BEGIN
                   SELECT id, nextval('ordered_do_sequence')
                       INTO selected, allocated FROM do_source ORDER BY id LIMIT 1.4;
                   IF selected <> 1 OR allocated <> 1 THEN
                       RAISE EXCEPTION 'unexpected ordered projection';
                   END IF;
               END; $$"#,
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut session, "SELECT nextval('ordered_do_sequence')"),
        vec![vec![Value::Int8(2)]]
    );

    for sql in [
        "DO $$ BEGIN SELECT 1; END; $$",
        "DO $$ BEGIN INSERT INTO do_source VALUES (3) RETURNING id; END; $$",
        "DO $$ BEGIN IF FALSE THEN RAISE EXCEPTION '%'; END IF; END; $$",
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            SqlState::SyntaxError
        );
    }

    for (sql, expected) in [
        (
            "DO $$ DECLARE value BIGINT := 9223372036854775807.5::NUMERIC; BEGIN END; $$",
            SqlState::NumericValueOutOfRange,
        ),
        (
            "DO $$ BEGIN IF FALSE THEN missing := 1; END IF; END; $$",
            SqlState::SyntaxError,
        ),
        (
            "DO $$ BEGIN RAISE EXCEPTION 'failed' USING HINT = NULL; END; $$",
            SqlState::NullValueNotAllowed,
        ),
        (
            "DO LANGUAGE sql $$ SELECT 1 $$",
            SqlState::FeatureNotSupported,
        ),
        ("DO $$ BEGIN RETURN 1; END; $$", SqlState::DatatypeMismatch),
    ] {
        assert_eq!(session.execute(sql).unwrap_err().sqlstate, expected);
    }

    assert_eq!(
        session
            .execute(
                r#"DO $$
                   DECLARE id BIGINT := 9; selected BIGINT;
                   BEGIN SELECT id INTO selected FROM do_source; END;
                   $$"#
            )
            .unwrap_err()
            .sqlstate,
        SqlState::AmbiguousColumn
    );
    for sql in [
        r#"DO $$ DECLARE x BIGINT := 9; selected BIGINT;
           BEGIN SELECT x INTO selected FROM (SELECT 1 AS x) source; END; $$"#,
        r#"DO $$ DECLARE x BIGINT := 9; selected BIGINT;
           BEGIN WITH source AS (SELECT 1 AS x)
                 SELECT x INTO selected FROM source; END; $$"#,
        r#"DO $$ DECLARE x BIGINT := 9;
           BEGIN IF EXISTS (SELECT x FROM (SELECT 1 AS x) source) THEN
               INSERT INTO do_source VALUES (3);
           END IF; END; $$"#,
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            SqlState::AmbiguousColumn
        );
    }
    session
        .execute(
            r#"DO $$ DECLARE id BIGINT := 9; selected BIGINT; other BIGINT;
               BEGIN
                   SELECT (SELECT id), (SELECT 1 FROM do_source LIMIT 1)
                       INTO selected, other;
                   IF selected <> 9 OR other <> 1 THEN
                       RAISE EXCEPTION 'unexpected scoped locals';
                   END IF;
               END; $$"#,
        )
        .unwrap();
    session
        .execute(
            r#"DO $$ DECLARE x BIGINT := 9; selected BIGINT;
               BEGIN
                   SELECT source.z INTO selected FROM (SELECT x AS z) source;
                   IF selected <> 9 THEN RAISE EXCEPTION 'unexpected derived local'; END IF;
                   WITH source AS (SELECT x AS z) SELECT z INTO selected FROM source;
                   IF selected <> 9 THEN RAISE EXCEPTION 'unexpected CTE local'; END IF;
                   SELECT id AS x INTO selected
                       FROM (VALUES (2), (1)) source(id) ORDER BY x;
                   IF selected <> 1 THEN RAISE EXCEPTION 'unexpected alias ordering'; END IF;
                   SELECT id AS x INTO selected
                       FROM (VALUES (2), (1)) source(id) ORDER BY x + 0;
                   IF selected <> 2 THEN RAISE EXCEPTION 'unexpected local ordering'; END IF;
                   SELECT id AS x INTO selected
                       FROM (VALUES (2), (1)) source(id) GROUP BY x ORDER BY x;
                   IF selected <> 1 THEN RAISE EXCEPTION 'unexpected grouped alias'; END IF;
               END; $$"#,
        )
        .unwrap();

    session
        .execute("CREATE TABLE do_ambiguous (id BIGINT, value BIGINT); INSERT INTO do_ambiguous VALUES (1, 1)")
        .unwrap();
    assert_eq!(
        session
            .execute(
                r#"DO $$ DECLARE value BIGINT := 9; selected BIGINT;
                   BEGIN
                       WITH changed AS (
                           UPDATE do_ambiguous SET value = value + 1 RETURNING id
                       ) SELECT id INTO selected FROM changed;
                   END; $$"#,
            )
            .unwrap_err()
            .sqlstate,
        SqlState::AmbiguousColumn
    );
    assert_eq!(
        query_rows(&mut session, "SELECT value FROM do_ambiguous"),
        vec![vec![Value::Int8(1)]]
    );
    assert_eq!(
        session
            .execute("DO LANGUAGE missing_procedural_language $$ BEGIN END; $$")
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedObject
    );
}

#[test]
fn binds_procedural_queries_against_the_active_temporary_schema() {
    let db = Db::create();
    let mut first = db.create_session();
    let mut second = db.create_session();
    first
        .execute(
            "CREATE TABLE procedural_temp_result (value BIGINT); \
             CREATE TEMP TABLE procedural_temp_source (value BIGINT); \
             INSERT INTO procedural_temp_source VALUES (7)",
        )
        .unwrap();
    second
        .execute(
            "CREATE TEMP TABLE procedural_temp_source (label TEXT); \
             INSERT INTO procedural_temp_source VALUES ('other'); \
             SELECT label FROM procedural_temp_source",
        )
        .unwrap();

    first
        .execute(
            r#"DO $$ DECLARE selected BIGINT;
               BEGIN
                   SELECT value INTO selected FROM procedural_temp_source;
                   INSERT INTO procedural_temp_result VALUES (selected);
               END; $$"#,
        )
        .unwrap();
    assert_eq!(
        query_rows(&mut first, "SELECT value FROM procedural_temp_result"),
        vec![vec![Value::Int8(7)]]
    );
}

#[test]
fn fires_before_update_triggers_for_foreign_key_actions() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            r#"
            CREATE TABLE parents (id BIGINT PRIMARY KEY);
            CREATE TABLE children (
                id BIGINT PRIMARY KEY,
                parent_id BIGINT REFERENCES parents(id) ON UPDATE CASCADE ON DELETE SET NULL,
                changes BIGINT NOT NULL
            );
            CREATE FUNCTION count_child_change() RETURNS TRIGGER AS $$
            BEGIN NEW.changes := NEW.changes + 1; RETURN NEW; END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER count_child_change BEFORE UPDATE ON children
                FOR EACH ROW EXECUTE FUNCTION count_child_change();
            INSERT INTO parents VALUES (1), (3);
            INSERT INTO children VALUES (1, 1, 0), (2, 3, 0);
            UPDATE parents SET id = 2 WHERE id = 1;
            DELETE FROM parents WHERE id = 3;
            "#,
        )
        .unwrap();
    assert_eq!(
        query_rows(
            &mut session,
            "SELECT id, parent_id, changes FROM children ORDER BY id"
        ),
        vec![
            vec![Value::Int8(1), Value::Int8(2), Value::Int8(1)],
            vec![Value::Int8(2), Value::Null, Value::Int8(1)],
        ]
    );
}
