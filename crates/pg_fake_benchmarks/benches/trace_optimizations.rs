use std::time::Duration;

use criterion::{BatchSize, Criterion, Throughput, black_box, criterion_group, criterion_main};
use pg_fake::api::{Db, Session};

fn execute(session: &mut Session, sql: &str) {
    black_box(session.execute(sql).unwrap());
}

fn build_rows(rows: usize, columns: impl Fn(usize) -> String) -> String {
    (1..=rows).map(columns).collect::<Vec<_>>().join(",")
}

fn create_expression_session() -> Session {
    let mut session = Db::create().create_session();
    let rows = build_rows(100, |id| {
        format!("({id}, 'user {id}', {}, {id}, NULL)", id % 2 == 0)
    });
    execute(
        &mut session,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, active BOOLEAN, score INTEGER, manager_id INTEGER)",
    );
    execute(&mut session, &format!("INSERT INTO users VALUES {rows}"));
    session
}

fn create_mutation_session() -> Session {
    let mut session = Db::create().create_session();
    let rows = build_rows(1_000, |id| format!("({id}, 0)"));
    execute(
        &mut session,
        "CREATE TABLE mutation_rows (id INTEGER PRIMARY KEY, score INTEGER)",
    );
    execute(
        &mut session,
        &format!("INSERT INTO mutation_rows VALUES {rows}"),
    );
    session
}

fn create_join_session() -> Session {
    let mut session = Db::create().create_session();
    let users = build_rows(30, |id| format!("({id}, 'user {id}')"));
    let memberships = build_rows(30, |id| format!("({id}, {id})"));
    let teams = build_rows(30, |id| format!("({id}, 'team {id}')"));
    execute(
        &mut session,
        "CREATE TABLE join_users (id INTEGER PRIMARY KEY, name TEXT); \
         CREATE TABLE join_memberships (user_id INTEGER, team_id INTEGER); \
         CREATE TABLE join_teams (id INTEGER PRIMARY KEY, name TEXT)",
    );
    execute(
        &mut session,
        &format!(
            "INSERT INTO join_users VALUES {users}; \
             INSERT INTO join_memberships VALUES {memberships}; \
             INSERT INTO join_teams VALUES {teams}"
        ),
    );
    session
}

fn create_catalog_session() -> Session {
    let mut session = Db::create().create_session();
    let mut sql = String::from(
        "CREATE TABLE catalog_target (id INTEGER PRIMARY KEY); INSERT INTO catalog_target VALUES (1);",
    );
    for id in 1..=100 {
        sql.push_str(&format!("CREATE TABLE unrelated_{id} (id INTEGER);"));
    }
    execute(&mut session, &sql);
    session
}

fn create_order_session() -> Session {
    let mut session = Db::create().create_session();
    let rows = build_rows(1_000, |id| format!("({id}, {})", 1_001 - id));
    execute(
        &mut session,
        "CREATE TABLE ordered_rows (id INTEGER, score INTEGER)",
    );
    execute(
        &mut session,
        &format!("INSERT INTO ordered_rows VALUES {rows}"),
    );
    session
}

fn create_insert_database() -> Db {
    let db = Db::create();
    let mut session = db.create_session();
    execute(
        &mut session,
        "CREATE TABLE inserted_rows (id INTEGER PRIMARY KEY, name TEXT, active BOOLEAN DEFAULT true, score INTEGER DEFAULT 0)",
    );
    db
}

fn benchmark_trace_optimizations(criterion: &mut Criterion) {
    let mut expression_session = create_expression_session();
    let mut group = criterion.benchmark_group("trace_select_expression_100");
    group.throughput(Throughput::Elements(100));
    group.bench_function("generic_execute", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut expression_session,
                "SELECT id, score + 1 FROM users WHERE active = true",
            );
        });
    });
    group.finish();

    let mut filter_session = create_expression_session();
    let mut group = criterion.benchmark_group("trace_select_filter_100");
    group.throughput(Throughput::Elements(50));
    group.bench_function("generic_execute", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut filter_session,
                "SELECT id, name FROM users WHERE active = true",
            );
        });
    });
    group.finish();

    let mut mutation_session = create_mutation_session();
    let mut group = criterion.benchmark_group("trace_update_point_1000");
    group.throughput(Throughput::Elements(1));
    group.bench_function("hit", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut mutation_session,
                "UPDATE mutation_rows SET score = score + 1 WHERE id = 500",
            );
        });
    });
    group.bench_function("miss", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut mutation_session,
                "UPDATE mutation_rows SET score = 0 WHERE id = 2000",
            );
        });
    });
    group.finish();

    let mut broad_mutation_session = create_mutation_session();
    let mut group = criterion.benchmark_group("trace_update_many_1000");
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("non_indexed", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut broad_mutation_session,
                "UPDATE mutation_rows SET score = score + 1 WHERE score >= 0",
            );
        });
    });
    group.finish();

    let mut join_session = create_join_session();
    let mut group = criterion.benchmark_group("trace_join_chain_30");
    group.throughput(Throughput::Elements(30));
    group.bench_function("inner", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut join_session,
                "SELECT u.name, t.name FROM join_users u JOIN join_memberships m ON m.user_id = u.id JOIN join_teams t ON t.id = m.team_id",
            );
        });
    });
    group.bench_function("left", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut join_session,
                "SELECT u.name, t.name FROM join_users u LEFT JOIN join_memberships m ON m.user_id = u.id LEFT JOIN join_teams t ON t.id = m.team_id",
            );
        });
    });
    group.finish();

    let mut catalog_session = create_catalog_session();
    let mut group = criterion.benchmark_group("trace_select_100_unrelated_tables");
    group.throughput(Throughput::Elements(1));
    group.bench_function("generic_execute", |benchmark| {
        benchmark.iter(|| execute(&mut catalog_session, "SELECT id FROM catalog_target"));
    });
    group.finish();

    let mut group = criterion.benchmark_group("trace_update_miss_100_unrelated_tables");
    group.throughput(Throughput::Elements(1));
    group.bench_function("generic_execute", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut catalog_session,
                "UPDATE catalog_target SET id = id WHERE id = 2",
            );
        });
    });
    group.finish();

    let mut group = criterion.benchmark_group("trace_update_hit_100_unrelated_tables");
    group.throughput(Throughput::Elements(1));
    group.bench_function("generic_execute", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut catalog_session,
                "UPDATE catalog_target SET id = id WHERE id = 1",
            );
        });
    });
    group.finish();

    let mut order_session = create_order_session();
    let mut group = criterion.benchmark_group("trace_order_limit_1000");
    group.throughput(Throughput::Elements(10));
    group.bench_function("top_10", |benchmark| {
        benchmark.iter(|| {
            execute(
                &mut order_session,
                "SELECT id FROM ordered_rows ORDER BY score DESC LIMIT 10",
            );
        });
    });
    group.finish();

    let insert = format!(
        "INSERT INTO inserted_rows (id, name) VALUES {}",
        build_rows(100, |id| format!("({id}, 'inserted row {id}')"))
    );
    let mut group = criterion.benchmark_group("trace_insert_values_100");
    group.throughput(Throughput::Elements(100));
    group.bench_function("generic_execute", |benchmark| {
        benchmark.iter_batched(
            || create_insert_database().create_session(),
            |mut session| execute(&mut session, &insert),
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group! {
    name = trace_optimizations;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2))
        .sample_size(20);
    targets = benchmark_trace_optimizations
}
criterion_main!(trace_optimizations);
