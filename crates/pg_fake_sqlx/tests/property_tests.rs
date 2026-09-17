#[cfg(test)]
use std::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
use chaos_theory::check;
use chaos_theory::{Effect, Source, make::int_in};
use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::Connection;
use sqlx_postgres::PgConnection;
use tokio::runtime::Runtime;

#[cfg(test)]
mod common;
#[path = "common/differential.rs"]
mod differential;

use differential::{RowOrder, assert_statement};
#[cfg(test)]
use differential::{assert_statement_allow_error, start_isolated_postgres_server};

#[cfg(test)]
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
            .block_on(sqlx::raw_sql("ROLLBACK").execute(&mut *self.connection));
        let sql = format!(
            "DROP TABLE IF EXISTS {0}_foreign_child, {0}_foreign_parent, {0}_ddl, {0}",
            self.table
        );
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(sql.as_str()).execute(&mut *self.connection));
        let sql = format!("DROP FUNCTION IF EXISTS {}_trigger_fn()", self.table);
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(sql.as_str()).execute(&mut *self.connection));
    }
}

#[cfg(test)]
struct PostgresSessionsCase<'connection, 'runtime> {
    connections: &'connection mut [PgConnection],
    runtime: &'runtime Runtime,
    table: String,
}

#[cfg(test)]
impl Drop for PostgresSessionsCase<'_, '_> {
    fn drop(&mut self) {
        for connection in self.connections.iter_mut() {
            let _ = self
                .runtime
                .block_on(sqlx::raw_sql("ROLLBACK").execute(&mut *connection));
        }
        let sql = format!("DROP TABLE IF EXISTS {}", self.table);
        let _ = self
            .runtime
            .block_on(sqlx::raw_sql(sql.as_str()).execute(&mut self.connections[0]));
    }
}

fn integer(src: &mut Source, label: &str) -> i32 {
    src.any_of(label, int_in(-20..=20))
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

    fn is_exact_numeric(self) -> bool {
        matches!(
            self,
            Self::SmallInt | Self::Integer | Self::BigInt | Self::Numeric
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
            decimal_literal(src.any_of("decimal", int_in(-2000..=2000)))
        }
        SqlType::Boolean => if src.any("boolean") { "TRUE" } else { "FALSE" }.into(),
        SqlType::Text | SqlType::Varchar | SqlType::Char => text_literal(src, "text"),
        SqlType::Bytea => {
            let bytes = [
                src.any_of("a", int_in(0_u8..=255)),
                src.any_of("b", int_in(0_u8..=255)),
                src.any_of("c", int_in(0_u8..=255)),
                src.any_of("d", int_in(0_u8..=255)),
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
    if *next_key > 1 && src.any("on_conflict") {
        return generate_on_conflict_insert(src, table, next_key);
    }
    generate_conflict_free_main_insert(src, table, next_key)
}

fn generate_conflict_free_main_insert(
    src: &mut Source,
    table: &TableSchema,
    next_key: &mut i64,
) -> String {
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

fn generate_on_conflict_insert(
    src: &mut Source,
    table: &TableSchema,
    next_key: &mut i64,
) -> String {
    let key = src.any_of("key", int_in(1..=*next_key - 1));
    let update = src.select("action", &["nothing", "update"], |_src, action, _| {
        action == "update"
    });
    let columns = table
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    src.repeat_n("rows", if update { 1..=1 } else { 1..=3 }, |src| {
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
        rows.push(format!("({})", values.join(", ")));
        Effect::Success
    });
    let target_options: &[&str] = if update {
        &["columns", "constraint"]
    } else {
        &["none", "columns", "constraint"]
    };
    let target = src.select("target", target_options, |_src, target, _| match target {
        "none" => String::new(),
        "columns" => format!(" ({})", table.key().name),
        "constraint" => format!(" ON CONSTRAINT {}_pkey", table.name),
        _ => unreachable!(),
    });
    let returning = if src.any("returning") {
        " RETURNING *"
    } else {
        ""
    };
    let (alias, action) = if update {
        let column = &table.columns[1];
        let alias = if src.any("alias") { " AS target" } else { "" };
        let selection = if src.any("where") {
            format!(
                " WHERE {}.{} {} excluded.{}",
                if alias.is_empty() {
                    &table.name
                } else {
                    "target"
                },
                table.key().name,
                if src.any("where_matches") { "=" } else { "<>" },
                table.key().name,
            )
        } else {
            String::new()
        };
        (
            alias,
            format!(
                "DO UPDATE SET {} = excluded.{}{selection}",
                column.name, column.name
            ),
        )
    } else {
        ("", "DO NOTHING".to_owned())
    };
    format!(
        "INSERT INTO {}{alias} ({}) VALUES {} ON CONFLICT{target} {action}{returning}",
        table.name,
        columns.join(", "),
        rows.join(", ")
    )
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
                        src.any_of("right", int_in(1..=5)).to_string()
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
        "small" => src.any_of("count", int_in(1..=8)).to_string(),
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
                    let column =
                        choose_column(src, table, |column| column.data_type.is_exact_numeric());
                    format!("sum({0}), coalesce(sum({0}), 0)", column.name)
                }
                "average" => {
                    let column =
                        choose_column(src, table, |column| column.data_type.is_exact_numeric());
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
        let cutoff = src.any_of("cutoff", int_in(1..=20));
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
        let cutoff = src.any_of("cutoff", int_in(1..=20));
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
    let offset = src.any_of("offset", int_in(-2..=2));
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
                let value = src.any_of("value", int_in(1..=20));
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

fn generate_cte(src: &mut Source, table: &TableSchema) -> (String, RowOrder) {
    src.select(
        "cte_kind",
        &[
            "materialized",
            "data_modifying",
            "recursive_all",
            "recursive_distinct",
        ],
        |src, cte_kind, _| match cte_kind {
            "materialized" => {
                let key = &table.key().name;
                (
                    format!(
                        "WITH source(value) AS (SELECT {key} FROM {}) SELECT source.value FROM source ORDER BY source.value",
                        table.name
                    ),
                    RowOrder::Ordered,
                )
            }
            "data_modifying" => {
                let key = &table.key().name;
                (
                    format!(
                        "WITH changed(value) AS (UPDATE {} SET {key} = {key} RETURNING {key}) SELECT left_row.value, right_row.value FROM changed AS left_row JOIN changed AS right_row ON left_row.value = right_row.value ORDER BY left_row.value",
                        table.name
                    ),
                    RowOrder::Ordered,
                )
            }
            "recursive_all" => {
                let start = src.any_of("start", int_in(-3..=3));
                let length = src.any_of("length", int_in(0..=8));
                let end = start + length;
                (
                    format!(
                        "WITH RECURSIVE series(value) AS (VALUES ({start}) UNION ALL SELECT value + 1 FROM series WHERE value < {end}) SELECT value FROM series ORDER BY value"
                    ),
                    RowOrder::Ordered,
                )
            }
            "recursive_distinct" => {
                let start = src.any_of("start", int_in(-3..=3));
                let length = src.any_of("length", int_in(0..=8));
                let end = start + length;
                (
                    format!(
                        "WITH RECURSIVE series(value) AS (VALUES ({start}) UNION SELECT value + 1 FROM series CROSS JOIN (VALUES (1), (2)) AS branches(branch) WHERE value < {end}) SELECT value FROM series ORDER BY value"
                    ),
                    RowOrder::Ordered,
                )
            }
            _ => unreachable!(),
        },
    )
}

fn generate_set_operation(src: &mut Source) -> (String, RowOrder) {
    src.select(
        "set_operation",
        &[
            "UNION",
            "UNION ALL",
            "INTERSECT",
            "INTERSECT ALL",
            "EXCEPT",
            "EXCEPT ALL",
        ],
        |src, operator, _| {
            let left = src.any_of("left", int_in(-5..=5));
            let shared = src.any_of("shared", int_in(-5..=5));
            let right = src.any_of("right", int_in(-5..=5));
            let mut sql = format!(
                "VALUES ({left}), ({shared}), ({shared}), (NULL) {operator} VALUES ({right}), ({shared}), (NULL) ORDER BY 1 NULLS FIRST"
            );
            if let Some(limit) = src.maybe("limit", |src| row_count(src, true, false)) {
                sql.push_str(&format!(" LIMIT {limit}"));
            }
            if let Some(offset) = src.maybe("offset", |src| row_count(src, true, true)) {
                sql.push_str(&format!(" OFFSET {offset}"));
            }
            (sql, RowOrder::Ordered)
        },
    )
}

fn generate_json_document(src: &mut Source, depth: usize) -> String {
    if depth == 0 {
        return src
            .choose(
                "json_scalar",
                &[
                    "null",
                    "true",
                    "false",
                    "0",
                    "-0.00",
                    "1e+100",
                    "1e100000",
                    "999999999999999999999999999999999999999999",
                    r#""Привет 🌍""#,
                    r#""escaped\\ntext""#,
                    r#""\ud800""#,
                ],
            )
            .expect("JSON scalar choices are non-empty")
            .0
            .to_string();
    }
    let whitespace = *src
        .choose("json_whitespace", &["", " ", "  ", "\n\t"])
        .expect("JSON whitespace choices are non-empty")
        .0;
    match src.any_of("json_kind", int_in(0..=2)) {
        0 => generate_json_document(src, 0),
        1 => format!(
            "[{whitespace}{}{whitespace},{whitespace}{}{whitespace}]",
            generate_json_document(src, depth - 1),
            generate_json_document(src, depth - 1),
        ),
        _ => format!(
            "{{{whitespace}\"key\"{whitespace}:{whitespace}{}{whitespace},{whitespace}\"key\"{whitespace}:{whitespace}{}{whitespace}}}",
            generate_json_document(src, depth - 1),
            generate_json_document(src, depth - 1),
        ),
    }
}

#[cfg(test)]
fn generate_trigger_tree(src: &mut Source, depth: usize) -> String {
    if depth != 0 && src.any("nested_if") {
        let pivot = src.any_of("pivot", int_in(-5..=15));
        let left = generate_trigger_tree(src, depth - 1);
        let right = generate_trigger_tree(src, depth - 1);
        format!("IF NEW.value < {pivot} THEN {left} ELSE {right} END IF;")
    } else {
        let delta = src.any_of("delta", int_in(-3..=5));
        format!("NEW.value := NEW.value + {delta};")
    }
}

#[cfg(test)]
fn generate_trigger_function(src: &mut Source, table: &str) -> String {
    let fallback = src.any_of("fallback", int_in(1..=9));
    let tree = generate_trigger_tree(src, 2);
    format!(
        "CREATE FUNCTION {table}_trigger_fn() RETURNS TRIGGER AS $$ \
         BEGIN \
           IF NEW.label IS NULL THEN \
             RETURN NULL; \
           ELSIF NEW.value IS NULL THEN \
             NEW.value := {fallback}; \
           ELSE \
             {tree} \
           END IF; \
           RETURN NEW; \
         END; \
         $$ LANGUAGE plpgsql"
    )
}

#[cfg(test)]
fn generate_do_tree(src: &mut Source, table: &str) -> String {
    let inserted = src.any_of("inserted", int_in(-5..=15));
    let delta = src.any_of("update_delta", int_in(-3..=5));
    let label = if src.any("null_insert_label") {
        "NULL"
    } else {
        "'generated'"
    };
    let assignment = if src.any("equals_assignment") {
        "="
    } else {
        ":="
    };
    let branch_shift = src.any_of("branch_shift", int_in(0..=2));
    let nested = if src.any("nested_do_if") {
        format!(
            "IF label IS NOT NULL THEN \
               UPDATE {table} SET value = value + {delta} WHERE id = 3; \
             ELSE \
               DELETE FROM {table} WHERE id = 3; \
             END IF;"
        )
    } else {
        format!("UPDATE {table} SET value = value + {delta} WHERE id = 3;")
    };
    format!(
        "DO $$ \
         DECLARE \
           affected BIGINT; \
           observed BIGINT; \
           label TEXT := {label}; \
         BEGIN \
           INSERT INTO {table} VALUES (3, {inserted}, label); \
           GET DIAGNOSTICS affected = ROW_COUNT; \
           observed {assignment} affected + {branch_shift}; \
           SELECT label, affected INTO label, affected; \
           IF observed = 1 THEN \
             {nested} \
           ELSIF observed = 0 THEN \
             INSERT INTO {table} VALUES (4, 4, 'fallback'); \
           ELSE \
             INSERT INTO {table} VALUES (5, observed, 'else'); \
           END IF; \
         END; \
         $$"
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
            "cte",
            "set_operation",
            "foreign_join",
        ],
        |src, select_body, _| match select_body {
            "core" => generate_select_core(src, table),
            "distinct" => generate_distinct(src, table),
            "aggregate" => generate_aggregate(src, table),
            "join" => generate_join(src, table),
            "subquery" => generate_subquery(src, table),
            "cte" => generate_cte(src, table),
            "set_operation" => generate_set_operation(src),
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
                src.any_of("milliseconds", int_in(1..=1000))
            ),
            "milliseconds" => format!(
                "SET lock_timeout = '{}ms'",
                src.any_of("milliseconds", int_in(1..=1000))
            ),
            "seconds" => format!(
                "SET lock_timeout = '{}s'",
                src.any_of("seconds", int_in(1..=3))
            ),
            _ => unreachable!(),
        },
    )
}

fn local_timeout_sql(src: &mut Source) -> String {
    src.select(
        "local_timeout",
        &["lock_milliseconds", "lock_seconds", "statement_minutes"],
        |src, timeout, _| match timeout {
            "lock_milliseconds" => format!(
                "SET LOCAL lock_timeout = '{}ms'",
                src.any_of("milliseconds", int_in(1000..=5000))
            ),
            "lock_seconds" => format!(
                "SET LOCAL lock_timeout = '{}s'",
                src.any_of("seconds", int_in(1..=5))
            ),
            "statement_minutes" => format!(
                "SET LOCAL statement_timeout = '{}min'",
                src.any_of("minutes", int_in(1..=30))
            ),
            _ => unreachable!(),
        },
    )
}

struct DdlModel {
    table: String,
    exists: bool,
    transaction_start: Option<bool>,
}

fn generate_ddl(model: &mut DdlModel) -> String {
    if model.exists {
        model.exists = false;
        format!("DROP TABLE {}", model.table)
    } else {
        model.exists = true;
        format!("CREATE TABLE {} (id INTEGER PRIMARY KEY)", model.table)
    }
}

fn generate_statement(
    src: &mut Source,
    table: &TableSchema,
    foreign_tables: &ForeignTables,
    next_key: &mut i64,
    next_child_key: &mut i64,
    in_transaction: &mut bool,
    ddl: &mut DdlModel,
) -> (String, RowOrder) {
    let statements: &[&str] = if *in_transaction {
        &[
            "insert", "select", "update", "delete", "ddl", "set", "commit", "rollback",
        ]
    } else {
        &[
            "insert", "select", "update", "delete", "ddl", "set", "begin",
        ]
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
            "ddl" => (generate_ddl(ddl), RowOrder::Unordered),
            "set" => {
                let settings: &[&str] = if *in_transaction {
                    &["lock_timeout", "local_timeout"]
                } else {
                    &["session_characteristics", "lock_timeout"]
                };
                let sql = src.select("set", settings, |src, setting, _| match setting {
                    "session_characteristics" => format!(
                        "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {}",
                        isolation_level(src)
                    ),
                    "lock_timeout" => lock_timeout_sql(src),
                    "local_timeout" => local_timeout_sql(src),
                    _ => unreachable!(),
                });
                (sql, RowOrder::Unordered)
            }
            "begin" => {
                *in_transaction = true;
                ddl.transaction_start = Some(ddl.exists);
                let sql = if src.any("explicit_isolation") {
                    format!("BEGIN ISOLATION LEVEL {}", isolation_level(src))
                } else {
                    "BEGIN".into()
                };
                (sql, RowOrder::Unordered)
            }
            "commit" => {
                *in_transaction = false;
                ddl.transaction_start = None;
                ("COMMIT".into(), RowOrder::Unordered)
            }
            "rollback" => {
                *in_transaction = false;
                ddl.exists = ddl
                    .transaction_start
                    .take()
                    .expect("transaction start must be recorded");
                ("ROLLBACK".into(), RowOrder::Unordered)
            }
            _ => unreachable!(),
        },
    )
}

#[cfg(test)]
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

#[cfg(test)]
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
                generate_conflict_free_main_insert(src, table, next_key),
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

fn run_generated_sql_case(
    src: &mut Source,
    runtime: &Runtime,
    postgres: &mut PgConnection,
    table_name: String,
) {
    let mut postgres = PostgresCase {
        connection: postgres,
        runtime,
        table: table_name.clone(),
    };
    runtime
        .block_on(sqlx::raw_sql("RESET ALL").execute(postgres.get_connection()))
        .expect("must reset PostgreSQL settings for a fresh generated case");
    let mut fake = PgFakeConnection::new(Db::create());
    let table = generate_table(src, table_name);
    let foreign_tables = generate_foreign_tables(src, &table);
    let mut next_key = 1;
    let mut next_child_key = 1;
    let mut in_transaction = false;
    let mut ddl = DdlModel {
        table: format!("{}_ddl", table.name),
        exists: false,
        transaction_start: None,
    };
    let create = table.create_sql();
    src.log_value("sql", &create);
    assert_statement(
        runtime,
        postgres.get_connection(),
        &mut fake,
        &create,
        RowOrder::Unordered,
    );
    let insert = generate_main_insert(src, &table, &mut next_key);
    src.log_value("sql", &insert);
    assert_statement(
        runtime,
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
            runtime,
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
            &mut ddl,
        );
        src.log_value("sql", &sql);
        assert_statement(runtime, postgres.get_connection(), &mut fake, &sql, order);
        Effect::Success
    });

    if in_transaction {
        let sql = if src.any("commit_final_transaction") {
            ddl.transaction_start = None;
            "COMMIT"
        } else {
            ddl.exists = ddl
                .transaction_start
                .take()
                .expect("transaction start must be recorded");
            "ROLLBACK"
        };
        assert_statement(
            runtime,
            postgres.get_connection(),
            &mut fake,
            sql,
            RowOrder::Unordered,
        );
    }
}

pub fn fuzz_generated_sql_matches_postgres(src: &mut Source) {
    let database_url = dotenvy::var("PG_FAKE_DATABASE_URL")
        .expect("PG_FAKE_DATABASE_URL must point to PostgreSQL 18 when fuzzing");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&database_url))
        .expect("must connect SQLx to PostgreSQL 18");
    let table_name = "pg_fake_generated_sql_fuzz".to_owned();
    let sql = format!(
        "DROP TABLE IF EXISTS {0}_foreign_child, {0}_foreign_parent, {0}",
        table_name
    );
    runtime
        .block_on(sqlx::raw_sql(sql.as_str()).execute(&mut postgres))
        .expect("must clean PostgreSQL state before a fuzz input");
    run_generated_sql_case(src, &runtime, &mut postgres, table_name);
}

#[test]
fn generated_sql_matches_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
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
        run_generated_sql_case(src, &runtime, &mut postgres.borrow_mut(), table_name);
    });
}

#[test]
fn generated_set_operations_match_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let (sql, order) = generate_set_operation(src);
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            order,
        );
    });
}

#[test]
fn matches_generated_jsonb_normalization_and_comparison() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let left = generate_json_document(src, 3);
        let right = generate_json_document(src, 3);
        let sql = format!(
            "SELECT '{left}'::jsonb::text, '{right}'::jsonb::json::text, '{left}'::jsonb = '{right}'::jsonb, '{left}'::jsonb < '{right}'::jsonb"
        );
        src.log_value("sql", &sql);
        assert_statement_allow_error(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_json_text_and_errors() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let leading = if src.any("json_leading_space") {
            "  "
        } else {
            ""
        };
        let trailing = if src.any("json_trailing_space") {
            "\n"
        } else {
            ""
        };
        let document = format!("{leading}{}{trailing}", generate_json_document(src, 3));
        let sql = format!("SELECT ('{document}'::json)::text");
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );

        let invalid = *src
            .choose(
                "invalid_json",
                &["{", "[1,]", "01", r#""unterminated"#, r#""\u""#],
            )
            .expect("invalid JSON choices are non-empty")
            .0;
        let sql = format!("SELECT '{invalid}'::json");
        src.log_value("invalid_sql", &sql);
        assert_statement_allow_error(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn generated_procedural_trees_match_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    check(|src| {
        let table = format!(
            "pg_fake_procedural_property_{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let mut postgres_connection = postgres.borrow_mut();
        let mut postgres = PostgresCase {
            connection: &mut postgres_connection,
            runtime: &runtime,
            table: table.clone(),
        };
        let mut fake = PgFakeConnection::new(Db::create());
        let function = generate_trigger_function(src, &table);
        let block = generate_do_tree(src, &table);
        for (sql, order) in [
            (
                format!(
                    "CREATE TABLE {table} (id BIGINT PRIMARY KEY, value BIGINT NOT NULL, label TEXT)"
                ),
                RowOrder::Unordered,
            ),
            (function, RowOrder::Unordered),
            (
                format!(
                    "CREATE TRIGGER generated_before BEFORE INSERT OR UPDATE ON {table} \
                     FOR EACH ROW EXECUTE FUNCTION {table}_trigger_fn()"
                ),
                RowOrder::Unordered,
            ),
            (
                format!("INSERT INTO {table} VALUES (1, NULL, 'kept'), (2, 2, NULL)"),
                RowOrder::Unordered,
            ),
            (block, RowOrder::Unordered),
            (
                format!("SELECT id, value, label FROM {table} ORDER BY id"),
                RowOrder::Ordered,
            ),
        ] {
            src.log_value("sql", &sql);
            assert_statement(&runtime, postgres.get_connection(), &mut fake, &sql, order);
        }
    });
}

#[test]
fn generated_alter_table_rewrites_match_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    check(|src| {
        let table = format!(
            "pg_fake_alter_property_{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let mut postgres_connection = postgres.borrow_mut();
        let mut postgres = PostgresCase {
            connection: &mut postgres_connection,
            runtime: &runtime,
            table: table.clone(),
        };
        let mut fake = PgFakeConnection::new(Db::create());
        let values = (0..src.any_of("rows", int_in(1..=6)))
            .map(|id| {
                let value = integer(src, "value");
                format!("({id}, {value})")
            })
            .collect::<Vec<_>>()
            .join(",");
        let default = integer(src, "default");
        let multiplier = src.any_of("multiplier", int_in(1..=5));
        for sql in [
            format!("CREATE TABLE {table} (id INTEGER PRIMARY KEY, value INTEGER)"),
            format!("INSERT INTO {table} VALUES {values}"),
            format!("ALTER TABLE {table} RENAME COLUMN value TO amount"),
            format!(
                "ALTER TABLE {table} ADD COLUMN marker INTEGER DEFAULT {default} NOT NULL, \
                 ALTER COLUMN amount TYPE BIGINT USING amount * {multiplier}"
            ),
            format!(
                "ALTER TABLE {table} ADD CONSTRAINT marker_floor CHECK (marker >= {default}) NOT VALID"
            ),
            format!("ALTER TABLE {table} VALIDATE CONSTRAINT marker_floor"),
            format!("SELECT id, amount, marker FROM {table} ORDER BY id"),
            format!("ALTER TABLE {table} DROP COLUMN marker"),
            format!("SELECT * FROM {table} ORDER BY id"),
        ] {
            src.log_value("sql", &sql);
            assert_statement(
                &runtime,
                postgres.get_connection(),
                &mut fake,
                &sql,
                RowOrder::Ordered,
            );
        }
    });
}

#[test]
fn generated_nested_views_match_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    check(|src| {
        let suffix = format!(
            "{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let table = format!("pg_fake_view_property_source_{suffix}");
        let inner = format!("pg_fake_view_property_inner_{suffix}");
        let outer = format!("pg_fake_view_property_outer_{suffix}");
        let row_count = src.any_of("rows", int_in(1..=6));
        let values = (1..=row_count)
            .map(|id| format!("({id}, {})", integer(src, "value")))
            .collect::<Vec<_>>()
            .join(", ");
        let lower = integer(src, "lower");
        let upper = src.any_of("upper", int_in(1..=6));
        let replacement_lower = integer(src, "replacement_lower");
        let mut postgres = postgres.borrow_mut();
        let mut fake = PgFakeConnection::new(Db::create());

        for sql in [
            format!("CREATE TABLE {table} (id INTEGER, value INTEGER)"),
            format!("INSERT INTO {table} VALUES {values}"),
            format!(
                "CREATE VIEW {inner} (key, amount) AS \
                 SELECT id, value FROM {table} WHERE value >= {lower}"
            ),
            format!(
                "CREATE VIEW {outer} AS \
                 SELECT key, amount FROM {inner} WHERE key <= {upper}"
            ),
            format!("SELECT key, amount FROM {outer} ORDER BY key, amount"),
            format!(
                "WITH {outer} AS (SELECT 999 AS key, 999 AS amount) \
                 SELECT key, amount FROM {outer}"
            ),
            "BEGIN".to_owned(),
            format!(
                "CREATE OR REPLACE VIEW {inner} (key, amount) AS \
                 SELECT id, value FROM {table} WHERE value >= {replacement_lower}"
            ),
            "ROLLBACK".to_owned(),
            format!("SELECT key, amount FROM {outer} ORDER BY key, amount"),
            format!("DROP VIEW {outer}, {inner}"),
            format!("DROP TABLE {table}"),
        ] {
            src.log_value("sql", &sql);
            assert_statement(&runtime, &mut postgres, &mut fake, &sql, RowOrder::Ordered);
        }
    });
}

#[test]
fn generated_partial_unique_indexes_match_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    check(|src| {
        let table = format!(
            "pg_fake_index_property_{}_{}",
            std::process::id(),
            TABLE_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let mut postgres_connection = postgres.borrow_mut();
        let mut postgres = PostgresCase {
            connection: &mut postgres_connection,
            runtime: &runtime,
            table: table.clone(),
        };
        let mut fake = PgFakeConnection::new(Db::create());
        let key_count = src.any_of("key_count", int_in(1_usize..=4));
        let keys = (1..=key_count)
            .map(|index| {
                let direction = if src.any("descending") { "DESC" } else { "ASC" };
                format!("key_{index} {direction}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let key_values = "1, 1, 1, 1";
        let predicates = [
            ("active", "true, 1, 1", "false, 1, 1"),
            ("deleted_at IS NULL", "true, NULL, 1", "true, 1, 1"),
            ("deleted_at IS NOT NULL", "true, 1, 1", "true, NULL, 1"),
            ("state = 1", "true, 1, 1", "true, 1, 2"),
            ("state != 2", "true, 1, 1", "true, 1, 2"),
            ("state IN (1, 2, 3)", "true, 1, 1", "true, 1, 4"),
            (
                "(active AND state >= 0) OR deleted_at IS NULL",
                "true, 1, 0",
                "false, 1, -1",
            ),
        ];
        let ((predicate, qualifying, non_qualifying), _) =
            src.choose("predicate", &predicates).unwrap();
        let definitions = (1..=4)
            .map(|index| format!("key_{index} INTEGER"))
            .collect::<Vec<_>>()
            .join(", ");
        let statements = [
            (
                format!(
                    "CREATE TABLE {table} ({definitions}, active BOOLEAN, deleted_at INTEGER, state INTEGER, payload INTEGER)"
                ),
                false,
            ),
            (
                format!(
                    "INSERT INTO {table} VALUES ({key_values}, {qualifying}, 10), ({key_values}, {non_qualifying}, 20)"
                ),
                false,
            ),
            (
                format!(
                    "CREATE UNIQUE INDEX {table}_key ON {table} ({keys}) INCLUDE (payload) WHERE {predicate}"
                ),
                false,
            ),
            (
                format!("INSERT INTO {table} VALUES ({key_values}, {qualifying}, 30)"),
                true,
            ),
            (
                format!("INSERT INTO {table} VALUES ({key_values}, {non_qualifying}, 40)"),
                false,
            ),
            (
                format!(
                    "INSERT INTO {table} VALUES ({key_values}, {qualifying}, 50) \
                 ON CONFLICT ({}) WHERE {predicate} DO UPDATE SET payload = excluded.payload",
                    (1..=key_count)
                        .map(|index| format!("key_{index}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                false,
            ),
            (
                format!("SELECT payload FROM {table} ORDER BY payload"),
                false,
            ),
        ];
        for (sql, allow_error) in statements {
            src.log_value("sql", &sql);
            if allow_error {
                assert_statement_allow_error(
                    &runtime,
                    postgres.get_connection(),
                    &mut fake,
                    &sql,
                    RowOrder::Ordered,
                );
            } else {
                assert_statement(
                    &runtime,
                    postgres.get_connection(),
                    &mut fake,
                    &sql,
                    RowOrder::Ordered,
                );
            }
        }
    });
}

#[test]
fn generated_interleaved_transaction_snapshots_match_postgres() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
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
            let session = src.any_of("session", int_in(0_usize..=2));
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
        let min_value = src.any_of("min_value", int_in(-20_i64..=-1));
        let max_value = src.any_of("max_value", int_in(1_i64..=20));
        let start_value = src.any_of("start_value", int_in(min_value..=max_value));
        let cycles = [false, true];
        let (cycle, _) = src.choose("cycle", &cycles).unwrap();
        let calls = src.any_of("calls", int_in(1..=8));
        let db = pg_fake::Db::create();
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

#[test]
fn generated_temporary_relation_lifetimes_are_session_local() {
    check(|src| {
        let first_value = src.any_of("first_value", int_in(-100_i32..=100));
        let second_value = src.any_of("second_value", int_in(-100_i32..=100));
        let db = pg_fake::Db::create();
        let mut first = db.create_session();
        let mut second = db.create_session();
        first
            .execute("CREATE TEMP TABLE property_temp (value INTEGER)")
            .unwrap();
        second
            .execute("CREATE TEMP TABLE property_temp (value INTEGER)")
            .unwrap();
        first
            .execute(&format!(
                "INSERT INTO pg_temp.property_temp VALUES ({first_value})"
            ))
            .unwrap();
        second
            .execute(&format!(
                "INSERT INTO property_temp VALUES ({second_value})"
            ))
            .unwrap();
        assert_eq!(
            first
                .query("SELECT value FROM property_temp", &[])
                .unwrap()
                .rows,
            vec![vec![pg_fake::value::Value::Int4(first_value)]]
        );
        assert_eq!(
            second
                .query("SELECT value FROM pg_temp.property_temp", &[])
                .unwrap()
                .rows,
            vec![vec![pg_fake::value::Value::Int4(second_value)]]
        );

        let mut transaction = first.begin().unwrap();
        transaction
            .execute("CREATE TEMP TABLE property_drop (value INTEGER) ON COMMIT DROP")
            .unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            first
                .query("SELECT * FROM pg_temp.property_drop", &[])
                .unwrap_err()
                .sqlstate,
            pg_fake::error::SqlState::UndefinedTable
        );
    });
}

#[test]
fn matches_generated_json_operations() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let left = generate_json_document(src, 3);
        let right = generate_json_document(src, 2);
        let index: i32 = src.any_of("index", int_in(-5..=5));
        let sql = match src.any_of("json_operation", int_in(0..=6)) {
            0 => format!(
                "SELECT '{left}'::jsonb @> '{right}', '{left}'::jsonb <@ '{right}', '{left}'::jsonb || '{right}'"
            ),
            1 => format!(
                "SELECT '{left}'::json -> {index}, '{left}'::json ->> 'a', '{left}'::jsonb #> '{{a,{index}}}', '{left}'::jsonb #>> '{{}}'"
            ),
            2 => format!(
                "SELECT '{left}'::jsonb - {index}, '{left}'::jsonb - 'a', '{left}'::jsonb #- '{{a,{index}}}'"
            ),
            3 => format!(
                "SELECT jsonb_set('{left}','{{a,{index}}}','{right}'), jsonb_typeof('{left}'), '{left}'::jsonb ?| '{{a,b,NULL}}', '{left}'::jsonb ?& '{{a,b,NULL}}'"
            ),
            4 => format!("SELECT * FROM json_each_text('{left}') AS e ORDER BY key,value"),
            5 => format!(
                "SELECT * FROM jsonb_array_elements('{left}') WITH ORDINALITY AS e ORDER BY ordinality"
            ),
            _ => format!(
                "SELECT json_build_array('{left}'::json,{index},NULL), jsonb_build_object('a','{left}'::jsonb,'b','{right}'::jsonb), json_array_length('{left}')"
            ),
        };
        src.log_value("sql", &sql);
        assert_statement_allow_error(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_migration_data_transform_queries() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let first = src.any_of("first", int_in(-20_i32..=20));
        let second = src.any_of("second", int_in(-20_i32..=20));
        let third = src.any_of("third", int_in(-20_i32..=20));
        let transform = src.any_of("transform", int_in(0..=8));
        let sql = match transform {
            0 => format!(
                "SELECT ({first}::integer * {second}::numeric)::bigint, \
                        {first}::integer IS DISTINCT FROM {second}::integer, \
                        {third} IN ({first}, {second}, NULL), \
                        coalesce(NULL::integer, {third}) > {second}"
            ),
            1 => format!(
                "SELECT value, row_number() OVER (ORDER BY value DESC NULLS FIRST) \
                 FROM (VALUES ({first}), (NULL), ({second}), ({third})) AS generated(value) \
                 ORDER BY 2"
            ),
            2 => format!(
                "SELECT value, count(*) OVER (PARTITION BY value) \
                 FROM (VALUES ({first}), (NULL), ({second}), ({third}), (NULL)) AS generated(value) \
                 ORDER BY value NULLS FIRST"
            ),
            3 => format!(
                "SELECT count(*), max(value), \
                        string_agg(label, ':' ORDER BY value DESC NULLS LAST) \
                 FROM (VALUES ({first}, 'a'), ({second}, NULL), ({third}, 'c')) AS generated(value, label)"
            ),
            4 => format!(
                "WITH left_side AS MATERIALIZED (SELECT {first} AS value), \
                      right_side AS (SELECT {second} AS value) \
                 SELECT EXISTS (SELECT 1 FROM right_side WHERE value = {second}), \
                        (SELECT max(value) FROM left_side LIMIT 1), \
                        'ABC-{third}' ~ '^ABC--?[0-9]{{1,2}}$'"
            ),
            5 => format!(
                "SELECT extract(epoch FROM '1970-01-01 00:00:00.{:06}+00'::timestamptz), \
                        extract(epoch FROM '1969-12-31 23:59:59.{:06}+00'::timestamptz)",
                first.unsigned_abs() * 10_000,
                second.unsigned_abs() * 10_000,
            ),
            6 => format!(
                "SELECT 'ABC' ~ '^ABC(-[0-9]{{1,{}}})?$', \
                        'ABC-{}' ~ '^ABC(-[0-9]{{1,{}}})?$'",
                first.unsigned_abs() % 9 + 1,
                second.unsigned_abs(),
                third.unsigned_abs() % 9 + 1,
            ),
            7 => format!("SELECT 'x' ~ 'x{{{}}}'", 256 + first.unsigned_abs()),
            _ => format!(
                "SELECT count() OVER (PARTITION BY value) \
                 FROM (VALUES ({first}), ({second}), ({third})) AS generated(value)"
            ),
        };
        src.log_value("sql", &sql);
        if transform >= 7 {
            assert_statement_allow_error(
                &runtime,
                &mut postgres.borrow_mut(),
                &mut fake.borrow_mut(),
                &sql,
                RowOrder::Ordered,
            );
        } else {
            assert_statement(
                &runtime,
                &mut postgres.borrow_mut(),
                &mut fake.borrow_mut(),
                &sql,
                RowOrder::Ordered,
            );
        }
    });
}

#[cfg(feature = "time")]
#[test]
fn matches_generated_offset_datetime_parameters() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let seconds = src.any_of("unix_seconds", int_in(-2_208_988_800_i64..=4_102_444_800));
        let nanoseconds = src.any_of("nanoseconds", int_in(0_i64..=999_999_999));
        let value = time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds),
        )
        .unwrap();
        src.log_value("value", &value);
        let expected = runtime
            .block_on(
                sqlx::query_scalar::<_, time::OffsetDateTime>("SELECT $1::timestamptz")
                    .bind(value)
                    .fetch_one(&mut *postgres.borrow_mut()),
            )
            .unwrap();
        let actual = runtime
            .block_on(
                sqlx::query_scalar::<_, time::OffsetDateTime>("SELECT $1::timestamptz")
                    .bind(value)
                    .fetch_one(&mut *fake.borrow_mut()),
            )
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.offset(), time::UtcOffset::UTC);
    });
}

#[test]
fn matches_generated_runtime_expressions() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    assert_statement(
        &runtime,
        &mut postgres.borrow_mut(),
        &mut fake.borrow_mut(),
        "SET TIME ZONE 'UTC'",
        RowOrder::Ordered,
    );
    check(|src| {
        let case = src.any_of("case", int_in(0..=5));
        let epoch = src.any_of("epoch", int_in(-2_208_988_800_i64..=4_102_444_800));
        let fraction = src.any_of("fraction", int_in(0..=999_999));
        let unit = [
            "microseconds",
            "milliseconds",
            "second",
            "minute",
            "hour",
            "day",
            "week",
            "month",
            "quarter",
            "year",
        ][src.any_of("unit", int_in(0..=9))];
        let zone = [
            "UTC",
            "+03:00",
            "-05:30",
            "America/New_York",
            "Europe/Berlin",
            "Asia/Kolkata",
        ][src.any_of("zone", int_in(0..=5))];
        let sql = match case {
            0 => format!(
                "SELECT to_timestamp({epoch}.{fraction:06}), floor({epoch}.{fraction:06}), floor({epoch}.{fraction:06}::double precision), date_trunc('{unit}', to_timestamp({epoch}.{fraction:06}))"
            ),
            1 => format!(
                "SELECT date_trunc('{unit}', to_timestamp({epoch}.{fraction:06}), '{zone}'), to_timestamp({epoch}) AT TIME ZONE '{zone}', to_char(to_timestamp({epoch}.{fraction:06}), 'YYYY-MM-DD HH24:MI:SS.US OF')"
            ),
            2 => {
                let text_length = src.any_of("text_length", int_in(0..=12));
                let pattern_length = src.any_of("pattern_length", int_in(0..=12));
                let alphabet = ['a', 'b', 'A', 'B', '%', '_', '\\'];
                let text = (0..text_length)
                    .map(|_| alphabet[src.any_of("text_char", int_in(0..=6))])
                    .collect::<String>();
                let pattern = (0..pattern_length)
                    .map(|_| alphabet[src.any_of("pattern_char", int_in(0..=6))])
                    .collect::<String>();
                let escape = ["\\", "", "a", "%", "_"][src.any_of("escape", int_in(0..=4))];
                format!(
                    "SELECT '{text}' LIKE '{pattern}' ESCAPE '{escape}', '{text}' ILIKE '{pattern}' ESCAPE '{escape}', '{text}' NOT LIKE '{pattern}' ESCAPE '{escape}'"
                )
            }
            3 => {
                let count = src.any_of("count", int_in(0..=10));
                let lower = src.any_of("lower", int_in(0..=5));
                let upper = lower + src.any_of("extra", int_in(0..=5));
                let text = "aB".repeat(count);
                let pattern = format!("^([a-z]{{{lower},{upper}}})?$");
                format!(
                    "SELECT '{text}' ~ '{pattern}', '{text}' ~* '{pattern}', '{text}' !~ '{pattern}', '{text}' !~* '{pattern}', regexp_like('{text}', '{pattern}', 'i')"
                )
            }
            4 => format!(
                "SELECT floor(value), string_agg(to_char(to_timestamp(value), 'SS.US'), ':' ORDER BY value) FILTER (WHERE value::text LIKE '%1%') FROM (VALUES ({epoch}.{fraction:06}), (NULL::numeric), ({epoch}.1)) AS input(value) GROUP BY floor(value) ORDER BY 1"
            ),
            _ => format!(
                "SELECT to_char(date_trunc('{unit}', to_timestamp({epoch})), 'YYYY-MM-DD HH24:MI:SS'), NULL::text ILIKE '%', regexp_like(NULL::text, '['), floor(NULL::numeric)"
            ),
        };
        src.log_value("sql", &sql);
        if case == 2 {
            assert_statement_allow_error(
                &runtime,
                &mut postgres.borrow_mut(),
                &mut fake.borrow_mut(),
                &sql,
                RowOrder::Ordered,
            );
        } else {
            assert_statement(
                &runtime,
                &mut postgres.borrow_mut(),
                &mut fake.borrow_mut(),
                &sql,
                RowOrder::Ordered,
            );
        }
    });
}

#[test]
fn matches_generated_lateral_joins() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let parents = (0..src.any_of("parents", int_in(1..=6)))
            .map(|id| {
                format!(
                    "({id}, {})",
                    if src.any("null") {
                        "NULL::integer".to_owned()
                    } else {
                        integer(src, "value").to_string()
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let children = (0..src.any_of("children", int_in(1..=8)))
            .map(|id| {
                format!(
                    "({id}, {}, {})",
                    src.any_of("parent", int_in(0..=6)),
                    integer(src, "amount")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let limit = src.any_of("limit", int_in(0..=4));
        let offset = src.any_of("offset", int_in(0..=3));
        let shape = src.any_of("shape", int_in(0..=6));
        let inner = match shape {
            0 => format!("SELECT c.id AS value FROM (VALUES {children}) c(id, parent_id, amount) WHERE c.parent_id = p.id ORDER BY c.amount, c.id LIMIT {limit} OFFSET {offset}"),
            1 => format!("SELECT sum(c.amount) AS value FROM (VALUES {children}) c(id, parent_id, amount) WHERE c.parent_id = p.id"),
            2 => "SELECT p.amount + y.value AS value FROM (VALUES (1), (2)) y(value) WHERE p.amount IS NOT NULL".to_owned(),
            3 => "SELECT z.value FROM (VALUES (1)) y(value) CROSS JOIN LATERAL (SELECT p.amount + y.value AS value) z".to_owned(),
            4 => "WITH y AS (SELECT p.amount AS value) SELECT value FROM y".to_owned(),
            5 => "SELECT p.amount AS value FROM (VALUES (3)) p(amount) UNION ALL SELECT p.amount".to_owned(),
            _ => "WITH RECURSIVE y(value) AS (SELECT p.id UNION ALL SELECT value + 1 FROM y WHERE value < 4) SELECT value FROM y".to_owned(),
        };
        let join = if src.any("left") { "LEFT" } else { "INNER" };
        let sql = format!(
            "SELECT p.id, x.value FROM (VALUES {parents}) p(id, amount) {join} JOIN LATERAL ({inner}) x ON TRUE ORDER BY p.id, x.value"
        );
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_skip_locked_queues() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let holder = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let worker = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let db = Db::create();
    let fake_holder = RefCell::new(PgFakeConnection::new(db.clone()));
    let fake_worker = RefCell::new(PgFakeConnection::new(db.clone()));
    let observer = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake_observer = RefCell::new(PgFakeConnection::new(db));
    for sql in [
        "CREATE TABLE generated_queue(id INT PRIMARY KEY, priority INT)",
        "INSERT INTO generated_queue VALUES(1,5),(2,2),(3,8),(4,4),(5,7),(6,1),(7,3),(8,6)",
    ] {
        assert_statement(
            &runtime,
            &mut holder.borrow_mut(),
            &mut fake_holder.borrow_mut(),
            sql,
            RowOrder::Ordered,
        );
    }
    check(|src| {
        let held =
            ["KEY SHARE", "SHARE", "NO KEY UPDATE", "UPDATE"][src.any_of("held", int_in(0..=3))];
        let requested = ["KEY SHARE", "SHARE", "NO KEY UPDATE", "UPDATE"]
            [src.any_of("requested", int_in(0..=3))];
        let divisor = src.any_of("divisor", int_in(2..=5));
        let remainder = src.any_of("remainder", int_in(0..=1));
        let limit = src.any_of("limit", int_in(0..=5));
        let offset = src.any_of("offset", int_in(0..=3));
        let order = if src.any("descending") { "DESC" } else { "ASC" };
        let shape = src.any_of("shape", int_in(0..=6));
        let inner_limit = src.any_of("inner_limit", int_in(0..=8));
        let inner_offset = src.any_of("inner_offset", int_in(0..=3));
        let locked = format!(
            "SELECT * FROM generated_queue ORDER BY priority {order},id LIMIT {inner_limit} OFFSET {inner_offset} FOR {requested} SKIP LOCKED"
        );
        let sql = match shape {
            0 | 1 => {
                let source = if shape == 0 {
                    "generated_queue q"
                } else {
                    "(SELECT * FROM generated_queue) q"
                };
                format!(
                    "SELECT q.id FROM {source} ORDER BY q.priority {order},q.id LIMIT {limit} OFFSET {offset} FOR {requested} OF q SKIP LOCKED"
                )
            }
            2 => format!(
                "SELECT id FROM ({locked}) q WHERE id%2={remainder} LIMIT {limit} OFFSET {offset}"
            ),
            3 => format!("SELECT id FROM ({locked}) q LIMIT {limit} FOR {held} SKIP LOCKED"),
            4 => format!("WITH q AS ({locked}) SELECT id FROM q LIMIT {limit}"),
            5 => format!("SELECT q.id FROM ({locked}) q CROSS JOIN (VALUES(1)) v(n) LIMIT {limit}"),
            _ => format!("SELECT EXISTS({locked}) AS present"),
        };
        for sql in [
            "BEGIN".to_owned(),
            format!(
                "SELECT id FROM generated_queue WHERE id%{divisor}={remainder} ORDER BY id FOR {held}"
            ),
        ] {
            assert_statement(
                &runtime,
                &mut holder.borrow_mut(),
                &mut fake_holder.borrow_mut(),
                &sql,
                RowOrder::Ordered,
            );
        }
        assert_statement(
            &runtime,
            &mut worker.borrow_mut(),
            &mut fake_worker.borrow_mut(),
            "BEGIN",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut worker.borrow_mut(),
            &mut fake_worker.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
        for id in 1..=8 {
            assert_statement_allow_error(
                &runtime,
                &mut observer.borrow_mut(),
                &mut fake_observer.borrow_mut(),
                &format!("SELECT id FROM generated_queue WHERE id={id} FOR UPDATE NOWAIT"),
                RowOrder::Ordered,
            );
        }
        assert_statement(
            &runtime,
            &mut worker.borrow_mut(),
            &mut fake_worker.borrow_mut(),
            "ROLLBACK",
            RowOrder::Ordered,
        );
        assert_statement(
            &runtime,
            &mut holder.borrow_mut(),
            &mut fake_holder.borrow_mut(),
            "ROLLBACK",
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_advisory_transactions() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        (0..3)
            .map(|_| {
                runtime
                    .block_on(PgConnection::connect(&server.url))
                    .unwrap()
            })
            .collect::<Vec<_>>(),
    );
    let db = Db::create();
    let fake = RefCell::new(
        (0..3)
            .map(|_| PgFakeConnection::new(db.clone()))
            .collect::<Vec<_>>(),
    );
    check(|src| {
        let mut postgres = postgres.borrow_mut();
        let mut fake = fake.borrow_mut();
        for session in 0..3 {
            assert_statement(
                &runtime,
                &mut postgres[session],
                &mut fake[session],
                "BEGIN",
                RowOrder::Ordered,
            );
        }
        let steps = src.any_of("steps", int_in(1..=16));
        for _ in 0..steps {
            let session = src.any_of("session", int_in(0..=2));
            let operation = src.any_of("operation", int_in(0..=5));
            if operation >= 4 {
                for sql in [if operation == 4 { "COMMIT" } else { "ROLLBACK" }, "BEGIN"] {
                    assert_statement(
                        &runtime,
                        &mut postgres[session],
                        &mut fake[session],
                        sql,
                        RowOrder::Ordered,
                    );
                }
                continue;
            }
            let first = src.any_of("first_key", int_in(-2..=2));
            let second = src.any_of("second_key", int_in(-2..=2));
            let key = if src.any("integer_pair") {
                format!("{first},{second}")
            } else {
                let key = (i64::from(first) << 32) | i64::from(second as u32);
                format!("'{key}'::BIGINT")
            };
            let function = if operation % 2 == 0 {
                "pg_try_advisory_xact_lock"
            } else {
                "pg_try_advisory_xact_lock_shared"
            };
            let sql = if operation >= 2 {
                format!("SELECT {function}({key}),{function}({key})")
            } else {
                format!("SELECT {function}({key})")
            };
            assert_statement(
                &runtime,
                &mut postgres[session],
                &mut fake[session],
                &sql,
                RowOrder::Ordered,
            );
        }
        for session in 0..3 {
            assert_statement(
                &runtime,
                &mut postgres[session],
                &mut fake[session],
                "ROLLBACK",
                RowOrder::Ordered,
            );
        }
    });
}

#[test]
fn matches_generated_text_hashes() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let alphabet = ['a', 'Z', '0', '-', 'é', 'Ж', '🙂'];
        let length = src.any_of("length", int_in(0..=128));
        let value = (0..length)
            .map(|_| alphabet[src.any_of("character", int_in(0..=alphabet.len() - 1))])
            .collect::<String>();
        let seed = src.any_of("seed", int_in(i64::MIN..=i64::MAX));
        let pair = src.any_of("pair", int_in(i32::MIN..=i32::MAX));
        let sql = format!(
            "SELECT hashtext('{value}'), hashtextextended('{value}',{seed}), \
                    pg_try_advisory_xact_lock(hashtext('{value}')), \
                    pg_try_advisory_xact_lock(hashtextextended('{value}',{seed})), \
                    pg_try_advisory_xact_lock({pair},hashtext('{value}'))"
        );
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_bigint_and_uuid_arrays() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let integers = (0..src.any_of("integer_count", int_in(1..=8)))
            .map(|_| {
                if src.any("integer_null") {
                    "NULL::BIGINT".to_owned()
                } else {
                    format!("{}::BIGINT", src.any_of("integer", int_in(-100_i64..=100)))
                }
            })
            .collect::<Vec<_>>();
        let uuids = (0..src.any_of("uuid_count", int_in(1..=8)))
            .map(|_| {
                if src.any("uuid_null") {
                    "NULL::UUID".to_owned()
                } else {
                    format!(
                        "'00000000-0000-4000-8000-{:012x}'::UUID",
                        src.any_of("uuid", int_in(0_u64..=0xffff_ffff))
                    )
                }
            })
            .collect::<Vec<_>>();
        let integer_texts = integers
            .iter()
            .map(|value| {
                if value == "NULL::BIGINT" {
                    "NULL".to_owned()
                } else {
                    format!(
                        "'{}'",
                        value
                            .strip_suffix("::BIGINT")
                            .expect("BIGINT literal has a type suffix")
                    )
                }
            })
            .collect::<Vec<_>>();
        let uuid_texts = uuids
            .iter()
            .map(|value| {
                value
                    .strip_suffix("::UUID")
                    .map_or_else(|| "NULL".to_owned(), str::to_owned)
            })
            .collect::<Vec<_>>();
        let sql = format!(
            "SELECT ARRAY[{}], ARRAY[{}], ARRAY[{}]::TEXT[]::BIGINT[], ARRAY[{}]::TEXT[]::UUID[], ARRAY[]::BIGINT[]",
            integers.join(","),
            uuids.join(","),
            integer_texts.join(","),
            uuid_texts.join(","),
        );
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_required_array_queries() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .unwrap(),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let rows = (0..src.any_of("row_count", int_in(1..=8)))
            .map(|index| {
                let value = if src.any("value_null") {
                    "NULL::BIGINT".to_owned()
                } else {
                    format!("{}::BIGINT", src.any_of("value", int_in(-100_i64..=100)))
                };
                let included: bool = src.any("included");
                format!("({value},{index},{included})")
            })
            .collect::<Vec<_>>();
        let target = src.any_of("target", int_in(0_u64..=15));
        let candidates = (0..src.any_of("candidate_count", int_in(0..=8)))
            .map(|_| {
                if src.any("candidate_null") {
                    "NULL::UUID".to_owned()
                } else {
                    format!(
                        "'00000000-0000-4000-8000-{:012x}'::UUID",
                        src.any_of("candidate", int_in(0_u64..=15))
                    )
                }
            })
            .collect::<Vec<_>>();
        let candidates = if candidates.is_empty() {
            "ARRAY[]::UUID[]".to_owned()
        } else {
            format!("ARRAY[{}]", candidates.join(","))
        };
        let index = src.any_of("subscript", int_in(-2_i32..=10));
        let target = format!("'00000000-0000-4000-8000-{target:012x}'::UUID");
        let sql = format!(
            "SELECT (SELECT (array_agg(value ORDER BY ordering DESC) \
                                FILTER (WHERE included))[{index}] \
                     FROM (VALUES {}) input(value,ordering,included)), \
                    {target} = ANY({candidates}), \
                    {target} <> ALL({candidates})",
            rows.join(",")
        );
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}

#[test]
fn matches_generated_pg_lsn_values_and_arithmetic() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let postgres = RefCell::new(
        runtime
            .block_on(PgConnection::connect(&server.url))
            .expect("must connect SQLx to PostgreSQL 18 once"),
    );
    let fake = RefCell::new(PgFakeConnection::new(Db::create()));
    check(|src| {
        let value = src.any_of("lsn", int_in(1_000_u64..=u64::MAX - 1_000));
        let other = src.any_of("other_lsn", int_in(0_u64..=u64::MAX));
        let offset = src.any_of("offset", int_in(-1_000_i64..=1_000));
        let format_lsn = |value: u64| format!("{:X}/{:X}", value >> 32, value as u32);
        let value = format_lsn(value);
        let other = format_lsn(other);
        let sql = format!(
            "SELECT '{value}'::pg_lsn::text, \
                    '{value}'::pg_lsn < '{other}'::pg_lsn, \
                    '{value}'::pg_lsn - '{other}'::pg_lsn, \
                    ('{value}'::pg_lsn + {offset}::numeric)::text, \
                    minimum::text, maximum::text \
             FROM (SELECT min(position) AS minimum, max(position) AS maximum \
                   FROM (VALUES ('{value}'::pg_lsn), ('{other}'::pg_lsn)) AS positions(position)) AS bounds"
        );
        src.log_value("sql", &sql);
        assert_statement(
            &runtime,
            &mut postgres.borrow_mut(),
            &mut fake.borrow_mut(),
            &sql,
            RowOrder::Ordered,
        );
    });
}
