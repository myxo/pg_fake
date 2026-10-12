use pg_fake::{Db, value::Value};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
fn format_uuid(kind: u32, i: usize) -> String {
    format!("{kind:08x}-0000-0000-0000-{i:012x}")
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let case = &args[1];
    let n: usize = args[2].parse().unwrap();
    let profile: u64 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(0);
    let plain = case.ends_with("_plain");
    let query_case = case.trim_end_matches("_plain");
    let db = Db::create();
    let mut s = db.create_session();
    s.execute("CREATE TABLE accounts(id UUID PRIMARY KEY,name TEXT,email TEXT,home_group_id UUID);
 CREATE TABLE account_roles(account_id UUID NOT NULL REFERENCES accounts(id),role TEXT NOT NULL,UNIQUE(account_id,role));
 CREATE INDEX account_roles_account_idx ON account_roles(account_id);
 CREATE TABLE access_groups(id UUID PRIMARY KEY,name TEXT NOT NULL,display_name TEXT,deleted_at TIMESTAMPTZ);
 CREATE TABLE memberships(id UUID PRIMARY KEY,group_id UUID NOT NULL REFERENCES access_groups(id),account_id UUID NOT NULL REFERENCES accounts(id),role TEXT NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,deleted_at TIMESTAMPTZ);
 CREATE UNIQUE INDEX memberships_active_pair ON memberships(group_id,account_id) WHERE deleted_at IS NULL;
 CREATE INDEX memberships_active_account ON memberships(account_id,group_id) WHERE deleted_at IS NULL;
 CREATE TABLE credentials(id UUID PRIMARY KEY,group_id UUID NOT NULL REFERENCES access_groups(id),account_id UUID NOT NULL REFERENCES accounts(id),name TEXT,deleted_at TIMESTAMPTZ);").unwrap();
    for table in [
        "accounts",
        "access_groups",
        "account_roles",
        "memberships",
        "credentials",
    ] {
        let values = (1..=n)
            .map(|i| {
                let a = format_uuid(1, i);
                let g = format_uuid(2, i);
                let m = format_uuid(3, i);
                let c = format_uuid(4, i);
                match table {
                    "accounts" => format!("('{a}','account {i}','a{i}@example.invalid','{g}')"),
                    "access_groups" => format!("('{g}','group {i}',NULL,NULL)"),
                    "account_roles" => format!("('{a}','member')"),
                    "memberships" => format!(
                        "('{m}','{g}','{a}','member',TIMESTAMPTZ '2024-01-01 00:00:00+00',NULL)"
                    ),
                    "credentials" => format!("('{c}','{g}','{a}','credential {i}',NULL)"),
                    _ => unreachable!(),
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        s.execute(&format!("INSERT INTO {table} VALUES {values}"))
            .unwrap();
    }
    if plain {
        s.execute("CREATE INDEX diagnostic_memberships_account ON memberships(account_id); CREATE INDEX diagnostic_memberships_group ON memberships(group_id)").unwrap();
    }
    let (sql, kinds, write) = match query_case {
        "account" => (
            "SELECT id,name,email,home_group_id FROM accounts WHERE id=$1",
            vec![1],
            false,
        ),
        "left_roles" => (
            "SELECT a.id,a.name,a.email,a.home_group_id,r.role FROM accounts a LEFT JOIN account_roles r ON r.account_id=a.id WHERE a.id=$1",
            vec![1],
            false,
        ),
        "left_group" => (
            "SELECT c.id,c.account_id,c.group_id,COALESCE(c.name,'') AS name,COALESCE(g.name,'') AS group_name FROM credentials c LEFT JOIN access_groups g ON g.id=c.group_id WHERE c.id=$1 AND c.deleted_at IS NULL",
            vec![4],
            false,
        ),
        "memberships" => (
            "SELECT g.id,g.name,g.display_name,m.role,(a.home_group_id=g.id) AS is_home FROM memberships m JOIN access_groups g ON g.id=m.group_id JOIN accounts a ON a.id=m.account_id WHERE m.account_id=$1 AND m.deleted_at IS NULL AND g.deleted_at IS NULL ORDER BY m.created_at,m.id",
            vec![1],
            false,
        ),
        "group_members" => (
            "SELECT m.account_id,a.name,home.name AS home_group_name,m.role FROM memberships m JOIN access_groups g ON g.id=m.group_id JOIN accounts a ON a.id=m.account_id JOIN access_groups home ON home.id=a.home_group_id WHERE m.group_id=$1 AND m.deleted_at IS NULL AND g.deleted_at IS NULL AND home.deleted_at IS NULL ORDER BY m.created_at,m.id",
            vec![2],
            false,
        ),
        "composite" => (
            "SELECT role FROM memberships WHERE account_id=$1 AND group_id=$2 AND deleted_at IS NULL",
            vec![1, 2],
            false,
        ),
        "prefix" => (
            "SELECT group_id FROM memberships WHERE account_id=$1 AND deleted_at IS NULL ORDER BY group_id",
            vec![1],
            false,
        ),
        "update" => (
            "UPDATE credentials SET name='changed' WHERE id=$1",
            vec![4],
            true,
        ),
        "guard_update" => (
            "UPDATE credentials SET name='changed' WHERE id=$1 AND group_id=$2 AND account_id=$3 AND deleted_at IS NULL",
            vec![4, 2, 1],
            true,
        ),
        "guard_softdelete" => (
            "UPDATE credentials SET deleted_at=CURRENT_TIMESTAMP WHERE id=$1 AND group_id=$2 AND account_id=$3 AND deleted_at IS NULL",
            vec![4, 2, 1],
            true,
        ),
        "delete" => ("DELETE FROM credentials WHERE id=$1", vec![4], true),
        "guard_delete" => (
            "DELETE FROM credentials WHERE id=$1 AND group_id=$2 AND account_id=$3 AND deleted_at IS NULL",
            vec![4, 2, 1],
            true,
        ),
        _ => panic!("unknown case"),
    };
    let statement = s.prepare(sql).unwrap();
    let parameters: Vec<Vec<Value>> = (1..=n)
        .map(|i| {
            kinds
                .iter()
                .map(|k| Value::Uuid(format_uuid(*k, i).parse().unwrap()))
                .collect()
        })
        .collect();
    if write {
        s.execute("BEGIN").unwrap();
        let affected = s.execute_prepared(&statement, &parameters[0]).unwrap();
        assert_eq!(affected, 1);
        s.execute("ROLLBACK").unwrap();
    } else {
        let result = s.query_prepared(&statement, &parameters[0]).unwrap();
        assert_eq!(result.rows.len(), 1);
    }
    let mut index = 0;
    let mut operation = || {
        let params = &parameters[index % n];
        index += 1;
        if write {
            s.execute("BEGIN").unwrap();
        }
        let start = Instant::now();
        if write {
            black_box(s.execute_prepared(&statement, params).unwrap());
        } else {
            black_box(s.query_prepared(&statement, params).unwrap());
        }
        let elapsed = start.elapsed();
        if write {
            s.execute("ROLLBACK").unwrap();
        }
        elapsed
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
        let mut elapsed = Duration::ZERO;
        let mut count = 0;
        while start.elapsed() < Duration::from_millis(120) {
            elapsed += operation();
            count += 1;
        }
        times.push(elapsed.as_secs_f64() * 1e6 / count as f64);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{case},{n},1,{:.3},{:.3},{:.3}",
        times[4], times[0], times[8]
    );
}
