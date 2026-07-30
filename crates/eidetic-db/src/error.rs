use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot create database directory {path}: {source}")]
    OpenFile {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to open database: {0}")]
    Connect(#[source] sqlx::Error),

    #[error("failed to run migrations: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),

    #[error("database query failed: {0}")]
    Query(#[source] sqlx::Error),

    #[error("corrupt row in {table}.{column}: {detail}")]
    CorruptRow {
        table: &'static str,
        column: &'static str,
        detail: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
