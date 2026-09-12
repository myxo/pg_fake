
use pg_fake::{
    api::{ColumnMeta, Db, QueryResult},
    value::{BaseType, Value},
};

use super::{format_value, has_complete_sql_statement, run_sql, write_query_result};

#[test]
fn formats_query_result_as_table() {
    let result = QueryResult {
        columns: vec![
            ColumnMeta {
                name: "id".into(),
                type_oid: BaseType::Int4.map_to_oid(),
                typmod: -1,
            },
            ColumnMeta {
                name: "name".into(),
                type_oid: BaseType::Text.map_to_oid(),
                typmod: -1,
            },
        ],
        rows: vec![
            vec![Value::Int4(1), Value::Text("Ada".into())],
            vec![Value::Int4(20), Value::Null],
        ],
    };
    let mut output = Vec::new();

    write_query_result(&mut output, &result).unwrap();

    assert_eq!(
        String::from_utf8(output).unwrap(),
        "id | name\n---+-----\n1  | Ada \n20 | NULL\n(2 rows)\n"
    );
}

#[test]
fn detects_only_top_level_statement_terminators() {
    assert!(!has_complete_sql_statement("SELECT ';'"));
    assert!(!has_complete_sql_statement("SELECT 1 -- ;"));
    assert!(!has_complete_sql_statement("/* ; */ SELECT 1"));
    assert!(has_complete_sql_statement("/* ; */ SELECT 1;"));
    assert!(has_complete_sql_statement("/* outer /* ; */ */ SELECT 1;"));
}

#[test]
fn formats_nulls_and_control_characters() {
    assert_eq!(format_value(&Value::Null), "NULL");
    assert_eq!(format_value(&Value::Text("one\ntwo".into())), "one\\ntwo");
}

#[test]
fn runs_sql_batches_against_one_session() {
    let mut session = Db::create().create_session();
    let mut output = Vec::new();

    run_sql(
        &mut output,
        &mut session,
        "CREATE TABLE items (id INTEGER, name TEXT); \
             INSERT INTO items VALUES (1, 'Ada'), (2, NULL); \
             SELECT * FROM items ORDER BY id",
    )
    .unwrap();

    assert_eq!(
        String::from_utf8(output).unwrap(),
        "0 rows affected\n2 rows affected\nid | name\n---+-----\n1  | Ada \n2  | NULL\n(2 rows)\n"
    );
}
