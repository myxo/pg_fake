use std::sync::{Arc, Mutex};

use crate::{
    error::{PgError, Result, SqlState},
    executor::{self, DatabaseState, ExecutionResult},
    parser,
    txn::Snapshot,
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
}

impl Db {
    pub fn new() -> Self {
        Db {
            state: Arc::new(Mutex::new(DatabaseState::new())),
        }
    }
    pub fn session(&self) -> Session {
        Session { db: self.clone() }
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
    fn run(&mut self, sql: &str) -> Result<ExecutionResult> {
        let mut statements = parser::parse(sql)?;
        if statements.len() != 1 {
            return Err(PgError::new(
                SqlState::SyntaxError,
                "exactly one statement is required",
            ));
        }
        let statement = statements.pop().expect("statement count was checked");
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
                visible_version(chain, &snapshot, reader)
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
        assert_eq!(error.sqlstate, SqlState::DatatypeMismatch);
    }
}
