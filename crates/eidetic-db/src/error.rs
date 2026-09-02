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

    /// Every stored embedding was produced by a different model than the one
    /// answering the query. Ranking across widths is meaningless, so this
    /// fails loudly instead of returning an empty page that looks like "your
    /// library has nothing matching".
    #[error(
        "no embedding matches the active model: {stored_dims} stored, query is {query_dim}-dim. \
         Run `eidetic embed` to re-embed the library, or set EIDETIC_MODEL back to the variant \
         that produced it."
    )]
    ModelMismatch {
        query_dim: usize,
        stored_dims: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
