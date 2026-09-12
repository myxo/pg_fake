use crate::value::{Oid, Value};

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

#[derive(Debug, Clone, PartialEq)]
pub enum StatementResult {
    Affected(u64),
    Query(QueryResult),
}
