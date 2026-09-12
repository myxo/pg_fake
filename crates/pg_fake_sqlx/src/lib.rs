mod connection;
mod error;
mod migrate;
mod row;
mod types;

pub use connection::{
    PgFake, PgFakeConnectOptions, PgFakeConnection, PgFakePool, PgFakePoolOptions,
    PgFakeQueryResult, PgFakeStatement,
};
pub use error::PgFakeDatabaseError;
pub use pg_fake::api::Db;
pub use row::{PgFakeColumn, PgFakeRow, PgFakeValue, PgFakeValueRef};
pub use types::{PgFakeArguments, PgFakeTypeInfo};

sqlx_core::impl_acquire!(PgFake, PgFakeConnection);
