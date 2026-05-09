use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    // No `#[source]`: the sqlx parse error can echo the raw URL (with password)
    // through Display chains, so we drop the inner cause on purpose.
    #[error("invalid EIDETIC_DATABASE_URL")]
    InvalidUrl,

    #[error("failed to connect to database: {0}")]
    Connect(#[source] sqlx::Error),

    #[error("failed to run migrations: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),

    #[error("database query failed: {0}")]
    Query(#[source] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
