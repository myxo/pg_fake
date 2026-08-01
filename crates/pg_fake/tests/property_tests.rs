use std::{
    cell::RefCell,
    env,
    num::NonZeroI32,
    path::PathBuf,
    str::FromStr,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use bigdecimal::BigDecimal;
use chaos_theory::{Effect, Source, check};
use pg_fake::{
    api::{Db, Session},
    parser::{self, Statement},
    value::Value,
};
use postgres::{Client, NoTls, SimpleQueryMessage};
use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Affected(u64),
    Rows {
        values: Vec<Vec<Option<String>>>,
        type_oids: Option<Vec<u32>>,
    },
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

#[derive(Debug, Clone, Copy, chaos_theory::Arbitrary)]
enum TextValue {
    Empty,
    Lower,
    Mixed,
    Words,
    Quote,
    Unicode,
}

impl TextValue {
    fn sql(self) -> String {
        let value = match self {
            TextValue::Empty => "",
            TextValue::Lower => "a",
            TextValue::Mixed => "MiXeD",
            TextValue::Words => "two words",
            TextValue::Quote => "quote's",
            TextValue::Unicode => "東京",
        };
        format!("'{}'", value.replace('\'', "''"))
    }
}

#[derive(Debug, Clone, Copy, chaos_theory::Arbitrary)]
enum CharValue {
    Empty,
    One,
    Word,
    Eight,
}

impl CharValue {
    fn sql(self) -> &'static str {
        match self {
            CharValue::Empty => "''",
            CharValue::One => "'x'",
            CharValue::Word => "'fixed'",
            CharValue::Eight => "'eight888'",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
struct GeneratedRow {
    small_value: Option<i16>,
    int_value: Option<i32>,
    big_value: Option<i64>,
    numeric_hundredths: Option<i16>,
    real_hundredths: Option<i16>,
    double_hundredths: Option<i16>,
    flag: Option<bool>,
    text_value: Option<TextValue>,
    varchar_value: Option<TextValue>,
    char_value: Option<CharValue>,
    bytes: Option<[u8; 4]>,
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum Column {
    RowKey,
    Small,
    Int,
    Big,
    Numeric,
    Real,
    Double,
    Flag,
    Text,
    Varchar,
    Char,
    Bytes,
}

impl Column {
    fn sql(&self) -> &'static str {
        match self {
            Column::RowKey => "row_key",
            Column::Small => "small_value",
            Column::Int => "int_value",
            Column::Big => "big_value",
            Column::Numeric => "numeric_value",
            Column::Real => "real_value",
            Column::Double => "double_value",
            Column::Flag => "flag",
            Column::Text => "text_value",
            Column::Varchar => "varchar_value",
            Column::Char => "char_value",
            Column::Bytes => "bytes",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum ArithmeticExpression {
    Add(i32),
    Subtract(i32),
    Multiply(i32),
    Divide(NonZeroI32),
    Modulo(NonZeroI32),
}

impl ArithmeticExpression {
    fn sql(&self) -> String {
        match self {
            ArithmeticExpression::Add(value) => format!("int_value::BIGINT + {value}"),
            ArithmeticExpression::Subtract(value) => format!("int_value::BIGINT - {value}"),
            ArithmeticExpression::Multiply(value) => format!("int_value::BIGINT * {value}"),
            ArithmeticExpression::Divide(value) => format!("int_value::BIGINT / {value}"),
            ArithmeticExpression::Modulo(value) => format!("int_value::BIGINT % {value}"),
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum ComparisonOperator {
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterOrEqual,
    LessOrEqual,
}

impl ComparisonOperator {
    fn sql(&self) -> &'static str {
        match self {
            ComparisonOperator::Equal => "=",
            ComparisonOperator::NotEqual => "<>",
            ComparisonOperator::Greater => ">",
            ComparisonOperator::Less => "<",
            ComparisonOperator::GreaterOrEqual => ">=",
            ComparisonOperator::LessOrEqual => "<=",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum BooleanExpression {
    AndTrue,
    OrFalse,
    Not,
    IsTrue,
    IsFalse,
    IsUnknown,
}

impl BooleanExpression {
    fn sql(&self) -> &'static str {
        match self {
            BooleanExpression::AndTrue => "flag AND TRUE",
            BooleanExpression::OrFalse => "flag OR FALSE",
            BooleanExpression::Not => "NOT flag",
            BooleanExpression::IsTrue => "flag IS TRUE",
            BooleanExpression::IsFalse => "flag IS FALSE",
            BooleanExpression::IsUnknown => "flag IS UNKNOWN",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum NullExpression {
    IsNull,
    IsNotNull,
    IsDistinct,
    IsNotDistinct,
    AddNull,
    EqualNull,
}

impl NullExpression {
    fn sql(&self) -> &'static str {
        match self {
            NullExpression::IsNull => "text_value IS NULL",
            NullExpression::IsNotNull => "text_value IS NOT NULL",
            NullExpression::IsDistinct => "int_value IS DISTINCT FROM small_value",
            NullExpression::IsNotDistinct => "int_value IS NOT DISTINCT FROM small_value",
            NullExpression::AddNull => "int_value + NULL",
            NullExpression::EqualNull => "int_value = NULL",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum FunctionExpression {
    Coalesce,
    NullIf,
    Greatest,
    Least,
    Length,
    Lower,
    Upper,
    Abs,
}

impl FunctionExpression {
    fn sql(&self) -> &'static str {
        match self {
            FunctionExpression::Coalesce => "COALESCE(text_value, varchar_value, 'fallback')",
            FunctionExpression::NullIf => "NULLIF(int_value, small_value)",
            FunctionExpression::Greatest => "GREATEST(int_value, small_value, 0)",
            FunctionExpression::Least => "LEAST(big_value, int_value, 0)",
            FunctionExpression::Length => "length(text_value)",
            FunctionExpression::Lower => "lower(varchar_value)",
            FunctionExpression::Upper => "upper(text_value)",
            FunctionExpression::Abs => "abs(numeric_value)",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum CastExpression {
    SmallToBig,
    IntToText,
    BoolToInt,
    IntToBool,
    IntToByteaRoundtrip,
    TextToVarchar,
    NumericToInt,
    RealToDouble,
}

impl CastExpression {
    fn sql(&self) -> &'static str {
        match self {
            CastExpression::SmallToBig => "small_value::BIGINT",
            CastExpression::IntToText => "CAST(int_value AS TEXT)",
            CastExpression::BoolToInt => "CAST(flag AS INTEGER)",
            CastExpression::IntToBool => "CAST(int_value AS BOOLEAN)",
            CastExpression::IntToByteaRoundtrip => "int_value::BYTEA::INTEGER",
            CastExpression::TextToVarchar => "CAST(text_value AS VARCHAR(6))",
            CastExpression::NumericToInt => "CAST(numeric_value AS INTEGER)",
            CastExpression::RealToDouble => "CAST(real_value AS DOUBLE PRECISION)",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum SelectExpression {
    Wildcard,
    Column(Column),
    Arithmetic(ArithmeticExpression),
    Comparison {
        operator: ComparisonOperator,
        right: i32,
    },
    Boolean(BooleanExpression),
    Null(NullExpression),
    SimpleCase(i32),
    SearchedCase(i32),
    Function(FunctionExpression),
    Cast(CastExpression),
}

impl SelectExpression {
    fn sql(&self) -> String {
        match self {
            SelectExpression::Wildcard => "*".into(),
            SelectExpression::Column(column) => column.sql().into(),
            SelectExpression::Arithmetic(expression) => expression.sql(),
            SelectExpression::Comparison { operator, right } => {
                format!("numeric_value {} {right}", operator.sql())
            }
            SelectExpression::Boolean(expression) => expression.sql().into(),
            SelectExpression::Null(expression) => expression.sql().into(),
            SelectExpression::SimpleCase(value) => {
                format!("CASE int_value WHEN {value} THEN 'match' ELSE text_value END")
            }
            SelectExpression::SearchedCase(value) => format!(
                "CASE WHEN int_value > {value} THEN big_value WHEN flag IS TRUE THEN 0 ELSE small_value END"
            ),
            SelectExpression::Function(expression) => expression.sql().into(),
            SelectExpression::Cast(expression) => expression.sql().into(),
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum NullableColumn {
    Small,
    Text,
    Bytes,
}

impl NullableColumn {
    fn sql(&self) -> &'static str {
        match self {
            NullableColumn::Small => "small_value",
            NullableColumn::Text => "text_value",
            NullableColumn::Bytes => "bytes",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum Predicate {
    Comparison(i32),
    Flag,
    NotFlag,
    FlagIsTrue,
    FlagIsFalse,
    Null { column: NullableColumn, not: bool },
    Distinct { not: bool, value: i32 },
    Combined(i32),
}

impl Predicate {
    fn sql(&self) -> String {
        match self {
            Predicate::Comparison(value) => format!("int_value >= {value}"),
            Predicate::Flag => "flag".into(),
            Predicate::NotFlag => "NOT flag".into(),
            Predicate::FlagIsTrue => "flag IS TRUE".into(),
            Predicate::FlagIsFalse => "flag IS FALSE".into(),
            Predicate::Null { column, not } => {
                format!("{} IS {}NULL", column.sql(), if *not { "NOT " } else { "" })
            }
            Predicate::Distinct { not, value } => format!(
                "int_value IS {}DISTINCT FROM {value}",
                if *not { "NOT " } else { "" }
            ),
            Predicate::Combined(value) => {
                format!("(int_value < {value} OR flag IS TRUE) AND text_value IS NOT NULL")
            }
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum OrderKey {
    FirstProjection,
    NumericExpression,
    LowerText,
    FlagIsTrue,
}

impl OrderKey {
    fn sql(&self) -> &'static str {
        match self {
            OrderKey::FirstProjection => "1",
            OrderKey::NumericExpression => "numeric_value + int_value",
            OrderKey::LowerText => "lower(text_value)",
            OrderKey::FlagIsTrue => "flag IS TRUE",
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
struct OrderSpec {
    key: OrderKey,
    descending: bool,
    nulls_first: bool,
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum Assignment {
    Multiple,
    Small(i16),
    Int(i32),
    Big(i64),
    Numeric(i16),
    Real(i16),
    Double(i16),
    Flag,
    Text,
    Varchar,
    Char,
    Bytes,
}

impl Assignment {
    fn sql(&self) -> String {
        match self {
            Assignment::Multiple => "big_value = int_value, int_value = small_value".into(),
            Assignment::Small(value) => format!("small_value = {value}"),
            Assignment::Int(value) => format!("int_value = {value}"),
            Assignment::Big(value) => format!("big_value = {value}"),
            Assignment::Numeric(value) => {
                format!("numeric_value = {}", decimal_literal(i32::from(*value)))
            }
            Assignment::Real(value) => {
                format!("real_value = {}", decimal_literal(i32::from(*value)))
            }
            Assignment::Double(value) => {
                format!("double_value = {}", decimal_literal(i32::from(*value)))
            }
            Assignment::Flag => "flag = NOT flag".into(),
            Assignment::Text => "text_value = upper(COALESCE(text_value, 'fallback'))".into(),
            Assignment::Varchar => {
                "varchar_value = CAST(COALESCE(text_value, '') AS VARCHAR(12))".into()
            }
            Assignment::Char => "char_value = CAST(COALESCE(varchar_value, '') AS CHAR(8))".into(),
            Assignment::Bytes => "bytes = int_value::BYTEA".into(),
        }
    }
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum AutocommitAction {
    Insert,
    Select,
    Update,
    Delete,
    Begin,
}

#[derive(Debug, chaos_theory::Arbitrary)]
enum TransactionAction {
    Insert,
    Select,
    Update,
    Delete,
    Commit,
    Rollback,
}

fn postgres_server() -> PostgresServer {
    if let Ok(url) = env::var("PG_FAKE_TEST_DATABASE_URL") {
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
    let messages = client.simple_query(sql).unwrap_or_else(|error| {
        panic!(
            "generated SQL must be valid for PostgreSQL: {sql}\nSQLSTATE: {:?}\n{error}",
            error.code().map(|code| code.code())
        )
    });
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

fn fake_outcome(session: &mut Session, statement: &Statement, sql: &str) -> Outcome {
    match statement {
        Statement::Query(_) => {
            let result = session.query(sql, &[]).unwrap_or_else(|error| {
                panic!(
                    "generated SQL must be supported by pg_fake: {sql}\nSQLSTATE: {}\n{error}",
                    error.sqlstate.code()
                )
            });
            Outcome::Rows {
                values: result
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|value| match value {
                                Value::Null => None,
                                value => Some(value.to_text()),
                            })
                            .collect()
                    })
                    .collect(),
                type_oids: Some(
                    result
                        .columns
                        .iter()
                        .map(|column| column.type_oid)
                        .collect(),
                ),
            }
        }
        _ => Outcome::Affected(session.execute(sql).unwrap_or_else(|error| {
            panic!(
                "generated SQL must be supported by pg_fake: {sql}\nSQLSTATE: {}\n{error}",
                error.sqlstate.code()
            )
        })),
    }
}

fn assert_statement(postgres: &mut Client, fake: &mut Session, sql: &str, row_order: RowOrder) {
    let mut statements = parser::parse(sql)
        .unwrap_or_else(|error| panic!("generated SQL must parse: {sql}\n{error}"));
    assert_eq!(
        statements.len(),
        1,
        "generated operation must be one statement"
    );
    let statement = statements.pop().expect("statement count was checked");
    let expected = postgres_outcome(postgres, &statement, sql);
    let actual = fake_outcome(fake, &statement, sql);
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

fn decimal_literal(value: i32) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let absolute = value.abs();
    format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
}

fn nullable<T>(value: Option<T>, render: impl FnOnce(T) -> String) -> String {
    value.map(render).unwrap_or_else(|| "NULL".into())
}

fn row(row: GeneratedRow, row_key: i64) -> String {
    let small_value = nullable(row.small_value, |value| value.to_string());
    let int_value = nullable(row.int_value, |value| value.to_string());
    let big_value = nullable(row.big_value, |value| value.to_string());
    let numeric_value = nullable(row.numeric_hundredths, |value| {
        decimal_literal(i32::from(value))
    });
    let real_value = nullable(row.real_hundredths, |value| {
        decimal_literal(i32::from(value))
    });
    let double_value = nullable(row.double_hundredths, |value| {
        decimal_literal(i32::from(value))
    });
    let flag = nullable(row.flag, |value| {
        if value { "TRUE" } else { "FALSE" }.into()
    });
    let text_value = nullable(row.text_value, TextValue::sql);
    let varchar_value = nullable(row.varchar_value, TextValue::sql);
    let char_value = nullable(row.char_value, |value| value.sql().into());
    let bytes = nullable(row.bytes, |bytes| {
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
    let mut rows = Vec::new();
    src.repeat_n("rows", 1..=4, |src| {
        rows.push(row(src.any("row"), *next_row_key));
        *next_row_key += 1;
        Effect::Success
    });
    format!("INSERT INTO {table} VALUES {}", rows.join(", "))
}

fn where_clause(src: &mut Source) -> String {
    let predicate: Option<Predicate> = src.any("where");
    predicate
        .map(|predicate| format!(" WHERE {}", predicate.sql()))
        .unwrap_or_default()
}

fn select_expression(src: &mut Source) -> String {
    src.any::<SelectExpression>("expression").sql()
}

fn select_sql(src: &mut Source, table: &str) -> (String, RowOrder) {
    let mut projections = Vec::new();
    src.repeat_n("projections", 1..=4, |src| {
        projections.push(select_expression(src));
        Effect::Success
    });
    let mut sql = format!(
        "SELECT {} FROM {table}{}",
        projections.join(", "),
        where_clause(src)
    );
    let ordered: Option<OrderSpec> = src.any("order");
    if let Some(order) = ordered {
        let direction = if order.descending { "DESC" } else { "ASC" };
        let nulls = if order.nulls_first {
            "NULLS FIRST"
        } else {
            "NULLS LAST"
        };
        sql.push_str(&format!(
            " ORDER BY {} {direction} {nulls}, row_key + 0",
            order.key.sql()
        ));
        (sql, RowOrder::Ordered)
    } else {
        (sql, RowOrder::Unordered)
    }
}

fn update_sql(src: &mut Source, table: &str) -> String {
    let assignment: Assignment = src.any("assignment");
    format!(
        "UPDATE {table} SET {}{}",
        assignment.sql(),
        where_clause(src)
    )
}

fn create_table_sql(table: &str) -> String {
    format!(
        "CREATE TABLE {table} (\
             row_key BIGINT PRIMARY KEY, \
             small_value SMALLINT CHECK (small_value IS NULL OR small_value >= -32768), \
             int_value INTEGER DEFAULT 0, \
             big_value BIGINT, \
             numeric_value NUMERIC(8, 2), \
             real_value REAL, \
             double_value DOUBLE PRECISION, \
             flag BOOLEAN, \
             text_value TEXT, \
             varchar_value VARCHAR(12), \
             char_value CHAR(8), \
             bytes BYTEA\
         )"
    )
}

#[test]
fn generated_sql_matches_postgres() {
    let _test_lock = TEST_LOCK.lock().expect("test mutex must not be poisoned");
    let server = postgres_server();
    let postgres = RefCell::new(
        Client::connect(&server.url, NoTls).expect("must connect to PostgreSQL 18 once"),
    );
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
        let db = Db::new();
        let mut fake = db.session();
        let mut next_row_key = 1;
        let mut in_transaction = false;

        let create = create_table_sql(&table);
        assert_statement(postgres.client(), &mut fake, &create, RowOrder::Unordered);
        let insert = insert_sql(src, &table, &mut next_row_key);
        assert_statement(postgres.client(), &mut fake, &insert, RowOrder::Unordered);

        src.repeat_n("statements", 3..=14, |src| {
            let (sql, order) = if in_transaction {
                match src.any::<TransactionAction>("action") {
                    TransactionAction::Insert => (
                        insert_sql(src, &table, &mut next_row_key),
                        RowOrder::Unordered,
                    ),
                    TransactionAction::Select => select_sql(src, &table),
                    TransactionAction::Update => (update_sql(src, &table), RowOrder::Unordered),
                    TransactionAction::Delete => (
                        format!("DELETE FROM {table}{}", where_clause(src)),
                        RowOrder::Unordered,
                    ),
                    TransactionAction::Commit => {
                        in_transaction = false;
                        ("COMMIT".into(), RowOrder::Unordered)
                    }
                    TransactionAction::Rollback => {
                        in_transaction = false;
                        ("ROLLBACK".into(), RowOrder::Unordered)
                    }
                }
            } else {
                match src.any::<AutocommitAction>("action") {
                    AutocommitAction::Insert => (
                        insert_sql(src, &table, &mut next_row_key),
                        RowOrder::Unordered,
                    ),
                    AutocommitAction::Select => select_sql(src, &table),
                    AutocommitAction::Update => (update_sql(src, &table), RowOrder::Unordered),
                    AutocommitAction::Delete => (
                        format!("DELETE FROM {table}{}", where_clause(src)),
                        RowOrder::Unordered,
                    ),
                    AutocommitAction::Begin => {
                        in_transaction = true;
                        ("BEGIN".into(), RowOrder::Unordered)
                    }
                }
            };
            src.log_value("sql", &sql);
            assert_statement(postgres.client(), &mut fake, &sql, order);
            Effect::Success
        });

        if in_transaction {
            assert_statement(
                postgres.client(),
                &mut fake,
                "ROLLBACK",
                RowOrder::Unordered,
            );
        }
    });
}
