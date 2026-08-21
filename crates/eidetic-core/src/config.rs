use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level configuration for an Eidetic deployment.
///
/// v0 loads from defaults + environment variables (`EIDETIC_*`). TOML/CLI
/// override layers will be added when there's a concrete need for them
/// (per ADR-pending; see IMPLEMENTATION_KICKOFF).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub paths: Paths,
    /// Path to the embedded SQLite database file (ADR-0005). Created on first
    /// connect if missing; no server process is involved.
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    pub library_dir: PathBuf,
    pub models_cache: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let cache = default_cache_dir();
        Self {
            paths: Paths {
                library_dir: cache.join("library"),
                models_cache: cache.join("models"),
            },
            database_path: cache.join("eidetic.db"),
        }
    }
}

impl Config {
    /// Load configuration from defaults, with environment variables
    /// overriding individual fields.
    ///
    /// Recognized env vars:
    /// - `EIDETIC_DATABASE_PATH`
    /// - `EIDETIC_LIBRARY_DIR`
    /// - `EIDETIC_MODELS_CACHE`
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(v) = std::env::var("EIDETIC_DATABASE_PATH") {
            config.database_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("EIDETIC_LIBRARY_DIR") {
            config.paths.library_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("EIDETIC_MODELS_CACHE") {
            config.paths.models_cache = PathBuf::from(v);
        }

        config
    }
}

fn default_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").expect(
        "HOME must be set; required for default cache dir \
         (override with EIDETIC_LIBRARY_DIR / EIDETIC_MODELS_CACHE)",
    );
    PathBuf::from(home).join(".cache").join("eidetic")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_are_under_cache_dir() {
        let config = Config::default();
        assert!(config.paths.library_dir.ends_with("library"));
        assert!(config.paths.models_cache.ends_with("models"));
    }
}
