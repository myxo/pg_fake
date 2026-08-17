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
use pg_fake_sqlx::{Db, PgFake, PgFakeConnection};
use sqlx::{
    AssertSqlSafe, Column, ColumnIndex, Connection, Database, Decode, Executor, Row, Type,
    TypeInfo, ValueRef,
};
use sqlx_postgres::{PgConnection, Postgres};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres as PostgresImage;
use tokio::runtime::Runtime;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows(Vec<Vec<Option<String>>>),
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
    _container: Option<Container<PostgresImage>>,
}

struct PostgresCase<'connection, 'runtime> {
    connection: &'connection mut PgConnection,
    runtime: &'runtime Runtime,
    table: String,
}

impl PostgresCase<'_, '_> {
    fn get_connection(&mut self) -> &mut PgConnection {
        self.connection
    }
}

impl Drop for PostgresCase<'_, '_> {
    fn drop(&mut self) {
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(AssertSqlSafe("ROLLBACK")).execute(&mut *self.connection));
        let sql = format!(
            "DROP TABLE IF EXISTS {0}_foreign_child, {0}_foreign_parent, {0}",
            self.table
        );
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut *self.connection));
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
    let container = PostgresImage::default()
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

enum TestConnection<'connection> {
    Fake(&'connection mut PgFakeConnection),
    Postgres(&'connection mut PgConnection),
}

impl TestConnection<'_> {
    fn execute(&mut self, runtime: &Runtime, statement: &Statement, sql: &str) -> Outcome {
        match self {
            Self::Fake(connection) => runtime.block_on(execute_sqlx::<PgFake>(
                connection,
                statement,
                sql,
                |result| result.rows_affected(),
            )),
            Self::Postgres(connection) => runtime.block_on(execute_sqlx::<Postgres>(
                connection,
                statement,
                sql,
                |result| result.rows_affected(),
            )),
        }
    }
}

async fn execute_sqlx<DB>(
    connection: &mut DB::Connection,
    statement: &Statement,
    sql: &str,
    rows_affected: impl FnOnce(DB::QueryResult) -> u64,
) -> Outcome
where
    DB: Database,
    for<'connection> &'connection mut DB::Connection: Executor<'connection, Database = DB>,
    for<'row> String: Decode<'row, DB> + Type<DB>,
    usize: ColumnIndex<DB::Row>,
{
    match statement {
        Statement::Query(_) => match sqlx::raw_sql(AssertSqlSafe(sql))
            .fetch_all(&mut *connection)
            .await
        {
            Ok(rows) => {
                let column_types = rows
                    .first()
                    .map(|row| {
                        row.columns()
                            .iter()
                            .map(|column| column.type_info().name().to_owned())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut values = rows
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
                    .collect::<Vec<_>>();
                normalize_rows(&mut values, &column_types);
                Outcome::Rows(values)
            }
            Err(error) => make_error_outcome(error),
        },
        _ => match sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&mut *connection)
            .await
        {
            Ok(result) => Outcome::Affected(rows_affected(result)),
            Err(error) => make_error_outcome(error),
        },
    }
}

fn make_error_outcome(error: sqlx::Error) -> Outcome {
    Outcome::Error(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .expect("database execution errors must have a SQLSTATE")
            .into_owned(),
    )
}

fn assert_statement(
    runtime: &Runtime,
    postgres: &mut PgConnection,
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
    let [expected, actual] = [
        TestConnection::Postgres(postgres),
        TestConnection::Fake(fake),
    ]
    .map(|mut connection| connection.execute(runtime, &statement, sql));
    if let Outcome::Error(sqlstate) = &expected {
        panic!("generator produced invalid SQL ({sqlstate}): {sql}");
    }
    match (expected, actual) {
        (Outcome::Rows(mut expected), Outcome::Rows(mut actual)) => {
            if matches!(row_order, RowOrder::Unordered) {
                expected.sort();
                actual.sort();
            }
            assert_eq!(actual, expected, "generated SQL: {sql}");
        }
        (expected, actual) => assert_eq!(actual, expected, "generated SQL: {sql}"),
    }
}

fn normalize_rows(rows: &mut [Vec<Option<String>>], column_types: &[String]) {
    for row in rows {
        assert_eq!(row.len(), column_types.len());
        for (value, column_type) in row.iter_mut().zip(column_types) {
            let Some(value) = value else {
                continue;
            };
            *value = match column_type.as_str() {
                "FLOAT4" => format!("{:08x}", value.parse::<f32>().unwrap().to_bits()),
                "FLOAT8" => format!("{:016x}", value.parse::<f64>().unwrap().to_bits()),
                "NUMERIC" => BigDecimal::from_str(value)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SqlType {
    SmallInt,
    Integer,
    BigInt,
    Numeric,
    Real,
    Double,
    Boolean,
    Text,
    Varchar,
    Char,
    Bytea,
}

impl SqlType {
    fn sql(self) -> &'static str {
        match self {
            Self::SmallInt => "SMALLINT",
            Self::Integer => "INTEGER",
            Self::BigInt => "BIGINT",
            Self::Numeric => "NUMERIC(8, 2)",
            Self::Real => "REAL",
            Self::Double => "DOUBLE PRECISION",
            Self::Boolean => "BOOLEAN",
            Self::Text => "TEXT",
            Self::Varchar => "VARCHAR(12)",
            Self::Char => "CHAR(8)",
            Self::Bytea => "BYTEA",
        }
    }

    fn is_numeric(self) -> bool {
        matches!(
            self,
            Self::SmallInt
                | Self::Integer
                | Self::BigInt
                | Self::Numeric
                | Self::Real
                | Self::Double
        )
    }

    fn is_integral(self) -> bool {
        matches!(self, Self::SmallInt | Self::Integer | Self::BigInt)
    }

    fn supports_text_functions(self) -> bool {
        matches!(self, Self::Text | Self::Varchar)
    }
}

#[derive(Debug)]
struct ColumnSchema {
    name: String,
    data_type: SqlType,
    nullable: bool,
    default: Option<String>,
}

#[derive(Debug)]
struct TableSchema {
    name: String,
    columns: Vec<ColumnSchema>,
    check_key_positive: bool,
    checked_column: Option<usize>,
    unique_column: Option<usize>,
}

impl TableSchema {
    fn key(&self) -> &ColumnSchema {
        self.columns
            .first()
            .expect("generated tables must have a key")
    }

    fn create_sql(&self) -> String {
        let mut definitions = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let mut definition = format!("{} {}", column.name, column.data_type.sql());
                if index == 0 {
                    definition.push_str(" PRIMARY KEY");
                } else {
                    if !column.nullable {
                        definition.push_str(" NOT NULL");
                    }
                    if let Some(default) = &column.default {
                        definition.push_str(&format!(" DEFAULT {default}"));
                    }
                }
                definition
            })
            .collect::<Vec<_>>();
        if self.check_key_positive {
            definitions.push(format!("CHECK ({} > 0)", self.key().name));
        }
        if let Some(index) = self.checked_column {
            let column = &self.columns[index];
            definitions.push(format!(
                "CHECK ({} IS NULL OR ({} >= -100 AND {} <= 100))",
                column.name, column.name, column.name
            ));
        }
        if let Some(index) = self.unique_column {
            definitions.push(format!(
                "UNIQUE ({}, {})",
                self.key().name,
                self.columns[index].name
            ));
        }
        format!("CREATE TABLE {} ({})", self.name, definitions.join(", "))
    }
}

#[derive(Debug)]
struct ForeignTables {
    parent: String,
    child: String,
    nullable: bool,
    default_parent: bool,
    inline_reference: bool,
    on_delete: &'static str,
    on_update: &'static str,
}

impl ForeignTables {
    fn create_parent_sql(&self) -> String {
        format!("CREATE TABLE {} (id BIGINT PRIMARY KEY)", self.parent)
    }

    fn create_child_sql(&self) -> String {
        let nullability = if self.nullable { "" } else { " NOT NULL" };
        let default = if self.default_parent {
            " DEFAULT 1"
        } else {
            ""
        };
        let reference = format!(
            "REFERENCES {} (id) ON DELETE {} ON UPDATE {}",
            self.parent, self.on_delete, self.on_update
        );
        if self.inline_reference {
            format!(
                "CREATE TABLE {} (id BIGINT PRIMARY KEY, parent_id BIGINT{nullability}{default} {reference})",
                self.child
            )
        } else {
            format!(
                "CREATE TABLE {} (id BIGINT PRIMARY KEY, parent_id BIGINT{nullability}{default}, \
                 FOREIGN KEY (parent_id) {reference})",
                self.child
            )
        }
    }
}

fn generate_type(src: &mut Source) -> SqlType {
    src.select(
        "type",
        &[
            "smallint", "integer", "bigint", "numeric", "real", "double", "boolean", "text",
            "varchar", "char", "bytea",
        ],
        |_src, data_type, _| match data_type {
            "smallint" => SqlType::SmallInt,
            "integer" => SqlType::Integer,
            "bigint" => SqlType::BigInt,
            "numeric" => SqlType::Numeric,
            "real" => SqlType::Real,
            "double" => SqlType::Double,
            "boolean" => SqlType::Boolean,
            "text" => SqlType::Text,
            "varchar" => SqlType::Varchar,
            "char" => SqlType::Char,
            "bytea" => SqlType::Bytea,
            _ => unreachable!(),
        },
    )
}

fn decimal_literal(value: i32) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let absolute = value.abs();
    format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
}

fn text_literal(src: &mut Source, label: &str) -> String {
    let values = ["", "a", "MiXeD", "word", "'", "東京"];
    let (value, _) = src
        .choose(label, &values)
        .expect("text choices must not be empty");
    format!("'{}'", value.replace('\'', "''"))
}

fn generate_non_null_literal(src: &mut Source, data_type: SqlType) -> String {
    match data_type {
        SqlType::SmallInt | SqlType::Integer | SqlType::BigInt => {
            integer(src, "integer").to_string()
        }
        SqlType::Numeric | SqlType::Real | SqlType::Double => {
            decimal_literal(src.any_of("decimal", int_in_range(-2000..=2000)))
        }
        SqlType::Boolean => if src.any("boolean") { "TRUE" } else { "FALSE" }.into(),
        SqlType::Text | SqlType::Varchar | SqlType::Char => text_literal(src, "text"),
        SqlType::Bytea => {
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
        }
    }
}

fn generate_literal(src: &mut Source, column: &ColumnSchema) -> String {
    if column.nullable {
        src.maybe("null", |src| {
            generate_non_null_literal(src, column.data_type)
        })
        .unwrap_or_else(|| "NULL".into())
    } else {
        generate_non_null_literal(src, column.data_type)
    }
}

fn generate_typed_literal(src: &mut Source, data_type: SqlType) -> String {
    format!(
        "CAST({} AS {})",
        generate_non_null_literal(src, data_type),
        data_type.sql()
    )
}

fn generate_table(src: &mut Source, name: String) -> TableSchema {
    let mut columns = vec![ColumnSchema {
        name: "key".into(),
        data_type: SqlType::BigInt,
        nullable: false,
        default: None,
    }];
    src.repeat_n("columns", 1..=8, |src| {
        let data_type = generate_type(src);
        let nullable = src.any("nullable");
        let default = src.maybe("default", |src| generate_non_null_literal(src, data_type));
        columns.push(ColumnSchema {
            name: format!("value_{}", columns.len()),
            data_type,
            nullable,
            default,
        });
        Effect::Success
    });
    let checked_columns = columns
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, column)| column.data_type.is_numeric())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let checked_column = src
        .maybe("check", |src| src.choose("column", &checked_columns))
        .flatten()
        .map(|(index, _)| *index);
    let unique_column = src
        .maybe("unique", |src| src.choose("column", &columns[1..]))
        .flatten()
        .map(|(_, index)| index + 1);
    TableSchema {
        name,
        columns,
        check_key_positive: src.any("check_key_positive"),
        checked_column,
        unique_column,
    }
}

fn generate_foreign_tables(src: &mut Source, table: &TableSchema) -> ForeignTables {
    let actions = [
        "NO ACTION",
        "RESTRICT",
        "CASCADE",
        "SET NULL",
        "SET DEFAULT",
    ];
    let (on_delete, _) = src.choose("on_delete", &actions).unwrap();
    let (on_update, _) = src.choose("on_update", &actions).unwrap();
    ForeignTables {
        parent: format!("{}_foreign_parent", table.name),
        child: format!("{}_foreign_child", table.name),
        nullable: src.any("nullable"),
        default_parent: src.any("default_parent"),
        inline_reference: src.any("inline_reference"),
        on_delete,
        on_update,
    }
}

fn generate_foreign_insert(
    src: &mut Source,
    tables: &ForeignTables,
    next_child_key: &mut i64,
) -> String {
    let key = *next_child_key;
    *next_child_key += 1;
    let mut values = vec!["parent"];
    if tables.nullable {
        values.push("null");
    }
    if tables.default_parent {
        values.push("default");
    }
    // Generate INSERT ... SELECT here once non-VALUES insert sources are supported.
    src.select("value", &values, |_src, value, _| match value {
        "parent" => format!(
            "INSERT INTO {} (id, parent_id) VALUES ({key}, 1)",
            tables.child
        ),
        "null" => format!(
            "INSERT INTO {} (id, parent_id) VALUES ({key}, NULL)",
            tables.child
        ),
        "default" => format!("INSERT INTO {} (id) VALUES ({key})", tables.child),
        _ => unreachable!(),
    })
}

fn generate_foreign_select(src: &mut Source, tables: &ForeignTables) -> (String, RowOrder) {
    let join = if src.any("outer") {
        "LEFT JOIN"
    } else {
        "INNER JOIN"
    };
    (
        format!(
            "SELECT child.id, parent.id FROM {} AS child {join} {} AS parent \
             ON child.parent_id = parent.id ORDER BY child.id, parent.id",
            tables.child, tables.parent
        ),
        RowOrder::Ordered,
    )
}

fn choose_column<'a>(
    src: &mut Source,
    table: &'a TableSchema,
    predicate: impl Fn(&ColumnSchema) -> bool,
) -> &'a ColumnSchema {
    src.choose_where("column", &table.columns, |column| predicate(column))
        .map(|(column, _)| column)
        .expect("generated table must have a compatible column")
}

fn generate_insert(src: &mut Source, table: &TableSchema, next_key: &mut i64) -> String {
    src.select("shape", &["full", "required", "subset"], |src, shape, _| {
        let included = table
            .columns
            .iter()
            .enumerate()
            .filter(|(index, column)| {
                *index == 0
                    || shape == "full"
                    || (!column.nullable && column.default.is_none())
                    || (shape == "subset" && src.any("include"))
            })
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        src.repeat_n("rows", 1..=4, |src| {
            let values = included
                .iter()
                .map(|(index, column)| {
                    if *index == 0 {
                        let key = *next_key;
                        *next_key += 1;
                        key.to_string()
                    } else if column.default.is_some() && src.any("use_default") {
                        "DEFAULT".into()
                    } else {
                        generate_literal(src, column)
                    }
                })
                .collect::<Vec<_>>();
            rows.push(format!("({})", values.join(", ")));
            Effect::Success
        });
        let columns = included
            .iter()
            .map(|(_, column)| column.name.as_str())
            .collect::<Vec<_>>();
        format!(
            "INSERT INTO {} ({}) VALUES {}",
            table.name,
            columns.join(", "),
            rows.join(", ")
        )
    })
}

fn generate_predicate(src: &mut Source, table: &TableSchema) -> String {
    src.select(
        "predicate",
        &["comparison", "boolean", "null", "distinct", "combined"],
        |src, predicate, _| match predicate {
            "comparison" => {
                let column = choose_column(src, table, |column| column.data_type.is_numeric());
                let operators = ["=", "<>", ">", "<", ">=", "<="];
                let (operator, _) = src.choose("operator", &operators).unwrap();
                format!(
                    "{} {operator} {}",
                    column.name,
                    generate_typed_literal(src, column.data_type)
                )
            }
            "boolean" => {
                if let Some((column, _)) =
                    src.choose_where("boolean_column", &table.columns, |column| {
                        column.data_type == SqlType::Boolean
                    })
                {
                    let operator = if src.any("expected") {
                        "IS TRUE"
                    } else {
                        "IS FALSE"
                    };
                    format!("{} {operator}", column.name)
                } else {
                    format!("{} > 0", table.key().name)
                }
            }
            "null" => {
                let column = choose_column(src, table, |_| true);
                let operator = if src.any("not") {
                    "IS NOT NULL"
                } else {
                    "IS NULL"
                };
                format!("{} {operator}", column.name)
            }
            "distinct" => {
                let column = choose_column(src, table, |_| true);
                format!(
                    "{} IS {}DISTINCT FROM {}",
                    column.name,
                    if src.any("not") { "NOT " } else { "" },
                    generate_non_null_literal(src, column.data_type)
                )
            }
            "combined" => format!(
                "({}) {} ({})",
                generate_predicate_leaf(src, table),
                if src.any("and") { "AND" } else { "OR" },
                generate_predicate_leaf(src, table)
            ),
            _ => unreachable!(),
        },
    )
}

fn generate_predicate_leaf(src: &mut Source, table: &TableSchema) -> String {
    let column = choose_column(src, table, |_| true);
    if src.any("null_test") {
        format!(
            "{} IS {}NULL",
            column.name,
            if src.any("not") { "NOT " } else { "" }
        )
    } else {
        format!(
            "{} IS {}DISTINCT FROM {}",
            column.name,
            if src.any("not") { "NOT " } else { "" },
            generate_non_null_literal(src, column.data_type)
        )
    }
}

fn generate_where_clause(src: &mut Source, table: &TableSchema) -> String {
    src.maybe("where", |src| generate_predicate(src, table))
        .map(|predicate| format!(" WHERE {predicate}"))
        .unwrap_or_default()
}

fn generate_select_expression(src: &mut Source, table: &TableSchema) -> String {
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
            "column" => choose_column(src, table, |_| true).name.clone(),
            "arithmetic" => {
                let column = choose_column(src, table, |column| column.data_type.is_numeric());
                let operators = ["+", "-", "*", "/", "%"];
                let operators = if column.data_type.is_integral() {
                    &operators[..]
                } else {
                    &operators[..4]
                };
                let (operator, _) = src.choose("operator", operators).unwrap();
                let right = if column.data_type == SqlType::Real {
                    if *operator == "/" {
                        src.any_of("right", int_in_range(1..=5)).to_string()
                    } else {
                        integer(src, "right").to_string()
                    }
                } else if *operator == "/" || *operator == "%" {
                    let divisors = [1, 2, 3, 4, 5];
                    let (right, _) = src.choose("right", &divisors).unwrap();
                    format!("CAST({} AS {})", right, column.data_type.sql())
                } else {
                    generate_typed_literal(src, column.data_type)
                };
                format!("{} {operator} {right}", column.name)
            }
            "comparison" => {
                let column = choose_column(src, table, |column| column.data_type.is_numeric());
                let operators = ["=", "<>", ">", "<", ">=", "<="];
                let (operator, _) = src.choose("operator", &operators).unwrap();
                format!(
                    "{} {operator} {}",
                    column.name,
                    generate_typed_literal(src, column.data_type)
                )
            }
            "boolean" => generate_predicate(src, table),
            "null" => generate_predicate_leaf(src, table),
            "case" => {
                let column = choose_column(src, table, |_| true);
                format!(
                    "CASE WHEN {} THEN {} ELSE CAST({} AS {}) END",
                    generate_predicate_leaf(src, table),
                    column.name,
                    generate_non_null_literal(src, column.data_type),
                    column.data_type.sql()
                )
            }
            "function" => {
                let column = choose_column(src, table, |_| true);
                if column.data_type.supports_text_functions() {
                    let functions = ["lower", "upper", "length"];
                    let (function, _) = src.choose("text_function", &functions).unwrap();
                    format!("{function}({})", column.name)
                } else if column.data_type.is_numeric() {
                    format!("abs({})", column.name)
                } else {
                    format!(
                        "COALESCE({}, {})",
                        column.name,
                        generate_typed_literal(src, column.data_type)
                    )
                }
            }
            "cast" => {
                let column = choose_column(src, table, |_| true);
                let target = match column.data_type {
                    SqlType::SmallInt => "BIGINT",
                    SqlType::Integer | SqlType::BigInt => "TEXT",
                    SqlType::Numeric => "INTEGER",
                    SqlType::Real => "TEXT",
                    SqlType::Double => "REAL",
                    SqlType::Boolean => "INTEGER",
                    SqlType::Text => "VARCHAR(12)",
                    SqlType::Varchar => "TEXT",
                    SqlType::Char => "VARCHAR(12)",
                    SqlType::Bytea => "BYTEA",
                };
                format!("CAST({} AS {target})", column.name)
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

fn generate_select(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    let mut projections = Vec::new();
    src.repeat_n("projections", 1..=4, |src| {
        projections.push(generate_select_expression(src, table));
        Effect::Success
    });
    let mut sql = format!(
        "SELECT {} FROM {}{}",
        projections.join(", "),
        table.name,
        generate_where_clause(src, table)
    );
    let ordered = src.maybe("order", |src| {
        let direction = if src.any("descending") { "DESC" } else { "ASC" };
        let nulls = if src.any("nulls_first") {
            "NULLS FIRST"
        } else {
            "NULLS LAST"
        };
        format!(
            " ORDER BY {}.{} {direction} {nulls}",
            table.name,
            table.key().name
        )
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

fn generate_aggregate(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    src.select(
        "aggregate",
        &["count", "sum", "average", "minimum_maximum", "boolean"],
        |src, aggregate, _| {
            let projections = match aggregate {
                "count" => {
                    let column = choose_column(src, table, |_| true);
                    format!(
                        "count(*), count({}), count(*) + count({})",
                        column.name, column.name
                    )
                }
                "sum" => {
                    let column = choose_column(src, table, |column| column.data_type.is_numeric());
                    format!("sum({0}), coalesce(sum({0}), 0)", column.name)
                }
                "average" => {
                    let column = choose_column(src, table, |column| column.data_type.is_numeric());
                    format!("avg({0}), coalesce(avg({0}), 0)", column.name)
                }
                "minimum_maximum" => {
                    let column =
                        choose_column(src, table, |column| column.data_type != SqlType::Boolean);
                    format!("min({0}), max({0})", column.name)
                }
                "boolean" => {
                    let booleans = table
                        .columns
                        .iter()
                        .filter(|column| column.data_type == SqlType::Boolean)
                        .collect::<Vec<_>>();
                    if booleans.is_empty() {
                        "bool_and(TRUE), bool_or(FALSE)".into()
                    } else {
                        let (column, _) = src
                            .choose("column", &booleans)
                            .expect("boolean columns must not be empty");
                        format!("bool_and({0}), bool_or({0})", column.name)
                    }
                }
                _ => unreachable!(),
            };
            (
                format!(
                    "SELECT {projections} FROM {}{}",
                    table.name,
                    generate_where_clause(src, table)
                ),
                RowOrder::Ordered,
            )
        },
    )
}

fn generate_assignment(src: &mut Source, column: &ColumnSchema) -> String {
    let mut variants = vec!["literal", "expression"];
    if column.nullable {
        variants.push("null");
    }
    if column.default.is_some() {
        variants.push("default");
    }
    src.select("value", &variants, |src, value, _| match value {
        "literal" => generate_non_null_literal(src, column.data_type),
        "null" => "NULL".into(),
        "default" => "DEFAULT".into(),
        "expression" if column.data_type == SqlType::Boolean => format!("NOT {}", column.name),
        "expression" if column.data_type.supports_text_functions() => {
            format!("upper({})", column.name)
        }
        "expression" if column.data_type.is_numeric() => format!("-{}", column.name),
        "expression" => format!(
            "COALESCE({}, {})",
            column.name,
            generate_typed_literal(src, column.data_type)
        ),
        _ => unreachable!(),
    })
}

fn generate_update(src: &mut Source, table: &TableSchema) -> String {
    let column = choose_column(src, table, |column| column.name != table.key().name);
    format!(
        "UPDATE {} SET {} = {}{}",
        table.name,
        column.name,
        generate_assignment(src, column),
        generate_where_clause(src, table)
    )
}

fn generate_delete(src: &mut Source, table: &TableSchema) -> String {
    format!(
        "DELETE FROM {}{}",
        table.name,
        generate_where_clause(src, table)
    )
}

fn generate_join(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    let offset = src.any_of("offset", int_in_range(-2..=2));
    src.select(
        "join",
        &["inner", "left", "right", "full", "cross"],
        |_src, join, _| {
            let key = &table.key().name;
            let source = match join {
                "inner" => format!(
                    "{} AS left_row INNER JOIN {} AS right_row ON left_row.{key} = right_row.{key} + {offset}",
                    table.name, table.name
                ),
                "left" => format!(
                    "{} AS left_row LEFT JOIN {} AS right_row ON left_row.{key} = right_row.{key} + {offset}",
                    table.name, table.name
                ),
                "right" => format!(
                    "{} AS left_row RIGHT JOIN {} AS right_row ON left_row.{key} = right_row.{key} + {offset}",
                    table.name, table.name
                ),
                "full" => format!(
                    "{} AS left_row FULL JOIN {} AS right_row ON left_row.{key} = right_row.{key} + {offset}",
                    table.name, table.name
                ),
                "cross" => format!(
                    "{} AS left_row CROSS JOIN {} AS right_row",
                    table.name, table.name
                ),
                _ => unreachable!(),
            };
            let selection = if join == "cross" {
                format!(" WHERE left_row.{key} = right_row.{key} + {offset}")
            } else {
                String::new()
            };
            (
                format!(
                    "SELECT left_row.{key}, right_row.{key} FROM {source}{selection} ORDER BY 1, 2"
                ),
                RowOrder::Ordered,
            )
        },
    )
}

fn generate_subquery(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    let key = &table.key().name;
    src.select(
        "subquery",
        &["derived", "scalar", "in", "exists", "quantified", "correlated"],
        |src, subquery, _| match subquery {
            "derived" => (
                format!(
                    "SELECT source.{key} FROM (SELECT {key} FROM {}{}) AS source ORDER BY source.{key}",
                    table.name,
                    generate_where_clause(src, table)
                ),
                RowOrder::Ordered,
            ),
            "scalar" => (
                format!(
                    "SELECT outer_row.{key} FROM {} AS outer_row WHERE outer_row.{key} = \
                     (SELECT inner_row.{key} FROM {} AS inner_row ORDER BY inner_row.{key} LIMIT 1)",
                    table.name, table.name
                ),
                RowOrder::Unordered,
            ),
            "in" => (
                format!(
                    "SELECT outer_row.{key} FROM {} AS outer_row WHERE outer_row.{key} IN \
                     (SELECT inner_row.{key} FROM {} AS inner_row) ORDER BY outer_row.{key}",
                    table.name, table.name
                ),
                RowOrder::Ordered,
            ),
            "exists" => (
                format!(
                    "SELECT EXISTS (SELECT 1 FROM {}{})",
                    table.name,
                    generate_where_clause(src, table)
                ),
                RowOrder::Ordered,
            ),
            "quantified" => {
                let operator = if src.any("all") { "ALL" } else { "ANY" };
                let value = src.any_of("value", int_in_range(1..=20));
                (
                    format!(
                        "SELECT {value} = {operator} (SELECT {key} FROM {})",
                        table.name
                    ),
                    RowOrder::Ordered,
                )
            }
            "correlated" => (
                format!(
                    "SELECT outer_row.{key}, EXISTS (SELECT 1 FROM {} AS inner_row \
                     WHERE inner_row.{key} = outer_row.{key}) FROM {} AS outer_row \
                     ORDER BY outer_row.{key}",
                    table.name, table.name
                ),
                RowOrder::Ordered,
            ),
            _ => unreachable!(),
        },
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    check(|src| {
        let table_name = format!(
            "pg_fake_property_{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let mut postgres = postgres.borrow_mut();
        let mut postgres = PostgresCase {
            connection: &mut postgres,
            runtime: &runtime,
            table: table_name.clone(),
        };
        let mut fake = PgFakeConnection::new(Db::create());
        let table = generate_table(src, table_name);
        let foreign_tables = generate_foreign_tables(src, &table);
        let mut next_key = 1;
        let mut next_child_key = 1;
        let mut in_transaction = false;
        let create = table.create_sql();
        src.log_value("sql", &create);
        assert_statement(
            &runtime,
            postgres.get_connection(),
            &mut fake,
            &create,
            RowOrder::Unordered,
        );
        let insert = generate_insert(src, &table, &mut next_key);
        src.log_value("sql", &insert);
        assert_statement(
            &runtime,
            postgres.get_connection(),
            &mut fake,
            &insert,
            RowOrder::Unordered,
        );
        for sql in [
            foreign_tables.create_parent_sql(),
            foreign_tables.create_child_sql(),
            format!("INSERT INTO {} (id) VALUES (1)", foreign_tables.parent),
        ] {
            src.log_value("sql", &sql);
            assert_statement(
                &runtime,
                postgres.get_connection(),
                &mut fake,
                &sql,
                RowOrder::Unordered,
            );
        }

        src.repeat_n("statements", 3..=14, |src| {
            let actions: &[&'static str] = if in_transaction {
                &[
                    "insert",
                    "select",
                    "aggregate",
                    "join",
                    "subquery",
                    "foreign_insert",
                    "foreign_select",
                    "update",
                    "delete",
                    "set_lock_timeout",
                    "commit",
                    "rollback",
                ]
            } else {
                &[
                    "insert",
                    "select",
                    "aggregate",
                    "join",
                    "subquery",
                    "foreign_insert",
                    "foreign_select",
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
                        generate_insert(src, &table, &mut next_key),
                        RowOrder::Unordered,
                    ),
                    "select" => generate_select(src, &table),
                    "aggregate" => generate_aggregate(src, &table),
                    "join" => generate_join(src, &table),
                    "subquery" => generate_subquery(src, &table),
                    "foreign_insert" => (
                        generate_foreign_insert(src, &foreign_tables, &mut next_child_key),
                        RowOrder::Unordered,
                    ),
                    "foreign_select" => generate_foreign_select(src, &foreign_tables),
                    "update" => (generate_update(src, &table), RowOrder::Unordered),
                    "delete" => (generate_delete(src, &table), RowOrder::Unordered),
                    "begin" => {
                        in_transaction = true;
                        let sql = if src.any("explicit_isolation") {
                            format!("BEGIN ISOLATION LEVEL {}", isolation_level(src))
                        } else {
                            "BEGIN".into()
                        };
                        (sql, RowOrder::Unordered)
                    }
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
                assert_statement(&runtime, postgres.get_connection(), &mut fake, &sql, order);
                Effect::Success
            })
        });

        if in_transaction {
            let sql = if src.any("commit_final_transaction") {
                "COMMIT"
            } else {
                "ROLLBACK"
            };
            assert_statement(
                &runtime,
                postgres.get_connection(),
                &mut fake,
                sql,
                RowOrder::Unordered,
            );
        }
    });
}
