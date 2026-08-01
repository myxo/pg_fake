use std::sync::{Arc, Mutex};

use crate::{
    error::{PgError, Result, SqlState},
    executor::{self, DatabaseState, ExecutionResult},
    parser,
    txn::{Snapshot, Xid},
    value::{Oid, Value},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_oid: Oid,
    pub typmod: i32,
}
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Value>>,
}
#[derive(Clone)]
pub struct Db {
    state: Arc<Mutex<DatabaseState>>,
}
pub struct Session {
    db: Db,
    transaction: Option<SessionTransaction>,
}
#[derive(Clone, Copy)]
enum SessionTransaction {
    Active(Xid),
    Aborted(Xid),
}
pub struct Transaction<'session> {
    session: &'session mut Session,
    finished: bool,
}

impl Db {
    pub fn new() -> Self {
        Db {
            state: Arc::new(Mutex::new(DatabaseState::new())),
        }
    }
    pub fn session(&self) -> Session {
        Session {
            db: self.clone(),
            transaction: None,
        }
    }
}
impl Default for Db {
    fn default() -> Self {
        Self::new()
    }
}
impl Session {
    pub fn execute(&mut self, sql: &str) -> Result<u64> {
        match self.run(sql)? {
            ExecutionResult::Affected(rows) => Ok(rows),
            ExecutionResult::Query(_) => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "use query for SELECT statements",
            )),
        }
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        if !params.is_empty() {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "parameters are not implemented",
            ));
        }
        match self.run(sql)? {
            ExecutionResult::Query(result) => Ok(result),
            ExecutionResult::Affected(_) => Err(PgError::new(
                SqlState::FeatureNotSupported,
                "query requires a SELECT statement",
            )),
        }
    }
    pub fn begin(&mut self) -> Result<Transaction<'_>> {
        if self.transaction.is_some() {
            return Err(PgError::new(
                SqlState::ActiveSqlTransaction,
                "transaction already in progress",
            ));
        }
        self.start_transaction();
        Ok(Transaction {
            session: self,
            finished: false,
        })
    }
    fn start_transaction(&mut self) {
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        self.transaction = Some(SessionTransaction::Active(state.transactions.begin()));
    }
    fn finish_transaction(&mut self, commit: bool) {
        let Some(transaction) = self.transaction.take() else {
            return;
        };
        let xid = match transaction {
            SessionTransaction::Active(xid) if commit => {
                let mut state = self.db.state.lock().expect("database mutex is poisoned");
                state.transactions.commit(xid);
                return;
            }
            SessionTransaction::Active(xid) | SessionTransaction::Aborted(xid) => xid,
        };
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        state.transactions.abort(xid);
    }
    fn abort_transaction(&mut self) {
        if let Some(SessionTransaction::Active(xid)) = self.transaction {
            self.transaction = Some(SessionTransaction::Aborted(xid));
        }
    }
    fn failed<T>(&mut self, error: PgError) -> Result<T> {
        self.abort_transaction();
        Err(error)
    }
    fn run(&mut self, sql: &str) -> Result<ExecutionResult> {
        let mut statements = match parser::parse(sql) {
            Ok(statements) => statements,
            Err(error) => return self.failed(error),
        };
        if statements.len() != 1 {
            return self.failed(PgError::new(
                SqlState::SyntaxError,
                "exactly one statement is required",
            ));
        }
        let statement = statements.pop().expect("statement count was checked");
        match &statement {
            parser::Statement::StartTransaction { modes, .. } => {
                if !modes.is_empty() {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "transaction modes are not implemented",
                    ));
                }
                return match self.transaction {
                    None => {
                        self.start_transaction();
                        Ok(ExecutionResult::Affected(0))
                    }
                    Some(SessionTransaction::Active(_)) => Ok(ExecutionResult::Affected(0)),
                    Some(SessionTransaction::Aborted(_)) => Err(PgError::new(
                        SqlState::InFailedSqlTransaction,
                        "current transaction is aborted",
                    )),
                };
            }
            parser::Statement::Commit { chain } => {
                if *chain {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "COMMIT AND CHAIN is not implemented",
                    ));
                }
                self.finish_transaction(true);
                return Ok(ExecutionResult::Affected(0));
            }
            parser::Statement::Rollback { chain, savepoint } => {
                if *chain || savepoint.is_some() {
                    return self.failed(PgError::new(
                        SqlState::FeatureNotSupported,
                        "ROLLBACK variant is not implemented",
                    ));
                }
                self.finish_transaction(false);
                return Ok(ExecutionResult::Affected(0));
            }
            _ => {}
        }
        if matches!(self.transaction, Some(SessionTransaction::Aborted(_))) {
            return Err(PgError::new(
                SqlState::InFailedSqlTransaction,
                "current transaction is aborted",
            ));
        }
        if self.transaction.is_some()
            && matches!(parser::classify(&statement), parser::StatementKind::Ddl)
        {
            return self.failed(PgError::new(
                SqlState::FeatureNotSupported,
                "DDL in an explicit transaction is not implemented",
            ));
        }
        if let Some(SessionTransaction::Active(xid)) = self.transaction {
            let mut state = self.db.state.lock().expect("database mutex is poisoned");
            let snapshot = Snapshot::new(&state.transactions);
            return match executor::dispatch(&mut state, &statement, xid, &snapshot) {
                Ok(result) => Ok(result),
                Err(error) => {
                    drop(state);
                    self.failed(error)
                }
            };
        }
        let mut state = self.db.state.lock().expect("database mutex is poisoned");
        let xid = state.transactions.begin();
        let snapshot = Snapshot::new(&state.transactions);
        match executor::dispatch(&mut state, &statement, xid, &snapshot) {
            Ok(result) => {
                state.transactions.commit(xid);
                Ok(result)
            }
            Err(error) => {
                state.transactions.abort(xid);
                Err(error)
            }
        }
    }
}

impl Transaction<'_> {
    pub fn execute(&mut self, sql: &str) -> Result<u64> {
        self.session.execute(sql)
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.session.query(sql, params)
    }
    pub fn commit(mut self) -> Result<()> {
        self.session.finish_transaction(true);
        self.finished = true;
        Ok(())
    }
    pub fn rollback(mut self) -> Result<()> {
        self.session.finish_transaction(false);
        self.finished = true;
        Ok(())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.session.finish_transaction(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        txn::{Snapshot, visible_version},
        value::BaseType,
    };

    use super::*;

    #[test]
    fn autocommit_creates_and_drops_tables() {
        let db = Db::new();
        let mut session = db.session();
        assert_eq!(session.execute("CREATE TABLE items (id INTEGER NOT NULL, name VARCHAR(12), amount NUMERIC(8, 2))").unwrap(), 0);
        let state = db.state.lock().unwrap();
        let table = state.catalog.table("items").unwrap();
        assert_eq!(table.columns[0].data_type.base, BaseType::Int4);
        assert_eq!(table.columns[1].data_type.typmod, 16);
        assert_eq!(table.columns[2].data_type.typmod, (8 << 16) + 2 + 4);
        drop(state);
        assert_eq!(session.execute("DROP TABLE items").unwrap(), 1);
    }

    #[test]
    fn selects_projections_with_metadata_in_row_id_order() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, name TEXT)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (2, 'second'), (1, 'first')")
            .unwrap();

        let result = session.query("SELECT name, id FROM items", &[]).unwrap();

        assert_eq!(
            result.columns,
            vec![
                ColumnMeta {
                    name: "name".into(),
                    type_oid: BaseType::Text.oid(),
                    typmod: -1,
                },
                ColumnMeta {
                    name: "id".into(),
                    type_oid: BaseType::Int4.oid(),
                    typmod: -1,
                },
            ]
        );
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Text("second".into()), Value::Int4(2)],
                vec![Value::Text("first".into()), Value::Int4(1)],
            ]
        );
        let all_columns = session.query("SELECT * FROM items", &[]).unwrap();
        assert_eq!(
            all_columns.rows,
            vec![
                vec![Value::Int4(2), Value::Text("second".into())],
                vec![Value::Int4(1), Value::Text("first".into())],
            ]
        );
    }

    #[test]
    fn select_excludes_uncommitted_rows_from_another_transaction() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        let mut state = db.state.lock().unwrap();
        let writer = state.transactions.begin();
        let table_id = state.catalog.table("items").unwrap().id;
        state
            .tables
            .get_mut(&table_id)
            .unwrap()
            .insert(writer, vec![Value::Int4(1)]);
        drop(state);

        assert!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap()
                .rows
                .is_empty()
        );

        let mut state = db.state.lock().unwrap();
        state.transactions.abort(writer);
    }

    #[test]
    fn select_reports_unknown_tables_and_columns() {
        let db = Db::new();
        let mut session = db.session();

        assert_eq!(
            session
                .query("SELECT * FROM missing", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        assert_eq!(
            session
                .query("SELECT missing FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedColumn
        );
    }

    #[test]
    fn evaluates_arithmetic_and_comparison_projections() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER, name TEXT, price NUMERIC)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (7, 3, 'seven', 2.5)")
            .unwrap();

        let result = session
            .query(
                "SELECT id + amount, id - amount, id * amount, id / amount, id % amount, id > amount, name = 'seven', price * 2.0 FROM items",
                &[],
            )
            .unwrap();

        assert_eq!(
            result.rows,
            vec![vec![
                Value::Int4(10),
                Value::Int4(4),
                Value::Int4(21),
                Value::Int4(2),
                Value::Int4(1),
                Value::Bool(true),
                Value::Bool(true),
                Value::Numeric("5.00".parse().unwrap()),
            ]]
        );
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| column.name.as_str())
                .collect::<Vec<_>>(),
            vec!["?column?"; 8]
        );
        assert_eq!(
            result
                .columns
                .iter()
                .map(|column| (column.type_oid, column.typmod))
                .collect::<Vec<_>>(),
            vec![
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Int4.oid(), -1),
                (BaseType::Bool.oid(), -1),
                (BaseType::Bool.oid(), -1),
                (BaseType::Numeric.oid(), -1),
            ]
        );
        assert_eq!(
            session
                .query("SELECT id / 0 FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::DivisionByZero
        );
        session
            .execute("INSERT INTO items VALUES (2147483647, 1, 'max', 1.0)")
            .unwrap();
        assert_eq!(
            session
                .query("SELECT id + 1 FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::NumericValueOutOfRange
        );
    }

    #[test]
    fn evaluates_case_and_common_scalar_functions() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE items (
                    id INTEGER,
                    score INTEGER,
                    label TEXT,
                    delta INTEGER
                )",
            )
            .unwrap();
        session
            .execute(
                "INSERT INTO items VALUES
                    (1, 7, 'MiXeD', 3),
                    (2, 0, NULL, NULL),
                    (3, NULL, 'third', 4)",
            )
            .unwrap();

        let result = session
            .query(
                "SELECT
                    CASE
                        WHEN score > 5 THEN 'high'
                        WHEN score IS NULL THEN 'missing'
                        ELSE 'low'
                    END,
                    CASE id
                        WHEN 1 THEN 'one'
                        WHEN 2 THEN NULL
                        ELSE 'other'
                    END,
                    CASE WHEN score > 100 THEN score END,
                    COALESCE(label, 'fallback'),
                    NULLIF(score, 0),
                    GREATEST(score, 5),
                    LEAST(score, 5),
                    length(label),
                    lower(label),
                    upper(label),
                    abs(-delta)
                 FROM items",
                &[],
            )
            .unwrap();

        assert_eq!(
            result.rows,
            vec![
                vec![
                    Value::Text("high".into()),
                    Value::Text("one".into()),
                    Value::Null,
                    Value::Text("MiXeD".into()),
                    Value::Int4(7),
                    Value::Int4(7),
                    Value::Int4(5),
                    Value::Int4(5),
                    Value::Text("mixed".into()),
                    Value::Text("MIXED".into()),
                    Value::Int4(3),
                ],
                vec![
                    Value::Text("low".into()),
                    Value::Null,
                    Value::Null,
                    Value::Text("fallback".into()),
                    Value::Null,
                    Value::Int4(5),
                    Value::Int4(0),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                vec![
                    Value::Text("missing".into()),
                    Value::Text("other".into()),
                    Value::Null,
                    Value::Text("third".into()),
                    Value::Null,
                    Value::Int4(5),
                    Value::Int4(5),
                    Value::Int4(5),
                    Value::Text("third".into()),
                    Value::Text("THIRD".into()),
                    Value::Int4(4),
                ],
            ]
        );
        assert_eq!(
            session
                .query(
                    "SELECT
                        CASE WHEN id = 1 THEN 10 ELSE 1 / (id - 1) END,
                        COALESCE(score, 1 / (score - 7))
                     FROM items
                     WHERE id = 1",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![Value::Int4(10), Value::Int4(7)]]
        );
    }

    #[test]
    fn abs_supports_all_phase_one_numeric_types() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE numbers (
                    int2_value SMALLINT,
                    int4_value INTEGER,
                    int8_value BIGINT,
                    float4_value REAL,
                    float8_value DOUBLE PRECISION,
                    numeric_value NUMERIC
                )",
            )
            .unwrap();
        let mut state = db.state.lock().unwrap();
        let xid = state.transactions.begin();
        let table_id = state.catalog.table("numbers").unwrap().id;
        state.tables.get_mut(&table_id).unwrap().insert(
            xid,
            vec![
                Value::Int2(-2),
                Value::Int4(-4),
                Value::Int8(-8),
                Value::Float4(-4.5),
                Value::Float8(-8.5),
                Value::Numeric("-12.25".parse().unwrap()),
            ],
        );
        state.transactions.commit(xid);
        drop(state);

        assert_eq!(
            session
                .query(
                    "SELECT
                        abs(int2_value),
                        abs(int4_value),
                        abs(int8_value),
                        abs(float4_value),
                        abs(float8_value),
                        abs(numeric_value)
                     FROM numbers",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Int2(2),
                Value::Int4(4),
                Value::Int8(8),
                Value::Float4(4.5),
                Value::Float8(8.5),
                Value::Numeric("12.25".parse().unwrap()),
            ]]
        );
    }

    #[test]
    fn case_and_functions_report_type_and_name_errors() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT CASE WHEN id = 1 THEN id ELSE TRUE END FROM items",
                    &[]
                )
                .unwrap_err()
                .sqlstate,
            SqlState::DatatypeMismatch
        );
        assert_eq!(
            session
                .query("SELECT unknown_function(id) FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedFunction
        );
    }

    #[test]
    fn coerces_phase_one_types_in_all_cast_contexts() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE types (
                    small_value SMALLINT,
                    int_value INTEGER,
                    big_value BIGINT,
                    numeric_value NUMERIC,
                    real_value REAL,
                    double_value DOUBLE PRECISION,
                    short_label VARCHAR(4)
                )",
            )
            .unwrap();
        session
            .execute("INSERT INTO types VALUES (1, 2, 3, 4, 5, 6, 'abcd')")
            .unwrap();

        assert_eq!(
            session
                .query(
                    "SELECT
                        small_value + int_value,
                        int_value + big_value,
                        big_value + numeric_value,
                        numeric_value + real_value,
                        real_value + double_value,
                        int_value = '2',
                        CASE WHEN TRUE THEN int_value ELSE numeric_value END,
                        COALESCE(NULL, int_value, numeric_value)
                     FROM types",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Int4(3),
                Value::Int8(5),
                Value::Numeric("7".parse().unwrap()),
                Value::Float4(9.0),
                Value::Float8(11.0),
                Value::Bool(true),
                Value::Numeric("2".parse().unwrap()),
                Value::Numeric("2".parse().unwrap()),
            ]]
        );
        assert_eq!(
            session
                .query(
                    "SELECT
                        CAST('42' AS INTEGER),
                        '3.5'::NUMERIC,
                        CAST(2.6 AS INTEGER),
                        CAST(1 AS TEXT),
                        CAST(TRUE AS TEXT),
                        1::BOOLEAN,
                        TRUE::INTEGER,
                        258::BYTEA,
                        '\\x00000102'::BYTEA::INTEGER,
                        CAST('abcdef' AS VARCHAR(3)),
                        CAST(12.36 AS NUMERIC(4, 1))
                     FROM types",
                    &[],
                )
                .unwrap()
                .rows,
            vec![vec![
                Value::Int4(42),
                Value::Numeric("3.5".parse().unwrap()),
                Value::Int4(3),
                Value::Text("1".into()),
                Value::Text("true".into()),
                Value::Bool(true),
                Value::Int4(1),
                Value::Bytea(vec![0, 0, 1, 2]),
                Value::Int4(258),
                Value::Text("abc".into()),
                Value::Numeric("12.4".parse().unwrap()),
            ]]
        );

        session
            .execute("UPDATE types SET small_value = int_value, int_value = 2.6")
            .unwrap();
        session.execute("UPDATE types SET int_value = '7'").unwrap();
        assert_eq!(
            session
                .query("SELECT small_value, int_value FROM types", &[])
                .unwrap()
                .rows,
            vec![vec![Value::Int2(2), Value::Int4(7)]]
        );
    }

    #[test]
    fn coercion_reports_postgres_error_categories() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute(
                "CREATE TABLE assignments (
                    small_value SMALLINT,
                    short_label VARCHAR(3),
                    fixed_numeric NUMERIC(4, 1)
                )",
            )
            .unwrap();

        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES ('bad', 'abc', 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::InvalidTextRepresentation
        );
        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES (40000, 'abc', 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::NumericValueOutOfRange
        );
        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES (1, 'toolong', 1)")
                .unwrap_err()
                .sqlstate,
            SqlState::StringDataRightTruncation
        );
        assert_eq!(
            session
                .execute("INSERT INTO assignments VALUES (1, 'abc', 1234.5)")
                .unwrap_err()
                .sqlstate,
            SqlState::NumericValueOutOfRange
        );
        assert_eq!(
            session
                .query("SELECT TRUE::BYTEA FROM assignments", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::CannotCoerce
        );
    }

    #[test]
    fn explicit_transactions_control_insert_and_update_visibility() {
        let db = Db::new();
        let mut first = db.session();
        let mut second = db.session();
        first
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        first.execute("INSERT INTO items VALUES (1, 1)").unwrap();

        first.execute("BEGIN").unwrap();
        first
            .execute("UPDATE items SET amount = amount + 1 WHERE id = 1")
            .unwrap();
        first.execute("INSERT INTO items VALUES (2, 2)").unwrap();
        assert_eq!(
            first.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)]
            ]
        );
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1), Value::Int4(1)]]
        );
        first.execute("COMMIT").unwrap();
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)]
            ]
        );

        first.execute("BEGIN").unwrap();
        first.execute("INSERT INTO items VALUES (3, 3)").unwrap();
        first.execute("ROLLBACK").unwrap();
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)]
            ]
        );
        first.execute("INSERT INTO items VALUES (4, 4)").unwrap();
        assert_eq!(
            second.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Int4(2)],
                vec![Value::Int4(4), Value::Int4(4)],
            ]
        );
    }

    #[test]
    fn deletes_matching_rows_and_all_rows() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, amount INTEGER)")
            .unwrap();
        session
            .execute("INSERT INTO items VALUES (1, 2), (2, NULL), (3, 4)")
            .unwrap();

        assert_eq!(
            session
                .execute("DELETE FROM items WHERE amount > 2")
                .unwrap(),
            1
        );
        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1), Value::Int4(2)],
                vec![Value::Int4(2), Value::Null],
            ]
        );
        assert_eq!(session.execute("DELETE FROM items").unwrap(), 2);
        assert!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }

    #[test]
    fn explicit_delete_visibility_matches_transaction_outcome() {
        let db = Db::new();
        let mut writer = db.session();
        let mut reader = db.session();
        writer.execute("CREATE TABLE items (id INTEGER)").unwrap();
        writer
            .execute("INSERT INTO items VALUES (1), (2), (3)")
            .unwrap();

        writer.execute("BEGIN").unwrap();
        assert_eq!(writer.execute("DELETE FROM items WHERE id = 1").unwrap(), 1);
        assert_eq!(
            writer.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(2)], vec![Value::Int4(3)]]
        );
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)]
            ]
        );
        writer.execute("ROLLBACK").unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![
                vec![Value::Int4(1)],
                vec![Value::Int4(2)],
                vec![Value::Int4(3)]
            ]
        );

        writer.execute("BEGIN").unwrap();
        writer.execute("DELETE FROM items WHERE id = 2").unwrap();
        writer.execute("COMMIT").unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(3)]]
        );
    }

    #[test]
    fn delete_requires_a_boolean_where_expression() {
        let db = Db::new();
        let mut session = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();

        assert_eq!(
            session
                .execute("DELETE FROM items WHERE id")
                .unwrap_err()
                .sqlstate,
            SqlState::DatatypeMismatch
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
    }

    #[test]
    fn explicit_transactions_abort_after_errors_and_raii_rolls_back() {
        let db = Db::new();
        let mut session = db.session();
        let mut reader = db.session();
        session.execute("CREATE TABLE items (id INTEGER)").unwrap();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO missing VALUES (1)")
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::InFailedSqlTransaction
        );
        session.execute("ROLLBACK").unwrap();
        session.execute("INSERT INTO items VALUES (1)").unwrap();

        {
            let mut transaction = session.begin().unwrap();
            transaction.execute("INSERT INTO items VALUES (2)").unwrap();
        }
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)]]
        );
        let mut transaction = session.begin().unwrap();
        transaction.execute("INSERT INTO items VALUES (3)").unwrap();
        transaction.commit().unwrap();
        assert_eq!(
            reader.query("SELECT * FROM items", &[]).unwrap().rows,
            vec![vec![Value::Int4(1)], vec![Value::Int4(3)]]
        );
    }

    #[test]
    fn rejects_ddl_inside_explicit_transactions() {
        let db = Db::new();
        let mut session = db.session();

        session.execute("BEGIN").unwrap();
        assert_eq!(
            session
                .execute("CREATE TABLE items (id INTEGER)")
                .unwrap_err()
                .sqlstate,
            SqlState::FeatureNotSupported
        );
        session.execute("ROLLBACK").unwrap();
        assert_eq!(
            session
                .query("SELECT * FROM items", &[])
                .unwrap_err()
                .sqlstate,
            SqlState::UndefinedTable
        );
    }

    #[test]
    fn insert_uses_exact_literal_types_and_commits() {
        let db = Db::new();
        let mut session = db.session();
        session
            .execute("CREATE TABLE items (id INTEGER, name TEXT)")
            .unwrap();
        assert_eq!(
            session
                .execute("INSERT INTO items (name, id) VALUES ('one', 1), ('two', 2)")
                .unwrap(),
            2
        );
        let mut state = db.state.lock().unwrap();
        let reader = state.transactions.begin();
        let snapshot = Snapshot::new(&state.transactions);
        let schema = state.catalog.table("items").unwrap();
        let table = state.tables.get(&schema.id).unwrap();
        let rows = table
            .rows()
            .map(|(_, chain)| {
                visible_version(chain, &snapshot, reader, &state.transactions)
                    .unwrap()
                    .row
                    .clone()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![
                vec![Value::Int4(1), Value::Text("one".into())],
                vec![Value::Int4(2), Value::Text("two".into())]
            ]
        );
        let _ = table;
        drop(state);
        let error = session
            .execute("INSERT INTO items VALUES ('wrong', 'type')")
            .unwrap_err();
        assert_eq!(error.sqlstate, SqlState::InvalidTextRepresentation);
    }
}
