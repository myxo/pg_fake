use std::{
    cell::RefCell,
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
use tokio::runtime::Runtime;

mod common;

use common::start_postgres_server;

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

fn returns_rows(statement: &Statement) -> bool {
    match statement {
        Statement::Query(_) => true,
        Statement::Insert(insert) => insert.returning.is_some(),
        Statement::Update(update) => update.returning.is_some(),
        Statement::Delete(delete) => delete.returning.is_some(),
        _ => false,
    }
}

static TEST_LOCK: Mutex<()> = Mutex::new(());
static TABLE_NUMBER: AtomicU64 = AtomicU64::new(1);

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

struct PostgresSessionsCase<'connection, 'runtime> {
    connections: &'connection mut [PgConnection],
    runtime: &'runtime Runtime,
    table: String,
}

impl Drop for PostgresSessionsCase<'_, '_> {
    fn drop(&mut self) {
        for connection in self.connections.iter_mut() {
            let _ = self
                .runtime
                .block_on(sqlx::raw_sql(AssertSqlSafe("ROLLBACK")).execute(&mut *connection));
        }
        let sql = format!("DROP TABLE IF EXISTS {}", self.table);
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(AssertSqlSafe(sql.as_str())).execute(&mut self.connections[0]));
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
    if returns_rows(statement) {
        match sqlx::raw_sql(AssertSqlSafe(sql))
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
        }
    } else {
        match sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&mut *connection)
            .await
        {
            Ok(result) => Outcome::Affected(rows_affected(result)),
            Err(error) => make_error_outcome(error),
        }
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

fn generate_main_insert(src: &mut Source, table: &TableSchema, next_key: &mut i64) -> String {
    let mut sql = src.select(
        "insert_source",
        &["values", "select"],
        |src, source, _| match source {
            "values" => src.select("shape", &["full", "required", "subset"], |src, shape, _| {
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
            }),
            "select" => {
                let key = *next_key;
                *next_key += 1;
                let values = table
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        if index == 0 {
                            key.to_string()
                        } else {
                            generate_literal(src, column)
                        }
                    })
                    .collect::<Vec<_>>();
                let columns = table
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect::<Vec<_>>();
                format!(
                    "INSERT INTO {} ({}) SELECT {} WHERE {}",
                    table.name,
                    columns.join(", "),
                    values.join(", "),
                    if src.any("source_row") {
                        "TRUE"
                    } else {
                        "FALSE"
                    }
                )
            }
            _ => unreachable!(),
        },
    );
    sql.push_str(&generate_returning_clause(src, table, &table.name, None));
    sql
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

fn generate_select_core(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
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

fn generate_distinct(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    src.select(
        "kind",
        &["rows", "on"],
        |src, distinct, _| match distinct {
            "rows" => {
                let first = choose_column(src, table, |_| true);
                let second = choose_column(src, table, |_| true);
                (
                    format!(
                        "SELECT DISTINCT {0}, {1} FROM {2}{3} ORDER BY 1 NULLS FIRST, 2 NULLS LAST",
                        first.name,
                        second.name,
                        table.name,
                        generate_where_clause(src, table),
                    ),
                    RowOrder::Ordered,
                )
            }
            "on" => {
                let key = choose_column(src, table, |_| true);
                let value = choose_column(src, table, |_| true);
                (
                    format!(
                        "SELECT DISTINCT ON ({0}) {0}, {1} FROM {2}{3} ORDER BY {0} NULLS FIRST, {1} DESC NULLS LAST",
                        key.name,
                        value.name,
                        table.name,
                        generate_where_clause(src, table),
                    ),
                    RowOrder::Ordered,
                )
            }
            _ => unreachable!(),
        },
    )
}

fn generate_aggregate(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    src.select(
        "aggregate",
        &[
            "count",
            "sum",
            "average",
            "minimum_maximum",
            "boolean",
            "grouped",
            "distinct_filter",
        ],
        |src, aggregate, _| {
            if aggregate == "grouped" {
                let column = choose_column(src, table, |_| true);
                return (
                    format!(
                        "SELECT {0}, count(*), count({1}) FROM {2}{3} GROUP BY {0} HAVING count(*) >= 1 ORDER BY {0}",
                        column.name,
                        table.key().name,
                        table.name,
                        generate_where_clause(src, table),
                    ),
                    RowOrder::Ordered,
                );
            }
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
                "distinct_filter" => {
                    let column = choose_column(src, table, |_| true);
                    let filter = table
                        .columns
                        .iter()
                        .find(|column| column.data_type == SqlType::Boolean)
                        .map(|column| column.name.as_str())
                        .unwrap_or("TRUE");
                    format!(
                        "count(DISTINCT {0}), count(*) FILTER (WHERE {filter})",
                        column.name
                    )
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
    let (mut sql, target, source) = if src.any("from_clause") {
        let cutoff = src.any_of("cutoff", int_in_range(1..=20));
        (
            format!(
                "UPDATE {0} AS target SET {1} = source.{1} FROM {0} AS source \
                 WHERE target.{2} = source.{2} AND target.{2} <= {cutoff}",
                table.name,
                column.name,
                table.key().name,
            ),
            "target",
            Some("source"),
        )
    } else {
        (
            format!(
                "UPDATE {} SET {} = {}{}",
                table.name,
                column.name,
                generate_assignment(src, column),
                generate_where_clause(src, table)
            ),
            table.name.as_str(),
            None,
        )
    };
    sql.push_str(&generate_returning_clause(src, table, target, source));
    sql
}

fn generate_delete(src: &mut Source, table: &TableSchema) -> String {
    let (mut sql, target, source) = if src.any("using_clause") {
        let cutoff = src.any_of("cutoff", int_in_range(1..=20));
        (
            format!(
                "DELETE FROM {0} AS target USING {0} AS source \
                 WHERE target.{1} = source.{1} AND target.{1} <= {cutoff}",
                table.name,
                table.key().name,
            ),
            "target",
            Some("source"),
        )
    } else {
        (
            format!(
                "DELETE FROM {}{}",
                table.name,
                generate_where_clause(src, table)
            ),
            table.name.as_str(),
            None,
        )
    };
    sql.push_str(&generate_returning_clause(src, table, target, source));
    sql
}

fn generate_returning_projection(
    src: &mut Source,
    table: &TableSchema,
    target: &str,
    source: Option<&str>,
) -> String {
    let mut projections = vec!["qualified", "alias", "expression"];
    if source.is_none() {
        projections.push("wildcard");
    } else {
        projections.push("source");
    }
    src.select(
        "target_list",
        &projections,
        |src, projection, _| match projection {
            "wildcard" => "*".into(),
            "qualified" => format!("{target}.*"),
            "alias" => {
                let column = choose_column(src, table, |_| true);
                format!("{target}.{} AS returned_value", column.name)
            }
            "expression" => {
                let column = choose_column(src, table, |_| true);
                format!(
                    "COALESCE({target}.{}, {}) AS returned_value",
                    column.name,
                    generate_typed_literal(src, column.data_type)
                )
            }
            "source" => {
                let column = choose_column(src, table, |_| true);
                format!("{}.{} AS source_value", source.unwrap(), column.name)
            }
            _ => unreachable!(),
        },
    )
}

fn generate_returning_clause(
    src: &mut Source,
    table: &TableSchema,
    target: &str,
    source: Option<&str>,
) -> String {
    src.maybe("returning_clause", |src| {
        generate_returning_projection(src, table, target, source)
    })
    .map(|projection| format!(" RETURNING {projection}"))
    .unwrap_or_default()
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

fn generate_insert(
    src: &mut Source,
    table: &TableSchema,
    foreign_tables: &ForeignTables,
    next_key: &mut i64,
    next_child_key: &mut i64,
) -> String {
    src.select(
        "table_name",
        &["main", "foreign_child"],
        |src, table_name, _| match table_name {
            "main" => generate_main_insert(src, table, next_key),
            "foreign_child" => {
                let mut sql = generate_foreign_insert(src, foreign_tables, next_child_key);
                if src.any("returning_clause") {
                    sql.push_str(" RETURNING *");
                }
                sql
            }
            _ => unreachable!(),
        },
    )
}

fn generate_select(
    src: &mut Source,
    table: &TableSchema,
    foreign_tables: &ForeignTables,
) -> (String, RowOrder) {
    src.select(
        "select_body",
        &[
            "core",
            "distinct",
            "aggregate",
            "join",
            "subquery",
            "foreign_join",
        ],
        |src, select_body, _| match select_body {
            "core" => generate_select_core(src, table),
            "distinct" => generate_distinct(src, table),
            "aggregate" => generate_aggregate(src, table),
            "join" => generate_join(src, table),
            "subquery" => generate_subquery(src, table),
            "foreign_join" => generate_foreign_select(src, foreign_tables),
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

fn generate_statement(
    src: &mut Source,
    table: &TableSchema,
    foreign_tables: &ForeignTables,
    next_key: &mut i64,
    next_child_key: &mut i64,
    in_transaction: &mut bool,
) -> (String, RowOrder) {
    let statements: &[&str] = if *in_transaction {
        &[
            "insert", "select", "update", "delete", "set", "commit", "rollback",
        ]
    } else {
        &["insert", "select", "update", "delete", "set", "begin"]
    };
    src.select(
        "statement",
        statements,
        |src, statement, _| match statement {
            "insert" => (
                generate_insert(src, table, foreign_tables, next_key, next_child_key),
                RowOrder::Unordered,
            ),
            "select" => generate_select(src, table, foreign_tables),
            "update" => (generate_update(src, table), RowOrder::Unordered),
            "delete" => (generate_delete(src, table), RowOrder::Unordered),
            "set" => {
                let settings: &[&str] = if *in_transaction {
                    &["lock_timeout"]
                } else {
                    &["session_characteristics", "lock_timeout"]
                };
                let sql = src.select("set", settings, |src, setting, _| match setting {
                    "session_characteristics" => format!(
                        "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {}",
                        isolation_level(src)
                    ),
                    "lock_timeout" => lock_timeout_sql(src),
                    _ => unreachable!(),
                });
                (sql, RowOrder::Unordered)
            }
            "begin" => {
                *in_transaction = true;
                let sql = if src.any("explicit_isolation") {
                    format!("BEGIN ISOLATION LEVEL {}", isolation_level(src))
                } else {
                    "BEGIN".into()
                };
                (sql, RowOrder::Unordered)
            }
            "commit" => {
                *in_transaction = false;
                ("COMMIT".into(), RowOrder::Unordered)
            }
            "rollback" => {
                *in_transaction = false;
                ("ROLLBACK".into(), RowOrder::Unordered)
            }
            _ => unreachable!(),
        },
    )
}

fn generate_snapshot_select(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    (
        format!(
            "SELECT {0} FROM {1}{2} ORDER BY {0}",
            table.key().name,
            table.name,
            generate_where_clause(src, table),
        ),
        RowOrder::Ordered,
    )
}

fn generate_snapshot_statement(
    src: &mut Source,
    table: &TableSchema,
    next_key: &mut i64,
    in_transaction: &mut bool,
) -> (String, RowOrder) {
    let statements: &[&str] = if *in_transaction {
        &["insert", "select", "commit", "rollback"]
    } else {
        &["insert", "select", "begin"]
    };
    src.select(
        "snapshot_statement",
        statements,
        |src, statement, _| match statement {
            "insert" => (
                generate_main_insert(src, table, next_key),
                RowOrder::Unordered,
            ),
            "select" => generate_snapshot_select(src, table),
            "begin" => {
                *in_transaction = true;
                (
                    format!("BEGIN ISOLATION LEVEL {}", isolation_level(src)),
                    RowOrder::Unordered,
                )
            }
            "commit" => {
                *in_transaction = false;
                ("COMMIT".into(), RowOrder::Unordered)
            }
            "rollback" => {
                *in_transaction = false;
                ("ROLLBACK".into(), RowOrder::Unordered)
            }
            _ => unreachable!(),
        },
    )
}

#[test]
fn generated_sql_matches_postgres() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = start_postgres_server();
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
        let insert = generate_main_insert(src, &table, &mut next_key);
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
            let (sql, order) = generate_statement(
                src,
                &table,
                &foreign_tables,
                &mut next_key,
                &mut next_child_key,
                &mut in_transaction,
            );
            src.log_value("sql", &sql);
            assert_statement(&runtime, postgres.get_connection(), &mut fake, &sql, order);
            Effect::Success
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

#[test]
fn generated_interleaved_transaction_snapshots_match_postgres() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = start_postgres_server();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        (0..3)
            .map(|_| {
                runtime
                    .block_on(PgConnection::connect(&server.url))
                    .expect("must connect SQLx to PostgreSQL 18 once")
            })
            .collect::<Vec<_>>(),
    );
    check(|src| {
        let table_name = format!(
            "pg_fake_snapshot_property_{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let mut postgres_connections = postgres.borrow_mut();
        let postgres = PostgresSessionsCase {
            connections: &mut postgres_connections,
            runtime: &runtime,
            table: table_name.clone(),
        };
        let db = Db::create();
        let mut fake = (0..3)
            .map(|_| PgFakeConnection::new(db.clone()))
            .collect::<Vec<_>>();
        let mut table = generate_table(src, table_name);
        table.unique_column = None;
        let mut next_key = 1;
        let create = table.create_sql();
        src.log_value("sql", &create);
        assert_statement(
            &runtime,
            &mut postgres.connections[0],
            &mut fake[0],
            &create,
            RowOrder::Unordered,
        );
        let insert = generate_main_insert(src, &table, &mut next_key);
        src.log_value("sql", &insert);
        assert_statement(
            &runtime,
            &mut postgres.connections[0],
            &mut fake[0],
            &insert,
            RowOrder::Unordered,
        );

        let mut in_transaction = [false; 3];
        for session in 0..3 {
            let begin = format!("BEGIN ISOLATION LEVEL {}", isolation_level(src));
            src.log_value("sql", &begin);
            assert_statement(
                &runtime,
                &mut postgres.connections[session],
                &mut fake[session],
                &begin,
                RowOrder::Unordered,
            );
            in_transaction[session] = true;
            let (select, order) = generate_snapshot_select(src, &table);
            src.log_value("sql", &select);
            assert_statement(
                &runtime,
                &mut postgres.connections[session],
                &mut fake[session],
                &select,
                order,
            );
        }

        src.repeat_n("interleaving", 12..=36, |src| {
            let session = src.any_of("session", int_in_range(0_usize..=2));
            let (sql, order) = generate_snapshot_statement(
                src,
                &table,
                &mut next_key,
                &mut in_transaction[session],
            );
            src.log_value("session", &session);
            src.log_value("sql", &sql);
            assert_statement(
                &runtime,
                &mut postgres.connections[session],
                &mut fake[session],
                &sql,
                order,
            );
            Effect::Success
        });

        for session in 0..3 {
            if !in_transaction[session] {
                continue;
            }
            let sql = if src.any("commit_final_transaction") {
                "COMMIT"
            } else {
                "ROLLBACK"
            };
            src.log_value("sql", &sql);
            assert_statement(
                &runtime,
                &mut postgres.connections[session],
                &mut fake[session],
                sql,
                RowOrder::Unordered,
            );
        }

        let sql = format!("SELECT * FROM {} ORDER BY key", table.name);
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.connections[0],
            &mut fake[0],
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn generated_sequence_allocations_follow_the_option_model() {
    check(|src| {
        let increments = [-5_i64, -3, -1, 1, 2, 4];
        let (increment, _) = src.choose("increment", &increments).unwrap();
        let min_value = src.any_of("min_value", int_in_range(-20_i64..=-1));
        let max_value = src.any_of("max_value", int_in_range(1_i64..=20));
        let start_value = src.any_of("start_value", int_in_range(min_value..=max_value));
        let cycles = [false, true];
        let (cycle, _) = src.choose("cycle", &cycles).unwrap();
        let calls = src.any_of("calls", int_in_range(1..=8));
        let db = pg_fake::api::Db::create();
        let mut session = db.create_session();
        session
            .execute(&format!(
                "CREATE SEQUENCE property_sequence INCREMENT {increment} MINVALUE {min_value} MAXVALUE {max_value} START {start_value} CACHE 7 {}",
                if *cycle { "CYCLE" } else { "NO CYCLE" }
            ))
            .unwrap();
        let mut expected = start_value;
        for call in 0..calls {
            let actual = session.query("SELECT nextval('property_sequence')", &[]);
            if call == 0 {
                assert_eq!(
                    actual.unwrap().rows,
                    vec![vec![pg_fake::value::Value::Int8(expected)]]
                );
                continue;
            }
            let candidate = i128::from(expected) + i128::from(*increment);
            if candidate > i128::from(max_value) || candidate < i128::from(min_value) {
                if !*cycle {
                    assert_eq!(
                        actual.unwrap_err().sqlstate,
                        pg_fake::error::SqlState::SequenceGeneratorLimitExceeded
                    );
                    break;
                }
                expected = if *increment > 0 { min_value } else { max_value };
            } else {
                expected = candidate as i64;
            }
            assert_eq!(
                actual.unwrap().rows,
                vec![vec![pg_fake::value::Value::Int8(expected)]]
            );
        }
    })
}
