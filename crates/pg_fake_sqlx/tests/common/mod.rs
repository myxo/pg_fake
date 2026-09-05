use std::env;

use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;

pub(crate) struct PostgresServer {
    pub(crate) url: String,
    _container: Option<Container<Postgres>>,
}

pub(crate) fn start_postgres_server() -> PostgresServer {
    let configured_url = env::var("PG_FAKE_DATABASE_URL").ok().or_else(|| {
        dotenvy::dotenv_iter().ok()?.find_map(|entry| {
            let (key, value) = entry.expect("must parse test environment configuration");
            (key == "PG_FAKE_DATABASE_URL").then_some(value)
        })
    });
    let container = configured_url.is_none().then(|| {
        Postgres::default()
            .with_tag("18")
            .start()
            .expect("must start PostgreSQL 18 container")
    });
    let url = configured_url.unwrap_or_else(|| {
        let container = container.as_ref().expect("container must be started");
        format!(
            "postgresql://postgres:postgres@{}:{}/postgres",
            container
                .get_host()
                .expect("container host must be available"),
            container
                .get_host_port_ipv4(5432)
                .expect("PostgreSQL port must be available")
        )
    });
    PostgresServer {
        url,
        _container: container,
    }
}
