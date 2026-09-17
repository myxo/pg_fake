use bigdecimal::BigDecimal;
use pg_fake::{
    Db,
    error::SqlState,
    value::{BaseType, PgLsn, Value},
};

#[test]
fn reports_a_deterministic_primary_server() {
    let db = Db::create();
    let mut session = db.create_session();
    let result = session
        .query(
            "SELECT pg_is_in_recovery(), pg_catalog.pg_is_in_recovery()",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::Bool(false), Value::Bool(false)]]
    );
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.type_oid)
            .collect::<Vec<_>>(),
        vec![BaseType::Bool.map_to_oid(), BaseType::Bool.map_to_oid()]
    );
}

#[test]
fn parses_compares_aggregates_and_offsets_pg_lsn_values() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE positions (value pg_lsn); \
             INSERT INTO positions VALUES ('0/0'), ('0/16AE7F8'), ('FFFFFFFF/FFFFFFFF')",
        )
        .unwrap();
    let result = session
        .query(
            "SELECT min(value), max(value), \
                    '0/16AE7F8'::pg_lsn - '0/16AE7F7'::pg_lsn, \
                    '0/16AE7F7'::pg_lsn + 16::numeric FROM positions",
            &[],
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::PgLsn(PgLsn(0)),
            Value::PgLsn(PgLsn(u64::MAX)),
            Value::Numeric(BigDecimal::from(1)),
            Value::PgLsn(PgLsn(0x16ae807)),
        ]]
    );
    assert_eq!(result.columns[0].type_oid, BaseType::PgLsn.map_to_oid());
    assert_eq!(
        session
            .execute("INSERT INTO positions VALUES ('G/0')")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidTextRepresentation
    );
}

#[test]
fn resolves_regclass_and_exposes_foundational_catalog_rows() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE catalog_items (id SERIAL PRIMARY KEY, name VARCHAR(12) NOT NULL); \
             CREATE INDEX catalog_items_name_idx ON catalog_items (name)",
        )
        .unwrap();

    assert_eq!(
        session
            .query(
                "SELECT to_regclass('catalog_items')::text, \
                        to_regclass('catalog_items_name_idx')::text, \
                        to_regclass('missing_relation')",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Text("catalog_items".into()),
            Value::Text("catalog_items_name_idx".into()),
            Value::Null,
        ]]
    );

    assert_eq!(
        session
            .query(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull \
                 FROM pg_catalog.pg_attribute AS a \
                 WHERE a.attrelid = 'catalog_items'::regclass \
                 ORDER BY a.attnum",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![
                Value::Text("id".into()),
                Value::Text("integer".into()),
                Value::Bool(true),
            ],
            vec![
                Value::Text("name".into()),
                Value::Text("character varying(12)".into()),
                Value::Bool(true),
            ],
        ]
    );
    assert_eq!(
        session
            .query(
                "SELECT c.relname, n.nspname, c.relkind \
                 FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace \
                 WHERE c.oid = 'catalog_items'::regclass",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Text("catalog_items".into()),
            Value::Text("public".into()),
            Value::Text("r".into()),
        ]]
    );

    let oid = session
        .query("SELECT 'catalog_items'::regclass", &[])
        .unwrap()
        .rows[0][0]
        .clone();
    session
        .execute("ALTER TABLE catalog_items RENAME TO renamed_items")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT 'renamed_items'::regclass", &[])
            .unwrap()
            .rows[0][0],
        oid
    );
    assert_eq!(
        session
            .query("SELECT to_regclass('renamed_items')::text", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Text("renamed_items".into())]]
    );
}

#[test]
fn gives_regclass_transactional_catalog_visibility() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("BEGIN; CREATE TABLE transient_catalog_item (id INTEGER)")
        .unwrap();
    assert!(
        !session
            .query("SELECT to_regclass('transient_catalog_item')", &[])
            .unwrap()
            .rows[0][0]
            .is_null()
    );
    session.execute("ROLLBACK").unwrap();
    assert_eq!(
        session
            .query("SELECT to_regclass('transient_catalog_item')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        session
            .query("SELECT 'transient_catalog_item'::regclass", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
}

#[test]
fn applies_default_and_temporary_regclass_lookup_rules() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE public.shadowed_catalog_item (id INTEGER); \
             CREATE TEMP TABLE shadowed_catalog_item (id INTEGER)",
        )
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT to_regclass('shadowed_catalog_item') = \
                        to_regclass('pg_temp.shadowed_catalog_item'), \
                        to_regclass('public.shadowed_catalog_item') IS NOT NULL, \
                        to_regclass('public.shadowed_catalog_item')::text",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Text("public.shadowed_catalog_item".into()),
        ]]
    );
}

#[test]
fn rejects_catalog_and_server_management_surface_outside_the_boundary() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute("CREATE VIEW catalog_boundary_view AS SELECT 1 AS id")
        .unwrap();
    for (sql, expected) in [
        (
            "SELECT unsupported_column FROM pg_catalog.pg_class",
            SqlState::UndefinedColumn,
        ),
        (
            "SELECT pg_catalog.pg_get_viewdef('catalog_boundary_view'::regclass)",
            SqlState::UndefinedFunction,
        ),
        (
            "SELECT * FROM information_schema.tables",
            SqlState::InvalidSchemaName,
        ),
        (
            "CREATE DATABASE outside_harness",
            SqlState::FeatureNotSupported,
        ),
        (
            "DROP DATABASE outside_harness",
            SqlState::FeatureNotSupported,
        ),
    ] {
        assert_eq!(
            session.query(sql, &[]).unwrap_err().sqlstate,
            expected,
            "SQL: {sql}"
        );
    }
}

#[test]
fn covers_regclass_catalog_edge_cases() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE public.path_item (id INTEGER PRIMARY KEY); \
             CREATE TEMP TABLE path_item (id INTEGER PRIMARY KEY); \
             CREATE TEMP VIEW temporary_catalog_view AS SELECT 1 AS id; \
             CREATE TEMP SEQUENCE temporary_catalog_sequence",
        )
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT to_regclass('path_item_pkey') IS NOT NULL, \
                        '4294967295'::regclass::text, \
                        format_type(1043, 0), \
                        (SELECT typarray FROM pg_type WHERE oid = 23)",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Bool(true),
            Value::Text("4294967295".into()),
            Value::Text("character varying".into()),
            Value::Oid(1007),
        ]]
    );
    session
        .execute("SET search_path TO public, pg_temp")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT to_regclass('path_item') = to_regclass('public.path_item'), \
                        to_regclass('path_item_pkey') = to_regclass('public.path_item_pkey')",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Bool(true), Value::Bool(true)]]
    );
    session
        .execute("CREATE TEMP TABLE pg_class (marker INTEGER); INSERT INTO pg_class VALUES (7)")
        .unwrap();
    session
        .execute("SET search_path TO pg_temp, pg_catalog, public")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT marker FROM pg_class", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(7)]]
    );
    assert_eq!(
        session
            .query(
                "SELECT to_regclass('path_item_pkey') = \
                        to_regclass('pg_temp.path_item_pkey')",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Bool(true)]]
    );
    assert_eq!(
        session
            .query(
                "SELECT relname, relpersistence FROM pg_catalog.pg_class \
                 WHERE relname IN ('temporary_catalog_view', 'temporary_catalog_sequence') \
                 ORDER BY relname",
                &[],
            )
            .unwrap()
            .rows,
        vec![
            vec![
                Value::Text("temporary_catalog_sequence".into()),
                Value::Text("t".into())
            ],
            vec![
                Value::Text("temporary_catalog_view".into()),
                Value::Text("t".into())
            ],
        ]
    );
    assert_eq!(
        session
            .query("SELECT '0/0'::pg_lsn + 18446744073709551616::numeric", &[],)
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidParameterValue
    );
}

#[test]
fn matches_relation_namespace_and_regclass_formatting_edges() {
    let db = Db::create();
    let mut session = db.create_session();
    session
        .execute(
            "CREATE TABLE public.shared_name (id INTEGER); \
             CREATE TEMP TABLE later_owner \
             (id INTEGER CONSTRAINT shared_name PRIMARY KEY); \
             SET search_path TO public, pg_temp",
        )
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT to_regclass('shared_name') = to_regclass('public.shared_name')",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Bool(true)]]
    );

    session.execute("SET search_path TO public").unwrap();
    session
        .execute(
            "CREATE TABLE constraint_owner \
             (id INTEGER CONSTRAINT occupied_name PRIMARY KEY); \
             CREATE TABLE occupied_addition (id INTEGER); \
             CREATE TABLE \"select\" (id INTEGER); \
             CREATE TABLE \" padded \" (id INTEGER); \
             CREATE TABLE \"foo-bar\" (id INTEGER); \
             CREATE TABLE \"foo bar\" (id INTEGER); \
             CREATE TEMP TABLE pg_class (marker INTEGER); \
             INSERT INTO pg_class VALUES (7)",
        )
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT marker, \
                        'pg_catalog.pg_class'::regclass::text, \
                        '\"select\"'::regclass::text, \
                        '\" padded \"'::regclass::text, \
                        to_regclass('foo-bar')::text, \
                        to_regclass('foo bar'), \
                        to_regclass('a..b'), \
                        '-'::regclass::text, \
                        '0'::regclass::text, \
                        to_regclass('4294967296'), \
                        to_regclass('missing_schema.item'), \
                        format_type(1700, 657410) \
                 FROM pg_class",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Int4(7),
            Value::Text("pg_catalog.pg_class".into()),
            Value::Text("\"select\"".into()),
            Value::Text("\" padded \"".into()),
            Value::Text("\"foo-bar\"".into()),
            Value::Null,
            Value::Null,
            Value::Text("-".into()),
            Value::Text("-".into()),
            Value::Null,
            Value::Null,
            Value::Text("numeric(10,-2)".into()),
        ]]
    );

    for sql in [
        "CREATE TABLE occupied_name (id INTEGER)",
        "CREATE INDEX occupied_name ON constraint_owner (id)",
        "ALTER TABLE constraint_owner ADD CONSTRAINT occupied_addition UNIQUE (id)",
        "ALTER TABLE constraint_owner ADD COLUMN extra INTEGER \
         CONSTRAINT occupied_addition UNIQUE",
        "CREATE TABLE self_collision (id INTEGER CONSTRAINT self_collision PRIMARY KEY)",
    ] {
        assert_eq!(
            session.execute(sql).unwrap_err().sqlstate,
            SqlState::DuplicateTable,
            "SQL: {sql}"
        );
    }
    assert_eq!(
        session
            .query("SELECT 'missing_schema.item'::regclass", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::UndefinedTable
    );
    for malformed in ["SELECT 'a..b'::regclass", "SELECT 'foo bar'::regclass"] {
        assert_eq!(
            session.query(malformed, &[]).unwrap_err().sqlstate,
            SqlState::InvalidName,
            "SQL: {malformed}"
        );
    }
    assert_eq!(
        session
            .query("SELECT '4294967296'::regclass", &[])
            .unwrap_err()
            .sqlstate,
        SqlState::NumericValueOutOfRange
    );
    assert_eq!(
        session
            .query(
                "SELECT typcategory FROM pg_catalog.pg_type WHERE typname = 'interval'",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Text("T".into())]]
    );

    session
        .execute(
            "CREATE TABLE generated_item_pkey (id INTEGER); \
             CREATE TABLE generated_item (id INTEGER PRIMARY KEY); \
             CREATE TABLE altered_item_pkey (id INTEGER); \
             CREATE TABLE altered_item (id INTEGER); \
             ALTER TABLE altered_item ADD PRIMARY KEY (id); \
             CREATE TABLE column_item_extra_key (id INTEGER); \
             CREATE TABLE column_item (id INTEGER); \
             ALTER TABLE column_item ADD COLUMN extra INTEGER UNIQUE",
        )
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT to_regclass('generated_item_pkey1') IS NOT NULL, \
                        to_regclass('altered_item_pkey1') IS NOT NULL, \
                        to_regclass('column_item_extra_key1') IS NOT NULL",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true)
        ]]
    );

    session
        .execute("SET search_path TO nonexistent_schema")
        .unwrap();
    assert_eq!(
        session
            .query("SELECT to_regclass('missing_item')", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        session
            .execute("CREATE TABLE no_schema_selected (id INTEGER)")
            .unwrap_err()
            .sqlstate,
        SqlState::InvalidSchemaName
    );

    session.execute("SET search_path TO pg_temp").unwrap();
    session
        .execute("CREATE TABLE path_temporary (id INTEGER)")
        .unwrap();
    assert_eq!(
        session
            .query(
                "SELECT relpersistence FROM pg_catalog.pg_class \
                 WHERE oid = 'pg_temp.path_temporary'::regclass",
                &[],
            )
            .unwrap()
            .rows,
        vec![vec![Value::Text("t".into())]]
    );
}

#[test]
fn isolates_search_path_between_sessions() {
    let db = Db::create();
    let mut temporary_first = db.create_session();
    let mut public_first = db.create_session();
    temporary_first
        .execute(
            "CREATE TABLE public.session_path_item (marker INTEGER); \
             INSERT INTO public.session_path_item VALUES (0); \
             CREATE TEMP TABLE session_path_item (marker INTEGER); \
             INSERT INTO pg_temp.session_path_item VALUES (1); \
             SET search_path TO pg_temp, public",
        )
        .unwrap();
    public_first
        .execute(
            "CREATE TEMP TABLE session_path_item (marker INTEGER); \
             INSERT INTO pg_temp.session_path_item VALUES (2); \
             SET search_path TO public, pg_temp",
        )
        .unwrap();

    assert_eq!(
        temporary_first
            .query("SELECT marker FROM session_path_item", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    assert_eq!(
        public_first
            .query("SELECT marker FROM session_path_item", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(0)]]
    );
    assert_eq!(
        temporary_first
            .query("SELECT marker FROM session_path_item", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(1)]]
    );
    public_first
        .execute("BEGIN; SET LOCAL search_path TO pg_temp, public")
        .unwrap();
    assert_eq!(
        public_first
            .query("SELECT marker FROM session_path_item", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(2)]]
    );
    public_first.execute("COMMIT").unwrap();
    assert_eq!(
        public_first
            .query("SELECT marker FROM session_path_item", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(0)]]
    );
    public_first
        .execute("BEGIN; SET search_path TO pg_temp, public; ROLLBACK")
        .unwrap();
    assert_eq!(
        public_first
            .query("SELECT marker FROM session_path_item", &[])
            .unwrap()
            .rows,
        vec![vec![Value::Int4(0)]]
    );
}
