use pg_fake::Db;
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let case = args.get(1).expect("case");
    let fixture = args.get(4).map(String::as_str).unwrap_or("wide");
    let n: usize = args.get(2).expect("size").parse().unwrap();
    let profile: u64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(0);
    let db = Db::create();
    let mut s = db.create_session();
    s.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, bucket INTEGER, label TEXT, doc JSONB)")
        .unwrap();
    let fixture_rows = if case == "mvcc" { 1 } else { n };
    let values = (1..=fixture_rows).map(|i| if fixture == "null_payload" { format!("({i},{},NULL,NULL)", i % 10) } else { format!("({i},{},' value {i} ','{{\"a\":{i},\"nested\":{{\"x\":1}},\"tags\":[1,2,3]}}')",i%10) }).collect::<Vec<_>>().join(",");
    s.execute(&format!("INSERT INTO t VALUES {values}"))
        .unwrap();
    let sql = match case.as_str() {
        "scan" => "SELECT id FROM t".into(),
        "lookup" | "mvcc" => "SELECT id FROM t WHERE id=1".into(),
        "group" => "SELECT bucket,count(*),sum(id) FROM t GROUP BY bucket ORDER BY bucket".into(),
        "group_unique" => "SELECT id,count(*) FROM t GROUP BY id".into(),
        "distinct" => "SELECT DISTINCT id FROM t".into(),
        "distinct_low" => "SELECT DISTINCT bucket FROM t".into(),
        "union" => "SELECT id FROM t UNION SELECT id FROM t".into(),
        "union_all" => "SELECT id FROM t UNION ALL SELECT id FROM t".into(),
        "values_all" | "values_union" => {
            let v=(1..=n).map(|i|format!("({i})")).collect::<Vec<_>>().join(",");
            format!("VALUES {v} UNION {} VALUES {v}",if case=="values_all" {"ALL"} else {""})
        },
        "row_number" => "SELECT id,row_number() OVER (ORDER BY id DESC) FROM t".into(),
        "rank" => "SELECT id,rank() OVER w,dense_rank() OVER w,ntile(10) OVER w FROM t WINDOW w AS (PARTITION BY bucket ORDER BY id DESC)".into(),
        "moving" => "SELECT id,sum(id) OVER (PARTITION BY bucket ORDER BY id ROWS BETWEEN 2 PRECEDING AND 2 FOLLOWING),avg(id) OVER (PARTITION BY bucket ORDER BY id GROUPS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t".into(),
        "cumulative" => "SELECT id,sum(id) OVER (ORDER BY id ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t".into(),
        "json" => "SELECT id,doc->>'a',doc#>>'{nested,x}' FROM t".into(),
        "contain" => "SELECT id FROM t WHERE doc @> '{\"nested\":{\"x\":1}}'::jsonb".into(),
        "patterns" => "SELECT id,label ILIKE '%VALUE%',regexp_like(label,'[a-z]+ [0-9]+','i') FROM t".into(),
        "temporal" => "SELECT id,to_char(date_trunc('minute',to_timestamp(id)),'YYYY-MM-DD HH24:MI:SS'),floor(id::numeric / 7) FROM t".into(),
        "exists" => "SELECT a.id FROM t a WHERE EXISTS(SELECT 1 FROM t b WHERE b.bucket=a.bucket)".into(),
        "snapshot" => "SELECT id FROM t".into(),
        _ => panic!("unknown case"),
    };
    let stmt = s.prepare(&sql).unwrap();
    if case == "mvcc" {
        s.execute("BEGIN ISOLATION LEVEL REPEATABLE READ").unwrap();
        let result = s.query_prepared(&stmt, &[]).unwrap();
        assert_eq!(result.rows.len(), 1);
        drop(result);
        let mut writer = db.create_session();
        let update = writer
            .prepare("UPDATE t SET bucket=bucket+1 WHERE id=1")
            .unwrap();
        for _ in 0..n {
            writer.execute_prepared(&update, &[]).unwrap();
        }
    }
    let expected = match case.as_str() {
        "lookup" | "mvcc" => 1,
        "group" | "distinct_low" => n.min(10),
        "union_all" | "values_all" => 2 * n,
        _ => n,
    };
    let result = s.query_prepared(&stmt, &[]).unwrap();
    assert_eq!(result.rows.len(), expected);
    drop(result);
    let mut operation = || {
        if case == "snapshot" {
            black_box(db.snapshot());
        } else {
            black_box(s.query_prepared(&stmt, &[]).unwrap());
        }
    };
    for _ in 0..16 {
        operation();
    }
    if profile > 0 {
        eprintln!("READY {case} {n}");
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(profile) {
            operation();
        }
        return;
    }
    let mut times = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        let mut count = 0;
        while start.elapsed() < Duration::from_millis(120) {
            for _ in 0..16 {
                operation();
                count += 1;
            }
        }
        times.push(start.elapsed().as_secs_f64() * 1e6 / count as f64);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{case},{n},{fixture},{:.3},{:.3},{:.3}",
        times[4], times[0], times[8]
    );
}
