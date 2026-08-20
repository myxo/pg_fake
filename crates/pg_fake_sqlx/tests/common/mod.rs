use std::{env, path::PathBuf};

use testcontainers::{Container, ImageExt, runners::SyncRunner};
use testcontainers_modules::postgres::Postgres;

pub(crate) struct PostgresServer {
    pub(crate) url: String,
    _container: Option<Container<Postgres>>,
}

pub(crate) fn start_postgres_server() -> PostgresServer {
    let configured_url = dotenvy::var("PG_FAKE_DATABASE_URL").ok();
    if configured_url.is_none() && env::var_os("DOCKER_HOST").is_none() {
        let socket = PathBuf::from(env::var_os("HOME").expect("HOME must be set"))
            .join(".colima/default/docker.sock");
        if socket.exists() {
            unsafe { env::set_var("DOCKER_HOST", format!("unix://{}", socket.display())) };
        }
    }
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
