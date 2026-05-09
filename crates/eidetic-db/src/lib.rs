//! Database access for Eidetic.
//!
//! This crate owns:
//! - The Postgres connection pool
//! - The migration runner (migrations live at the workspace root)
//! - Repository traits and their Postgres implementations
//!
//! No other crate touches `sqlx::Pool` directly. Other crates depend on
//! the repository traits (e.g. [`AssetsRepo`]), not on the pool itself.
//! This keeps SQL out of the rest of the codebase and makes testing
//! easier.

use eidetic_core::Config;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

pub mod assets;
pub use assets::{PgAssetsRepo, SearchResult};

pub mod error;
pub use error::{Error, Result};

/// Connect to Postgres, run pending migrations, return a pooled handle.
///
/// Migrations are embedded at compile time from the workspace-root
/// `migrations/` directory. They are idempotent and safe to run on every
/// startup; sqlx uses Postgres advisory locks internally so concurrent
/// runs from multiple processes are safe.
pub async fn connect(config: &Config) -> Result<PgPool> {
    info!(database_url = %sanitize_url(&config.database_url), "connecting to postgres");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .map_err(Error::Connect)?;

    info!("running migrations");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .map_err(Error::Migrate)?;

    Ok(pool)
}

/// Strip the password from a Postgres URL before logging it.
fn sanitize_url(url: &str) -> String {
    if let Some(at_idx) = url.find('@')
        && let Some(scheme_end) = url.find("://")
        && scheme_end + 3 < at_idx
    {
        let creds_start = scheme_end + 3;
        if let Some(colon_idx) = url[creds_start..at_idx].find(':') {
            let mut sanitized = String::with_capacity(url.len());
            sanitized.push_str(&url[..creds_start + colon_idx + 1]);
            sanitized.push_str("****");
            sanitized.push_str(&url[at_idx..]);
            return sanitized;
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_redacts_password() {
        let url = "postgres://eidetic:secret@localhost:5432/eidetic";
        assert_eq!(
            sanitize_url(url),
            "postgres://eidetic:****@localhost:5432/eidetic"
        );
    }

    #[test]
    fn sanitize_url_passes_through_when_no_credentials() {
        let url = "postgres://localhost:5432/eidetic";
        assert_eq!(sanitize_url(url), url);
    }

    #[test]
    fn sanitize_url_without_password_returns_unchanged() {
        // URL with user but no password — no colon in creds, should pass through
        let url = "postgres://eidetic@localhost:5432/eidetic";
        assert_eq!(sanitize_url(url), url);
    }
}
