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
        }
    }
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        if !params.is_empty() {
            return Err(PgError::new(
                SqlState::FeatureNotSupported,
                "parameters are not implemented",
            ));
        }
        let _ = self.run(sql)?;
        Err(PgError::new(
            SqlState::FeatureNotSupported,
            "queries are not implemented",
        ))
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
