//! No other crate touches `sqlx::Pool` directly; all DB access goes through
//! `PgAssetsRepo`.

use eidetic_core::Config;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use tracing::info;

pub mod assets;
pub use assets::{
    AssetDetail, ExifUpdate, InsertOutcome, LibraryStats, NewAsset, PgAssetsRepo, RecentAsset,
    SearchResult,
};

pub mod error;
pub use error::{Error, Result};

/// Connect to Postgres and run any pending migrations.
///
/// sqlx uses advisory locks internally, so concurrent startups from multiple
/// processes are safe.
pub async fn connect(config: &Config) -> Result<PgPool> {
    info!(database_url = %sanitize_url(&config.database_url), "connecting to postgres");

    // Parse the URL ourselves so any later sqlx error references structured
    // fields (host, port, user) instead of round-tripping the raw URL, which
    // sqlx::Error::Configuration would otherwise echo verbatim through anyhow.
    let options =
        PgConnectOptions::from_str(&config.database_url).map_err(|_| Error::InvalidUrl)?;

    let pool = PgPoolOptions::new()
        .max_connections(10)
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
    fn sanitize_url_without_password_returns_unchanged() {
        // URL with user but no password, no colon in creds, should pass through
        let url = "postgres://eidetic@localhost:5432/eidetic";
        assert_eq!(sanitize_url(url), url);
    }
}
