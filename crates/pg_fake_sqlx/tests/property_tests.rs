use std::{
    cell::RefCell,
    env,
    path::PathBuf,
    str::FromStr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use bigdecimal::BigDecimal;
use chaos_theory::{Effect, Source, check, make::int_in_range};
use pg_fake::parser::{self, Statement};
use pg_fake_sqlx::{Db, PgFakeConnection};
use postgres::{Client, NoTls, SimpleQueryMessage};
use sqlx::{AssertSqlSafe, Column, Row, ValueRef};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::runtime::Runtime;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows {
        values: Vec<Vec<Option<String>>>,
        type_oids: Option<Vec<u32>>,
    },
    Error(String),
}

#[derive(Clone, Copy)]
enum RowOrder {
    Unordered,
    Ordered,
}

static TEST_LOCK: Mutex<()> = Mutex::new(());
static TABLE_NUMBER: AtomicU64 = AtomicU64::new(1);

struct PostgresServer {
    url: String,
    _container: Option<Container<Postgres>>,
}

struct PostgresCase<'client> {
    client: &'client mut Client,
    table: String,
}

impl PostgresCase<'_> {
    fn client(&mut self) -> &mut Client {
        self.client
    }
}

impl Drop for PostgresCase<'_> {
    fn drop(&mut self) {
        let _ = self.client.batch_execute("ROLLBACK");
        let _ = self
            .client
            .batch_execute(&format!("DROP TABLE IF EXISTS {}", self.table));
    }
}

fn postgres_server() -> PostgresServer {
    if let Ok(url) = env::var("PG_FAKE_DATABASE_URL") {
        return PostgresServer {
            url,
            _container: None,
        };
    }
    if env::var_os("DOCKER_HOST").is_none() {
        let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
            .join(".colima/default/docker.sock");
        if socket.exists() {
            unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
        }
    }
    let container = Postgres::default()
        .with_tag("18")
        .start()
        .expect("must start PostgreSQL 18 container");
    let url = format!(
        "postgresql://postgres:postgres@{}:{}/postgres",
        container
            .get_host()
            .expect("container host must be available"),
        container
            .get_host_port_ipv4(5432)
            .expect("PostgreSQL port must be available")
    );
    PostgresServer {
        url,
        _container: Some(container),
    }
}

fn postgres_outcome(client: &mut Client, statement: &Statement, sql: &str) -> Outcome {
    let messages = match client.simple_query(sql) {
        Ok(messages) => messages,
        Err(error) => {
            return Outcome::Error(
                error
                    .code()
                    .expect("PostgreSQL execution errors must have a SQLSTATE")
                    .code()
                    .into(),
            );
        }
    };
    match statement {
        Statement::Query(_) => Outcome::Rows {
            values: messages
                .iter()
                .filter_map(|message| match message {
                    SimpleQueryMessage::Row(row) => Some(
                        (0..row.len())
                            .map(|index| row.get(index).map(str::to_owned))
                            .collect(),
                    ),
                    _ => None,
                })
                .collect(),
            type_oids: None,
        },
        _ => Outcome::Affected(
            messages
                .iter()
                .filter_map(|message| match message {
                    SimpleQueryMessage::CommandComplete(rows) => Some(*rows),
                    _ => None,
                })
                .last()
                .expect("non-query statements must complete"),
        ),
    }
}

async fn fake_outcome(
    connection: &mut PgFakeConnection,
    statement: &Statement,
    sql: &str,
) -> Outcome {
    match statement {
        Statement::Query(_) => match sqlx::query(AssertSqlSafe(sql)).fetch_all(connection).await {
            Ok(rows) => Outcome::Rows {
                type_oids: Some(
                    rows.first()
                        .map(|row| {
                            row.columns()
                                .iter()
                                .map(|column| {
                                    column
                                        .type_info()
                                        .base
                                        .expect("query columns must have a Phase-1 type")
                                        .oid()
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                values: rows
                    .iter()
                    .map(|row| {
                        (0..row.len())
                            .map(|index| {
                                let value = row.try_get_raw(index).unwrap();
                                if value.is_null() {
                                    None
                                } else {
                                    Some(row.try_get_unchecked::<String, _>(index).unwrap())
                                }
                            })
                            .collect()
                    })
                    .collect(),
            },
            Err(error) => Outcome::Error(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .expect("database execution errors must have a SQLSTATE")
                    .into_owned(),
            ),
        },
        _ => match sqlx::query(AssertSqlSafe(sql)).execute(connection).await {
            Ok(result) => Outcome::Affected(result.rows_affected()),
            Err(error) => Outcome::Error(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .expect("database execution errors must have a SQLSTATE")
                    .into_owned(),
            ),
        },
    }
}

fn assert_statement(
    runtime: &Runtime,
    postgres: &mut Client,
    fake: &mut PgFakeConnection,
    sql: &str,
    row_order: RowOrder,
) {
    let mut statements = parser::parse(sql)
        .unwrap_or_else(|error| panic!("generated SQL must parse: {sql}\n{error}"));
    assert_eq!(
        statements.len(),
        1,
        "generated operation must be one statement"
    );
    let statement = statements.pop().expect("statement count was checked");
    let expected = postgres_outcome(postgres, &statement, sql);
    let actual = runtime.block_on(fake_outcome(fake, &statement, sql));
    match (expected, actual) {
        (
            Outcome::Rows {
                values: mut expected,
                type_oids: None,
            },
            Outcome::Rows {
                values: mut actual,
                type_oids: Some(type_oids),
            },
        ) => {
            normalize_rows(&mut expected, &type_oids);
            normalize_rows(&mut actual, &type_oids);
            if matches!(row_order, RowOrder::Unordered) {
                expected.sort();
                actual.sort();
            }
            assert_eq!(actual, expected, "generated SQL: {sql}");
        }
        (expected, actual) => assert_eq!(actual, expected, "generated SQL: {sql}"),
    }
}

fn normalize_rows(rows: &mut [Vec<Option<String>>], type_oids: &[u32]) {
    for row in rows {
        assert_eq!(row.len(), type_oids.len());
        for (value, type_oid) in row.iter_mut().zip(type_oids) {
            let Some(value) = value else {
                continue;
            };
            *value = match type_oid {
                700 => format!("{:08x}", value.parse::<f32>().unwrap().to_bits()),
                701 => format!("{:016x}", value.parse::<f64>().unwrap().to_bits()),
                1700 => BigDecimal::from_str(value)
                    .unwrap()
                    .normalized()
                    .to_plain_string(),
                _ => continue,
            };
        }
    }
}

fn integer(src: &mut Source, label: &str) -> i32 {
    src.any_of(label, int_in_range(-20..=20))
}

fn decimal_literal(value: i32) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let absolute = value.abs();
    format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
}

fn text_literal(src: &mut Source, label: &str) -> String {
    let values = ["", "a", "MiXeD", "two words", "quote's", "東京"];
    let (value, _) = src
        .choose(label, &values)
        .expect("text choices must not be empty");
    format!("'{}'", value.replace('\'', "''"))
}

fn maybe_null(src: &mut Source, label: &str, value: impl FnOnce(&mut Source) -> String) -> String {
    src.maybe(label, value).unwrap_or_else(|| "NULL".into())
}

fn row(src: &mut Source, row_key: i64) -> String {
    let small_value = maybe_null(src, "small", |src| integer(src, "value").to_string());
    let int_value = maybe_null(src, "int", |src| integer(src, "value").to_string());
    let big_value = maybe_null(src, "big", |src| integer(src, "value").to_string());
    let numeric_value = maybe_null(src, "numeric", |src| {
        decimal_literal(src.any_of("value", int_in_range(-2000..=2000)))
    });
    let real_value = maybe_null(src, "real", |src| {
        decimal_literal(src.any_of("value", int_in_range(-2000..=2000)))
    });
    let double_value = maybe_null(src, "double", |src| {
        decimal_literal(src.any_of("value", int_in_range(-2000..=2000)))
    });
    let flag = maybe_null(src, "flag", |src| {
        if src.any("value") { "TRUE" } else { "FALSE" }.into()
    });
    let text_value = maybe_null(src, "text", |src| text_literal(src, "value"));
    let varchar_value = maybe_null(src, "varchar", |src| text_literal(src, "value"));
    let char_value = maybe_null(src, "char", |src| {
        let values = ["", "x", "fixed", "eight888"];
        let (value, _) = src
            .choose("value", &values)
            .expect("char choices must not be empty");
        format!("'{value}'")
    });
    let bytes = maybe_null(src, "bytes", |src| {
        let bytes = [
            src.any_of("a", int_in_range(0_u8..=255)),
            src.any_of("b", int_in_range(0_u8..=255)),
            src.any_of("c", int_in_range(0_u8..=255)),
            src.any_of("d", int_in_range(0_u8..=255)),
        ];
        format!(
            r"'\x{:02x}{:02x}{:02x}{:02x}'",
            bytes[0], bytes[1], bytes[2], bytes[3]
        )
    });
    format!(
        "({row_key}, {small_value}, {int_value}, {big_value}, {numeric_value}, \
         {real_value}, {double_value}, {flag}, {text_value}, {varchar_value}, \
         {char_value}, {bytes})"
    )
}

fn insert_sql(src: &mut Source, table: &str, next_row_key: &mut i64) -> String {
    src.select(
        "shape",
        &["full", "omitted", "defaults"],
        |src, shape, _| {
            let mut rows = Vec::new();
            src.repeat_n("rows", 1..=4, |src| {
                rows.push(match shape {
                    "full" => row(src, *next_row_key),
                    "omitted" => format!("({})", *next_row_key),
                    "defaults" => format!("({}, DEFAULT, DEFAULT)", *next_row_key),
                    _ => unreachable!(),
                });
                *next_row_key += 1;
                Effect::Success
            });
            match shape {
                "full" => format!("INSERT INTO {table} VALUES {}", rows.join(", ")),
                "omitted" => {
                    format!("INSERT INTO {table} (row_key) VALUES {}", rows.join(", "))
                }
                "defaults" => format!(
                    "INSERT INTO {table} (row_key, int_value, text_value) VALUES {}",
                    rows.join(", ")
                ),
                _ => unreachable!(),
            }
        },
    )
}

fn where_clause(src: &mut Source) -> String {
    src.maybe("where", |src| {
        src.select(
            "predicate",
            &["comparison", "boolean", "null", "distinct", "combined"],
            |src, predicate, _| match predicate {
                "comparison" => format!("int_value >= {}", integer(src, "value")),
                "boolean" => {
                    let values = ["flag", "NOT flag", "flag IS TRUE", "flag IS FALSE"];
                    let (value, _) = src
                        .choose("value", &values)
                        .expect("boolean predicate choices must not be empty");
                    (*value).into()
                }
                "null" => {
                    let columns = ["small_value", "text_value", "bytes"];
                    let (column, _) = src
                        .choose("column", &columns)
                        .expect("nullable columns must not be empty");
                    let operator = if src.any("not") {
                        "IS NOT NULL"
                    } else {
                        "IS NULL"
                    };
                    format!("{column} {operator}")
                }
                "distinct" => format!(
                    "int_value IS {}DISTINCT FROM {}",
                    if src.any("not") { "NOT " } else { "" },
                    integer(src, "value")
                ),
                "combined" => format!(
                    "(int_value < {} OR flag IS TRUE) AND text_value IS NOT NULL",
                    integer(src, "value")
                ),
                _ => unreachable!(),
            },
        )
    })
    .map(|predicate| format!(" WHERE {predicate}"))
    .unwrap_or_default()
}

fn select_expression(src: &mut Source) -> String {
    src.select(
        "expression",
        &[
            "wildcard",
            "column",
            "arithmetic",
            "comparison",
            "boolean",
            "null",
            "case",
            "function",
            "cast",
        ],
        |src, expression, _| match expression {
            "wildcard" => "*".into(),
            "column" => {
                let columns = [
                    "row_key",
                    "small_value",
                    "int_value",
                    "big_value",
                    "numeric_value",
                    "real_value",
                    "double_value",
                    "flag",
                    "text_value",
                    "varchar_value",
                    "char_value",
                    "bytes",
                ];
                let (column, _) = src
                    .choose("column", &columns)
                    .expect("column choices must not be empty");
                (*column).into()
            }
            "arithmetic" => {
                let operators = ["+", "-", "*", "/", "%"];
                let (operator, _) = src
                    .choose("operator", &operators)
                    .expect("arithmetic operators must not be empty");
                let right = if *operator == "/" || *operator == "%" {
                    src.any_of("right", int_in_range(1..=5))
                } else {
                    integer(src, "right")
                };
                format!("int_value {operator} {right}")
            }
            "comparison" => {
                let operators = ["=", "<>", ">", "<", ">=", "<="];
                let (operator, _) = src
                    .choose("operator", &operators)
                    .expect("comparison operators must not be empty");
                format!("numeric_value {operator} {}", integer(src, "right"))
            }
            "boolean" => {
                let values = [
                    "flag AND TRUE",
                    "flag OR FALSE",
                    "NOT flag",
                    "flag IS TRUE",
                    "flag IS FALSE",
                    "flag IS UNKNOWN",
                ];
                let (value, _) = src
                    .choose("value", &values)
                    .expect("boolean expressions must not be empty");
                (*value).into()
            }
            "null" => {
                let values = [
                    "text_value IS NULL",
                    "text_value IS NOT NULL",
                    "int_value IS DISTINCT FROM small_value",
                    "int_value IS NOT DISTINCT FROM small_value",
                    "int_value + NULL",
                    "int_value = NULL",
                ];
                let (value, _) = src
                    .choose("value", &values)
                    .expect("null expressions must not be empty");
                (*value).into()
            }
            "case" => {
                if src.any("simple") {
                    format!(
                        "CASE int_value WHEN {} THEN 'match' ELSE text_value END",
                        integer(src, "value")
                    )
                } else {
                    format!(
                        "CASE WHEN int_value > {} THEN big_value WHEN flag IS TRUE THEN 0 ELSE small_value END",
                        integer(src, "value")
                    )
                }
            }
            "function" => {
                let values = [
                    "COALESCE(text_value, varchar_value, 'fallback')",
                    "NULLIF(int_value, small_value)",
                    "GREATEST(int_value, small_value, 0)",
                    "LEAST(big_value, int_value, 0)",
                    "length(text_value)",
                    "lower(varchar_value)",
                    "upper(text_value)",
                    "abs(numeric_value)",
                ];
                let (value, _) = src
                    .choose("value", &values)
                    .expect("function expressions must not be empty");
                (*value).into()
            }
            "cast" => {
                let values = [
                    "small_value::BIGINT",
                    "CAST(int_value AS TEXT)",
                    "CAST(flag AS INTEGER)",
                    "CAST(int_value AS BOOLEAN)",
                    "int_value::BYTEA::INTEGER",
                    "CAST(text_value AS VARCHAR(6))",
                    "CAST(numeric_value AS INTEGER)",
                    "CAST(real_value AS DOUBLE PRECISION)",
                ];
                let (value, _) = src
                    .choose("value", &values)
                    .expect("cast expressions must not be empty");
                (*value).into()
            }
            _ => unreachable!(),
        },
    )
}

fn row_count(src: &mut Source, ordered: bool, offset: bool) -> String {
    let choices: &[&str] = match (ordered, offset) {
        (true, _) => &["null", "zero", "small", "beyond"],
        (false, true) => &["null", "zero"],
        (false, false) => &["null", "beyond"],
    };
    src.select("value", choices, |src, value, _| match value {
        "null" => "NULL".into(),
        "zero" => "0".into(),
        "small" => src.any_of("count", int_in_range(1..=8)).to_string(),
        "beyond" => "1000".into(),
        _ => unreachable!(),
    })
}

fn select_sql(src: &mut Source, table: &str) -> (String, RowOrder) {
    let mut projections = vec!["row_key".into()];
    src.repeat_n("projections", 1..=4, |src| {
        projections.push(select_expression(src));
        Effect::Success
    });
    let mut sql = format!(
        "SELECT {} FROM {table}{}",
        projections.join(", "),
        where_clause(src)
    );
    let ordered = src.maybe("order", |src| {
        let keys = [
            "1",
            "numeric_value + int_value",
            "length(text_value)",
            "flag IS TRUE",
        ];
        let (key, _) = src
            .choose("key", &keys)
            .expect("order keys must not be empty");
        let direction = if src.any("descending") { "DESC" } else { "ASC" };
        let nulls = if src.any("nulls_first") {
            "NULLS FIRST"
        } else {
            "NULLS LAST"
        };
        format!(" ORDER BY {key} {direction} {nulls}, row_key + 0")
    });
    let row_order = if let Some(order) = ordered {
        sql.push_str(&order);
        RowOrder::Ordered
    } else {
        RowOrder::Unordered
    };
    if let Some(limit) = src.maybe("limit", |src| {
        row_count(src, matches!(row_order, RowOrder::Ordered), false)
    }) {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    if let Some(offset) = src.maybe("offset", |src| {
        row_count(src, matches!(row_order, RowOrder::Ordered), true)
    }) {
        sql.push_str(&format!(" OFFSET {offset}"));
    }
    if let Some(lock) = src.maybe("row_lock", |src| {
        let locks = ["FOR UPDATE", "FOR SHARE"];
        let (lock, _) = src
            .choose("mode", &locks)
            .expect("row locks must not be empty");
        *lock
    }) {
        sql.push_str(&format!(" {lock}"));
    }
    (sql, row_order)
}

fn update_sql(src: &mut Source, table: &str) -> String {
    let assignment = src.select(
        "assignment",
        &[
            "multiple", "key", "default", "small", "int", "big", "numeric", "real", "double",
            "flag", "text", "varchar", "char", "bytes",
        ],
        |src, assignment, _| match assignment {
            "multiple" => "int_value = small_value, small_value = int_value".into(),
            "key" => "row_key = row_key + 1000000".into(),
            "default" => {
                "int_value = DEFAULT, numeric_value = DEFAULT, text_value = DEFAULT".into()
            }
            "small" => format!("small_value = {}", integer(src, "value")),
            "int" => format!("int_value = int_value + {}", integer(src, "value")),
            "big" => format!(
                "big_value = COALESCE(big_value, 0) - {}",
                integer(src, "value")
            ),
            "numeric" => format!(
                "numeric_value = {}",
                decimal_literal(src.any_of("value", int_in_range(-2000..=2000)))
            ),
            "real" => format!("real_value = {}", integer(src, "value")),
            "double" => format!("double_value = {}", integer(src, "value")),
            "flag" => "flag = NOT flag".into(),
            "text" => "text_value = upper(COALESCE(text_value, 'fallback'))".into(),
            "varchar" => "varchar_value = CAST(COALESCE(text_value, '') AS VARCHAR(12))".into(),
            "char" => "char_value = CAST(COALESCE(varchar_value, '') AS CHAR(8))".into(),
            "bytes" => "bytes = int_value::BYTEA".into(),
            _ => unreachable!(),
        },
    );
    format!("UPDATE {table} SET {assignment}{}", where_clause(src))
}

fn create_table_sql(table: &str) -> String {
    format!(
        "CREATE TABLE {table} (\
             row_key BIGINT NOT NULL PRIMARY KEY, \
             small_value SMALLINT CHECK (small_value IS NULL OR small_value >= -10), \
             int_value INTEGER DEFAULT 0, \
             big_value BIGINT, \
             numeric_value NUMERIC(8, 2) DEFAULT 1 + 2, \
             real_value REAL, \
             double_value DOUBLE PRECISION, \
             flag BOOLEAN, \
             text_value TEXT DEFAULT upper('default'), \
             varchar_value VARCHAR(12), \
             char_value CHAR(8), \
             bytes BYTEA, \
             UNIQUE (row_key, int_value), \
             CHECK (int_value IS NULL OR big_value IS NULL OR int_value <= big_value)\
         )"
    )
}

fn isolation_level(src: &mut Source) -> &'static str {
    let levels = ["READ COMMITTED", "REPEATABLE READ"];
    let (level, _) = src
        .choose("isolation", &levels)
        .expect("isolation levels must not be empty");
    level
}

fn lock_timeout_sql(src: &mut Source) -> String {
    src.select(
        "value",
        &["zero", "integer", "milliseconds", "seconds"],
        |src, value, _| match value {
            "zero" => "SET lock_timeout = 0".into(),
            "integer" => format!(
                "SET lock_timeout = {}",
                src.any_of("milliseconds", int_in_range(1..=1000))
            ),
            "milliseconds" => format!(
                "SET lock_timeout = '{}ms'",
                src.any_of("milliseconds", int_in_range(1..=1000))
            ),
            "seconds" => format!(
                "SET lock_timeout = '{}s'",
                src.any_of("seconds", int_in_range(1..=3))
            ),
            _ => unreachable!(),
        },
    )
}

#[test]
fn generated_sql_matches_postgres() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = postgres_server();
    let postgres = RefCell::new(
        Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL 18 once"),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    check(|src| {
        let table = format!(
            "pg_fake_property_{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let mut postgres = postgres.borrow_mut();
        let mut postgres = PostgresCase {
            client: &mut postgres,
            table: table.clone(),
        };
        let mut fake = PgFakeConnection::new(Db::new());
        let mut next_row_key = 1;
        let mut in_transaction = false;
        let foreign_parent = format!("{table}_foreign_parent");
        let foreign_child = format!("{table}_foreign_child");

        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL READ COMMITTED",
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            "SET lock_timeout = 1000",
            RowOrder::Unordered,
        );
        let create = create_table_sql(&table);
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &create,
            RowOrder::Unordered,
        );
        let insert = insert_sql(src, &table, &mut next_row_key);
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &insert,
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT left_row.row_key, right_row.row_key FROM {table} AS left_row INNER JOIN {table} AS right_row ON left_row.row_key = right_row.row_key ORDER BY left_row.row_key"
            ),
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT left_row.row_key, right_row.row_key FROM {table} AS left_row CROSS JOIN {table} AS right_row WHERE left_row.row_key = right_row.row_key ORDER BY left_row.row_key"
            ),
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT left_row.row_key, right_row.row_key FROM {table} AS left_row FULL JOIN {table} AS right_row ON left_row.row_key = right_row.row_key + 1000000 ORDER BY 1, 2"
            ),
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT source.row_key FROM (SELECT row_key FROM {table}) AS source ORDER BY source.row_key"
            ),
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT row_key FROM {table} WHERE row_key = (SELECT row_key FROM {table} ORDER BY row_key LIMIT 1)"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT row_key FROM {table} WHERE row_key IN (SELECT row_key FROM {table}) ORDER BY row_key"
            ),
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT EXISTS (SELECT 1 FROM {table}), 1000000 = ANY (SELECT row_key FROM {table}), 1000000 > ALL (SELECT row_key FROM {table})"
            ),
            RowOrder::Ordered,
        );
        let correlation_threshold = integer(src, "correlation_threshold");
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "SELECT outer_row.row_key, \
                    EXISTS (SELECT 1 FROM {table} AS inner_row \
                        WHERE inner_row.row_key = outer_row.row_key \
                          AND inner_row.int_value IS NOT DISTINCT FROM outer_row.int_value), \
                    outer_row.row_key IN (SELECT inner_row.row_key FROM {table} AS inner_row \
                        WHERE inner_row.int_value > outer_row.int_value + {correlation_threshold}), \
                    EXISTS (SELECT 1 FROM {table} AS middle_row \
                        WHERE middle_row.row_key = outer_row.row_key \
                          AND EXISTS (SELECT 1 WHERE middle_row.int_value IS NOT DISTINCT FROM outer_row.int_value)) \
                 FROM {table} AS outer_row ORDER BY outer_row.row_key"
            ),
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!("CREATE TABLE {foreign_parent} (id INTEGER PRIMARY KEY)"),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!(
                "CREATE TABLE {foreign_child} (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES {foreign_parent})"
            ),
            RowOrder::Unordered,
        );
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!("INSERT INTO {foreign_parent} VALUES (1)"),
            RowOrder::Unordered,
        );
        let foreign_key = if src.any("foreign_key_exists") {
            1
        } else {
            integer(src, "foreign_key")
        };
        assert_statement(
            &runtime,
            postgres.client(),
            &mut fake,
            &format!("INSERT INTO {foreign_child} VALUES (1, {foreign_key})"),
            RowOrder::Unordered,
        );

        src.repeat_n("statements", 3..=14, |src| {
            let actions: &[&'static str] = if in_transaction {
                &[
                    "insert",
                    "select",
                    "update",
                    "delete",
                    "set_transaction",
                    "set_lock_timeout",
                    "commit",
                    "rollback",
                ]
            } else {
                &[
                    "insert",
                    "select",
                    "update",
                    "delete",
                    "set_session",
                    "set_lock_timeout",
                    "begin",
                ]
            };
            src.select("action", actions, |src, action, _| {
                let (sql, order) = match action {
                    "insert" => (
                        insert_sql(src, &table, &mut next_row_key),
                        RowOrder::Unordered,
                    ),
                    "select" => select_sql(src, &table),
                    "update" => (update_sql(src, &table), RowOrder::Unordered),
                    "delete" => (
                        format!("DELETE FROM {table}{}", where_clause(src)),
                        RowOrder::Unordered,
                    ),
                    "begin" => {
                        in_transaction = true;
                        let sql = if src.any("explicit_isolation") {
                            format!("BEGIN ISOLATION LEVEL {}", isolation_level(src))
                        } else {
                            "BEGIN".into()
                        };
                        (sql, RowOrder::Unordered)
                    }
                    "set_transaction" => (
                        format!("SET TRANSACTION ISOLATION LEVEL {}", isolation_level(src)),
                        RowOrder::Unordered,
                    ),
                    "set_session" => (
                        format!(
                            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {}",
                            isolation_level(src)
                        ),
                        RowOrder::Unordered,
                    ),
                    "set_lock_timeout" => (lock_timeout_sql(src), RowOrder::Unordered),
                    "commit" => {
                        in_transaction = false;
                        ("COMMIT".into(), RowOrder::Unordered)
                    }
                    "rollback" => {
                        in_transaction = false;
                        ("ROLLBACK".into(), RowOrder::Unordered)
                    }
                    _ => unreachable!(),
                };
                src.log_value("sql", &sql);
                assert_statement(&runtime, postgres.client(), &mut fake, &sql, order);
                Effect::Success
            })
        });

        if in_transaction {
            assert_statement(
                &runtime,
                postgres.client(),
                &mut fake,
                "ROLLBACK",
                RowOrder::Unordered,
            );
        }
        postgres
            .client()
            .batch_execute(&format!(
                "DROP TABLE {foreign_child}; DROP TABLE {foreign_parent}"
            ))
            .unwrap();
    });
}
