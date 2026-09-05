use pg_fake_sqlx::{Db, PgFakeConnection};
use sqlx::{AssertSqlSafe, Column, Connection, Row, TypeInfo};
use sqlx_postgres::PgConnection;

mod common;
#[path = "common/differential.rs"]
mod differential;

use differential::{
    RowOrder, assert_statement, assert_statement_allow_error, start_isolated_postgres_server,
};

#[test]
fn matches_json_storage_parameters_metadata_and_unsupported_operations() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("must create tokio runtime");
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .expect("must connect SQLx to PostgreSQL 18");
    let mut fake = PgFakeConnection::new(Db::create());
    let table = "pg_fake_json_differential";
    let cleanup = format!("DROP TABLE IF EXISTS {table}");
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        &cleanup,
        RowOrder::Unordered,
    );
    let create = format!(
        "CREATE TABLE {table} (id INTEGER, payload JSON NOT NULL CHECK (payload IS NOT NULL), fallback JSON DEFAULT '{{ \"default\" : [1, 2] }}')"
    );
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        &create,
        RowOrder::Unordered,
    );

    let document = r#"{ "z" : 1e+02, "z" : -0.00, "unicode" : "Привет 🌍" }"#;
    let insert = format!(
        "INSERT INTO {table} (id, payload) VALUES (1, $1::json) RETURNING payload, fallback"
    );
    let postgres_row = runtime
        .block_on(
            sqlx::query(AssertSqlSafe(insert.as_str()))
                .bind(document)
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let fake_row = runtime
        .block_on(
            sqlx::query(AssertSqlSafe(insert.as_str()))
                .bind(document)
                .fetch_one(&mut fake),
        )
        .unwrap();
    let postgres_payload = postgres_row.get_unchecked::<String, _>(0);
    let fake_payload = fake_row.get_unchecked::<String, _>(0);
    assert_eq!(fake_payload, postgres_payload);
    assert_eq!(fake_payload, document);
    assert_eq!(
        fake_row.try_get_unchecked::<&str, _>(0).unwrap(),
        postgres_row.try_get_unchecked::<&str, _>(0).unwrap(),
    );
    assert_eq!(
        fake_row.try_get_unchecked::<Option<&str>, _>(0).unwrap(),
        postgres_row
            .try_get_unchecked::<Option<&str>, _>(0)
            .unwrap(),
    );
    assert_eq!(
        fake_row.columns()[0].type_info().name(),
        postgres_row.columns()[0].type_info().name(),
    );
    assert_eq!(fake_row.columns()[0].type_info().name(), "JSON");

    let nested = format!("{}0{}", "[".repeat(512), "]".repeat(512));
    let nested_sql = format!("SELECT '{nested}'::json");
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        &nested_sql,
        RowOrder::Ordered,
    );

    for sql in [
        "SELECT JSON '{\"typed\": true}'",
        "SELECT '{}'::pg_catalog.json",
        "SELECT '{\"$serde_json::private::Number\":null}'::json",
        "SELECT ' {\"a\":1} '::json UNION ALL SELECT '[2]'",
        "SELECT NULL UNION ALL SELECT '{}'::json",
        "(SELECT '{}' AS payload, 1 AS position ORDER BY position) UNION ALL SELECT '{}'::json, 1",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }

    for sql in [
        format!("INSERT INTO {table} (id, payload) VALUES (2, '[1,]')"),
        format!("INSERT INTO {table} (id, payload) VALUES (2, '{{}}'::text)"),
        format!("SELECT payload = payload FROM {table}"),
        format!("SELECT payload FROM {table} ORDER BY payload"),
        format!("SELECT payload FROM {table} GROUP BY payload"),
        format!("SELECT DISTINCT payload FROM {table}"),
        format!("SELECT count(DISTINCT payload) FROM {table}"),
        "SELECT nullif('{}'::json, NULL::json)".into(),
        "SELECT greatest('{}'::json, '{}'::json)".into(),
        "SELECT CASE NULL::json WHEN '{}'::json THEN 1 ELSE 2 END".into(),
        format!("SELECT '{{}}'::json = ANY (SELECT payload FROM {table} WHERE false)"),
        format!("CREATE INDEX {table}_payload_idx ON {table} (payload)"),
        format!("ALTER TABLE {table} ADD UNIQUE (payload)"),
        "SELECT '{}'::json UNION SELECT '{}'::json".into(),
        "VALUES ('{}'::json) ORDER BY 1".into(),
        "VALUES ('{}'::json), ('[]'::json) ORDER BY 1".into(),
        "SELECT least('{}'::json)".into(),
        "SELECT * FROM (VALUES ('{}'::json)) AS left_side(payload) JOIN (VALUES ('{}'::json)) AS right_side(payload) USING (payload)".into(),
        "WITH RECURSIVE documents(payload) AS (SELECT '{}'::json UNION SELECT payload FROM documents WHERE false) SELECT * FROM documents".into(),
        "SELECT true OR ('['::json IS NULL)".into(),
        "SELECT '[' WHERE false UNION ALL SELECT '{}'::json".into(),
        "SELECT '{}'::json UNION ALL SELECT '[' WHERE false".into(),
        "(SELECT '[' LIMIT 0) UNION ALL SELECT '{}'::json".into(),
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, &sql, RowOrder::Ordered);
    }
    let drop = format!("DROP TABLE {table}");
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        &drop,
        RowOrder::Unordered,
    );
}

#[test]
fn matches_jsonb_normalization_comparison_and_storage() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        "SELECT JSONB '{\"b\":2,\"aa\":1,\"b\":1.00}'",
        "SELECT '{}'::pg_catalog.jsonb, 'null'::jsonb, NULL::jsonb",
        r#"SELECT '{"$serde_json::private::Number":null}'::jsonb"#,
        r#"SELECT '{"$serde_json::private::Number":"not a number"}'::jsonb"#,
        r#"SELECT '{"a":1.230e-5,"b":-0.00,"c":1e+30}'::jsonb"#,
        r#"SELECT '{"\u0061":"\ud83c\udf0d", "a":"last", "z":"\b\f\n\r\t"}'::jsonb"#,
        "SELECT '{\"a\":1}'::json::jsonb::json::text, '[true]'::text::jsonb::varchar(4)",
        "SELECT '{}'::jsonb UNION ALL SELECT '[]'",
        "SELECT NULL UNION ALL SELECT '{}'::jsonb",
        "SELECT '1.00'::jsonb = '1e0'::jsonb, '1.00'::jsonb <= '1e0'::jsonb",
        "SELECT '[]'::jsonb < 'null'::jsonb, '[[]]'::jsonb > '[null]'::jsonb",
        "SELECT DISTINCT payload FROM (VALUES ('1'::jsonb), ('1.0'::jsonb), ('null'::jsonb), (NULL::jsonb)) AS d(payload) ORDER BY payload",
        "SELECT count(DISTINCT payload) FROM (VALUES ('1'::jsonb), ('1.0'::jsonb), ('null'::jsonb), (NULL::jsonb)) AS d(payload)",
        "SELECT payload::text, count(*) FROM (VALUES ('1'::jsonb), ('1.0'::jsonb), ('{}'::jsonb)) AS d(payload) GROUP BY payload ORDER BY payload",
        "SELECT '1'::jsonb UNION SELECT '1.0'::jsonb",
        "SELECT '1'::jsonb INTERSECT ALL SELECT '1.0'::jsonb",
        "SELECT '1'::jsonb EXCEPT SELECT '1.0'::jsonb",
        "CREATE TABLE jsonb_documents (id INT PRIMARY KEY, payload JSONB NOT NULL UNIQUE, fallback JSONB DEFAULT '{}')",
        "INSERT INTO jsonb_documents (id, payload) VALUES (1, '{\"amount\":{\"currency\":\"USD\",\"value\":\"12.50\"}}'::json), (2, '1.00') RETURNING *",
        "INSERT INTO jsonb_documents (id, payload) VALUES (3, '1') ON CONFLICT (payload) DO UPDATE SET id = excluded.id RETURNING *",
        "CREATE TABLE jsonb_other (payload JSONB)",
        "INSERT INTO jsonb_other VALUES ('1e0'), (NULL), ('{\"amount\":{\"value\":\"12.50\",\"currency\":\"USD\"}}')",
        "SELECT d.id FROM jsonb_documents d JOIN jsonb_other o ON d.payload = o.payload ORDER BY d.id",
        "CREATE TABLE jsonb_child (payload JSONB REFERENCES jsonb_documents(payload))",
        "INSERT INTO jsonb_child VALUES ('1.0')",
        "BEGIN",
        "UPDATE jsonb_documents SET payload = 'false' WHERE id = 1 RETURNING payload",
        "ROLLBACK",
        "SELECT * FROM jsonb_documents ORDER BY id",
    ] {
        assert_statement(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    let documents = [
        "[]",
        "null",
        "\"a\"",
        "\"z\"",
        "0",
        "1",
        "1.00",
        "false",
        "true",
        "[null]",
        "[[]]",
        "[1,2]",
        "[2]",
        "{}",
        "{\"aa\":0}",
        "{\"b\":0}",
        "{\"a\":1,\"b\":2}",
    ];
    let values = documents
        .iter()
        .enumerate()
        .map(|(index, doc)| format!("({index}, '{doc}'::jsonb)"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT a.id, b.id, a.payload = b.payload, a.payload < b.payload, a.payload > b.payload FROM (VALUES {values}) a(id,payload) CROSS JOIN (VALUES {values}) b(id,payload) ORDER BY a.id,b.id"
    );
    assert_statement(&runtime, &mut postgres, &mut fake, &sql, RowOrder::Ordered);
    for sql in [
        r#"SELECT '"\u0000"'::jsonb"#,
        r#"SELECT '"\ud800"'::jsonb"#,
        r#"SELECT '"\udc00"'::jsonb"#,
        r#"SELECT '{"a":"\u0000","a":1}'::jsonb"#,
        r#"SELECT '{"a":1e131072,"a":1}'::jsonb"#,
        "SELECT '1e131072'::jsonb",
        "SELECT '1e-16384'::jsonb",
        "SELECT '0e131072'::jsonb",
        "SELECT '0e-16384'::jsonb",
        "SELECT '0e1073741823'::jsonb",
        "SELECT '0e1073741824'::jsonb",
        "SELECT '[1,]'::jsonb",
        "SELECT true OR ('['::jsonb IS NULL)",
        "SELECT '[' WHERE false UNION ALL SELECT '{}'::jsonb",
        "SELECT 1::jsonb",
        "SELECT '{}'::jsonb = '{}'::json",
        "INSERT INTO jsonb_documents (id, payload) VALUES (5, '1.0')",
        "INSERT INTO jsonb_documents (id, payload) VALUES (5, NULL)",
        "INSERT INTO jsonb_documents (id, payload) VALUES (5, '{}'::text)",
        "INSERT INTO jsonb_documents (id, payload) VALUES (5, '2'), (6, '[1,]')",
        "INSERT INTO jsonb_child VALUES ('2')",
        "DELETE FROM jsonb_documents WHERE id = 3",
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
    let nested = format!("{}0{}", "[".repeat(512), "]".repeat(512));
    assert_statement(
        &runtime,
        &mut postgres,
        &mut fake,
        &format!("SELECT '{nested}'::jsonb"),
        RowOrder::Ordered,
    );
}

#[test]
fn matches_json_operators_functions_and_expansion() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    for sql in [
        r#"CREATE TABLE json_operations (id int, payload jsonb)"#,
        r#"INSERT INTO json_operations VALUES (1,'{"a":1,"b":null}'),(2,'{}'),(3,NULL)"#,
        r#"SELECT '{"a":1, "a":2, "b":[null,"x",3]}'::json -> 'a', '{"a": [ null, "x" ]}'::json #> '{a}', '{"a":[null,"x"]}'::json #>> '{a,-1}'"#,
        r#"SELECT '[1,null,"a"]'::jsonb -> -1, '[1,null,"a"]'::jsonb ->> 1, 'null'::jsonb #> '{}', '"x"'::jsonb #>> '{}'"#,
        r#"SELECT '{"a":1}'::jsonb -> 0, '[1]'::jsonb -> '0', '{}'::jsonb #> '{missing}', '{}'::jsonb #> '{NULL}', NULL::jsonb -> 'x'"#,
        r#"SELECT '[1,[2]]'::jsonb @> '1', '[[1]]'::jsonb @> '[1]', '{"a":[1]}'::jsonb @> '{"a":1}', '[1,1]'::jsonb <@ '[1]'"#,
        r#"SELECT '"a"'::jsonb ? 'a', '["a",null,1]'::jsonb ?| ARRAY['b','a'], '{}'::jsonb ?& '{NULL}', '{}'::jsonb ?| '{}'"#,
        r#"SELECT '{"a":1}'::jsonb || '{"a":2,"b":3}', '{}'::jsonb || '{}', '[]'::jsonb || '1', 'null'::jsonb || '[2]'"#,
        r#"SELECT '{"a":1,"b":2}'::jsonb - 'a', '["a","a",1,null]'::jsonb - 'a', '[1,2,3]'::jsonb - -1, '[1]'::jsonb - 99"#,
        r#"SELECT '{"a":[1,2]}'::jsonb #- '{a,0}', jsonb_set('{"a":[1,2]}','{a,-1}','3'), jsonb_set('{}','{a,b}','1'), jsonb_set('[]','{-5}','1')"#,
        r#"SELECT jsonb_set('{}','{x}','1',false), jsonb_set('{"a":1}','{}','2'), jsonb_set('{"a":1}','{a,b}','2')"#,
        r#"SELECT json_build_object('a',1,'a',2,'b',NULL,true,'x'), json_build_array(1,'x',true,NULL,'{ "x": 2 }'::json), jsonb_build_object('a',1,'a',2), jsonb_build_array()"#,
        r#"SELECT to_json(12.50::numeric), to_jsonb(true), to_json('hello'::text), to_json(NULL::int), to_jsonb('"x"'::json), to_jsonb(ARRAY['a',NULL])"#,
        r#"SELECT json_typeof('null'), jsonb_typeof('"x"'), json_array_length('[1,null]'), jsonb_array_length('[]')"#,
        r#"SELECT ('{"amount":{"value":12.50}}'::jsonb #> '{amount,value}')::numeric::bigint, ('{"amount":{"value":"12.50"}}'::jsonb #>> '{amount,value}')::numeric::bigint"#,
        r#"SELECT * FROM json_each('{"a":1,"a":2,"b":null}') AS e ORDER BY key,value::text"#,
        r#"SELECT * FROM jsonb_each_text('{"b":null,"a":[1,2]}') AS e ORDER BY key"#,
        r#"SELECT * FROM json_object_keys('{"a":1,"a":2}') AS k"#,
        r#"SELECT * FROM jsonb_array_elements_text('[1,"x",null]') WITH ORDINALITY AS a(value,n) ORDER BY n"#,
        r#"SELECT d.id,e.key,e.value FROM json_operations d, jsonb_each(d.payload) e ORDER BY d.id,e.key"#,
        r#"SELECT d.id,e.key,e.value FROM json_operations d LEFT JOIN LATERAL jsonb_each(d.payload) e ON true ORDER BY d.id,e.key"#,
        r#"SELECT d.id,e.key FROM json_operations d LEFT JOIN jsonb_each(d.payload) e ON e.key='missing' ORDER BY d.id"#,
        r#"SELECT e.value,a.value FROM jsonb_each('{"a":[1,2]}') e, jsonb_array_elements(e.value) a ORDER BY a.value"#,
        r#"SELECT count(*) FROM json_operations d, jsonb_object_keys(d.payload) k"#,
        r#"SELECT * FROM jsonb_array_elements(NULL) a"#,
        r#"SELECT json_array_length('{}')"#,
        r#"SELECT * FROM json_each('[]')"#,
        r#"SELECT jsonb_set('1','{}','2')"#,
        r#"SELECT jsonb_set('[]','{x}','2')"#,
        r#"SELECT jsonb_set('{}','{NULL}','2')"#,
        r#"SELECT '{}'::jsonb #- '{NULL}'"#,
        r#"SELECT '1'::jsonb - 'x'"#,
        r#"SELECT '{}'::jsonb - 0"#,
        r#"SELECT json_build_object('a')"#,
        r#"SELECT json_build_object(NULL,1)"#,
        r#"SELECT to_json('x')"#,
        r#"SELECT to_json(NULL)"#,
        r#"SELECT '"12"'::jsonb::numeric"#,
        r#"SELECT 'null'::jsonb::numeric"#,
        r#"SELECT '[1]'::jsonb -> 1::bigint"#,
        r#"SELECT jsonb_array_length('{}'::json)"#,
        r#"SELECT d.id FROM json_operations d RIGHT JOIN jsonb_each(d.payload) e ON true"#,
        r#"SELECT '{}'::jsonb || '{"x":"{,x,}"}'"#,
        r#"SELECT jsonb_build_object(NULL,1)"#,
        r#"SELECT j.value,j.*,g.value FROM (jsonb_each('{}') e FULL JOIN jsonb_each('{}') f USING(value)) j FULL JOIN jsonb_each('{"a":[1]}') g USING(value)"#,
        r#"SELECT j.* FROM (jsonb_each('{}') e FULL JOIN jsonb_each('{}') f USING(value)) j(v,k1,k2) FULL JOIN jsonb_each('{"a":[1]}') g(k3,v) USING(v) GROUP BY j.v,j.k1,j.k2"#,
        r#"SELECT j.value,j.* FROM (jsonb_each('{}') e FULL JOIN jsonb_each('{"a":[1]}') f USING(value)) j"#,
        r#"SELECT * FROM jsonb_each('{}') e FULL JOIN jsonb_each('{"a":[1]}') f USING(value) FULL JOIN jsonb_each('{}') g USING(value)"#,
        r#"SELECT * FROM jsonb_each('{}') e FULL JOIN jsonb_each('{"a":[1]}') f USING(value) JOIN jsonb_each('{"b":[1]}') g USING(value)"#,
        r#"SELECT e.*,f.* FROM jsonb_each('{"a":[1]}') e JOIN jsonb_each('{"b":[1]}') f USING(value)"#,
        r#"SELECT j.* FROM (jsonb_each('{}') e FULL JOIN jsonb_each('{"b":[1]}') f USING(value)) j(v,k1,k2)"#,
        r#"SELECT * FROM (jsonb_each('{}') e FULL JOIN jsonb_each('{"b":[1]}') f USING(value)) j(v,k1,k2)"#,
        r#"SELECT * FROM jsonb_each('{"a":[1]}') e JOIN jsonb_each('{"b":[1]}') f USING(value), jsonb_array_elements(value) x"#,
        r#"SELECT * FROM jsonb_each('{"a":[1]}') e JOIN jsonb_each('{"b":[1]}') f USING(value) CROSS JOIN jsonb_array_elements(value) x"#,
        r#"SELECT e.key FROM (SELECT '{"a":1}'::jsonb AS payload) d RIGHT JOIN jsonb_each((SELECT d.payload FROM (SELECT '{"b":2}'::jsonb AS payload) d)) e ON true"#,
        r#"SELECT * FROM jsonb_each('{"a":[1]}') e, jsonb_array_elements(value) a"#,
        r#"SELECT * FROM (jsonb_each('{"a":[1]}') e CROSS JOIN jsonb_array_elements(value) a) j(k,v,x)"#,
        r#"SELECT d.payload,e.key FROM (SELECT '{"a":1}'::jsonb AS payload) d RIGHT JOIN (jsonb_each(d.payload) e CROSS JOIN (SELECT 1) x) ON true"#,
        r#"SELECT d.payload,e.key FROM (SELECT '{"a":1}'::jsonb AS payload) d FULL JOIN (jsonb_each(d.payload) e CROSS JOIN (SELECT 1) x) ON true"#,
        r#"SELECT * FROM jsonb_each((SELECT '{"a":1}'::jsonb)) e"#,
        r#"SELECT d.id,e.key FROM json_operations d, jsonb_each((SELECT d.payload)) e ORDER BY d.id,e.key"#,
        r#"SELECT d.id,e.value FROM (VALUES (1,'[1,2]'::jsonb)) d(id,payload), (jsonb_array_elements(d.payload) e CROSS JOIN (VALUES (1)) x(id)) ORDER BY e.value"#,
        r#"SELECT d.id,e.value FROM (VALUES (1,'[1,2]'::jsonb)) d(id,payload) JOIN (jsonb_array_elements(d.payload) e CROSS JOIN (VALUES (1)) x(id)) ON true ORDER BY e.value"#,
        r#"SELECT d.payload,e.key FROM (SELECT '{"a":1}'::jsonb AS payload) d, (SELECT 1 AS id) x RIGHT JOIN jsonb_each(d.payload) e ON true"#,
        r#"SELECT k.ordinality FROM jsonb_object_keys('{"a":1}') WITH ORDINALITY AS k"#,
        r#"SELECT '{}'::jsonb || 'foo'::text, 'foo'::text || '{}'::jsonb, '{}'::jsonb || 'foo'::varchar"#,
        r#"SELECT '{}' -> true"#,
        r#"SELECT '{}' -> 'a'"#,
        r#"SELECT NULL ->> 1"#,
        r#"SELECT '{}' @> '{}'"#,
        r#"SELECT '{}' ? 'a'"#,
        r#"SELECT '{"a":1}'::jsonb #- '{a}'"#,
        r#"SELECT * FROM jsonb_each('{"a":1,"b":2}') e WHERE key='a'"#,
        r#"SELECT to_json(' { "a":1 } '::json), json_build_array(' 1 '::json), ' { "a":1 } '::json #> '{}'"#,
        r#"SELECT '[1]'::jsonb #> '{"0 "}', jsonb_set('[1]','{"0 "}','2')"#,
        r#"SELECT * FROM jsonb_object_keys('{"aa":1,"b":2}') WITH ORDINALITY AS k"#,
        r#"SELECT * FROM json_array_elements('[ 1, { "a" : 2 }, null ]') e"#,
        r#"SELECT * FROM json_array_elements_text('[1,{"a":2},null]') e"#,
        r#"SELECT * FROM jsonb_each('{"a":1}') e RIGHT JOIN jsonb_each('{}') x ON true"#,
        r#"SELECT * FROM jsonb_each('{}') e FULL JOIN jsonb_each('{"b":2}') x ON true"#,
        r#"SELECT jsonb_set('{}','{NULL}','1',false)"#,
        r#"SELECT jsonb_set('[]','{NULL}','1',false)"#,
        r#"SELECT '{"a":1}'::jsonb #- '{a,NULL}'"#,
        r#"SELECT '{"a":1}'::jsonb #- '{NULL}'"#,
        r#"SELECT '[1]'::jsonb #> '{0 }', jsonb_set('[1]','{0 }','2')"#,
        r#"SELECT json_build_object('{}'::json,1)"#,
        r#"SELECT jsonb_build_object('{}'::jsonb,1)"#,
        r#"SELECT to_json('2024-01-02 03:04:05+00'::timestamptz)"#,
        r#"SELECT '[1,2]'::jsonb #> ARRAY[NULL]"#,
        r#"SELECT * FROM json_each('{"x":"\\u0000"}') e"#,
    ] {
        assert_statement_allow_error(&runtime, &mut postgres, &mut fake, sql, RowOrder::Ordered);
    }
}

#[test]
fn matches_json_prepared_parameters_and_metadata() {
    let server = start_isolated_postgres_server();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut postgres = runtime
        .block_on(PgConnection::connect(&server.url))
        .unwrap();
    let mut fake = PgFakeConnection::new(Db::create());
    let indexed = sqlx::types::Json(serde_json::json!([1, 2]));
    let query = "SELECT $1 ->> $2";
    let expected: String = runtime
        .block_on(
            sqlx::query_scalar(AssertSqlSafe(query))
                .bind(&indexed)
                .bind(1_i32)
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let actual: String = runtime
        .block_on(
            sqlx::query_scalar(AssertSqlSafe(query))
                .bind(&indexed)
                .bind(1_i32)
                .fetch_one(&mut fake),
        )
        .unwrap();
    assert_eq!(actual, expected);
    let document = sqlx::types::Json(serde_json::json!({"a":[1,null,"x"]}));
    let path = vec!["a".to_owned(), "-1".to_owned()];
    let sql =
        "SELECT $1 #> $2 AS value, $1 #>> $2 AS text, jsonb_set($1,$2,'42') AS updated, $2 AS path";
    let real = runtime
        .block_on(
            sqlx::query(AssertSqlSafe(sql))
                .bind(&document)
                .bind(&path)
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let actual = runtime
        .block_on(
            sqlx::query(AssertSqlSafe(sql))
                .bind(&document)
                .bind(&path)
                .fetch_one(&mut fake),
        )
        .unwrap();
    for (real, actual) in real.columns().iter().zip(actual.columns()) {
        assert_eq!(real.name(), actual.name());
        assert_eq!(
            real.type_info().oid().unwrap().0,
            actual.type_info().base.unwrap().map_to_oid()
        );
    }
    assert_eq!(
        actual.get::<sqlx::types::Json<serde_json::Value>, _>(0),
        real.get::<sqlx::types::Json<serde_json::Value>, _>(0)
    );
    assert_eq!(actual.get::<String, _>(1), real.get::<String, _>(1));
    assert_eq!(
        actual.get::<sqlx::types::Json<serde_json::Value>, _>(2),
        real.get::<sqlx::types::Json<serde_json::Value>, _>(2)
    );
    assert_eq!(
        actual.get::<Vec<String>, _>(3),
        real.get::<Vec<String>, _>(3)
    );
    let sql = "SELECT * FROM jsonb_each($1) AS item";
    let real = runtime
        .block_on(
            sqlx::query(AssertSqlSafe(sql))
                .bind(&document)
                .fetch_one(&mut postgres),
        )
        .unwrap();
    let actual = runtime
        .block_on(
            sqlx::query(AssertSqlSafe(sql))
                .bind(&document)
                .fetch_one(&mut fake),
        )
        .unwrap();
    for (real, actual) in real.columns().iter().zip(actual.columns()) {
        assert_eq!(real.name(), actual.name());
        assert_eq!(
            real.type_info().oid().unwrap().0,
            actual.type_info().base.unwrap().map_to_oid()
        );
    }
    assert_eq!(actual.get::<String, _>(0), real.get::<String, _>(0));
    assert_eq!(
        actual.get::<sqlx::types::Json<serde_json::Value>, _>(1),
        real.get::<sqlx::types::Json<serde_json::Value>, _>(1)
    );
}
