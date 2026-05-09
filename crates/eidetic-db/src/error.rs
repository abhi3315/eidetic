use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to connect to database: {0}")]
    Connect(#[source] sqlx::Error),

    #[error("failed to run migrations: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),

    #[error("database query failed: {0}")]
    Query(#[source] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
