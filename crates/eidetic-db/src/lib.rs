//! No other crate touches `sqlx::Pool` directly; all DB access goes through
//! `AssetsRepo`.
//!
//! Storage is embedded SQLite (ADR-0005): one file, no server process. The
//! pool is opened with WAL journaling so readers never block the single
//! writer, and with a busy timeout so concurrent commands wait rather than
//! failing with `SQLITE_BUSY`.

use eidetic_core::Config;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::time::Duration;
use tracing::info;

pub mod assets;
pub use assets::{
    AssetDetail, AssetsRepo, InsertOutcome, LibraryStats, NewAsset, RecentAsset, SearchResult,
};

pub mod vector;
pub use vector::{BruteForce, VectorIndex};

pub mod error;
pub use error::{Error, Result};

/// Open (creating if absent) the SQLite database and run pending migrations.
///
/// sqlx takes a migration lock internally, so concurrent startups from
/// multiple processes are safe.
pub async fn connect(config: &Config) -> Result<SqlitePool> {
    let path = &config.database_path;
    info!(database_path = %path.display(), "opening sqlite database");

    // SQLite creates the file, but not its parent directory.
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| Error::OpenFile {
            path: parent.display().to_string(),
            source: e,
        })?;
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        // WAL: concurrent readers alongside the single writer. NORMAL sync is
        // the recommended pairing — durable across app crashes, trading only
        // the OS-crash window, which is acceptable for a re-importable library.
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(30));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .map_err(Error::Connect)?;

    info!("running migrations");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .map_err(Error::Migrate)?;

    Ok(pool)
}
