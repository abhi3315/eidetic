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

    /// A caller tried to persist a quality signal outside its contract
    /// (non-finite, or outside 0..1). Refused rather than clamped: a NaN
    /// that became 0.0 would read as "measured still shot".
    #[error("refusing to store {signal} = {value} for asset {asset}: expected a finite 0..1")]
    InvalidSignal {
        signal: &'static str,
        value: f32,
        asset: String,
    },

    /// Every stored embedding was produced by a different model than the one
    /// answering the query. Ranking across models is meaningless, so this
    /// fails loudly instead of returning an empty page that looks like "your
    /// library has nothing matching".
    #[error(
        "no embedding matches the active model {active}; stored: {stored}. Run `eidetic embed` \
         to re-embed the library (existing rows are kept until replaced), or set EIDETIC_MODEL \
         back to the variant that produced them. Ids without an @revision predate pinned \
         model revisions and always need re-embedding."
    )]
    ModelMismatch { active: String, stored: String },
}

pub type Result<T> = std::result::Result<T, Error>;
