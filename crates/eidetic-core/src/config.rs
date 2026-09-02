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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "cannot locate a default data directory: none of {vars} are set. \
         Set one, or override every path explicitly with EIDETIC_DATABASE_PATH, \
         EIDETIC_LIBRARY_DIR and EIDETIC_MODELS_CACHE."
    )]
    NoHome { vars: &'static str },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Config {
    /// Load configuration from environment variables, falling back to a
    /// platform default directory for anything not set.
    ///
    /// Each field resolves independently, so a run with all three variables
    /// set never consults the platform default at all — which matters
    /// because that lookup is the one part of startup that can fail, and on
    /// Windows it fails for a plain `cmd.exe` where Unix `HOME` is absent.
    ///
    /// Recognized env vars:
    /// - `EIDETIC_DATABASE_PATH`
    /// - `EIDETIC_LIBRARY_DIR`
    /// - `EIDETIC_MODELS_CACHE`
    pub fn from_env() -> Result<Self> {
        resolve(
            std::env::var("EIDETIC_DATABASE_PATH")
                .ok()
                .map(PathBuf::from),
            std::env::var("EIDETIC_LIBRARY_DIR").ok().map(PathBuf::from),
            std::env::var("EIDETIC_MODELS_CACHE")
                .ok()
                .map(PathBuf::from),
            default_cache_dir,
        )
    }
}

/// Combine explicit overrides with a lazily-resolved default directory.
///
/// Split from [`Config::from_env`] so the "all three set means `cache` is
/// never called" property is testable without mutating process-global
/// environment variables from a parallel test.
fn resolve(
    db: Option<PathBuf>,
    library: Option<PathBuf>,
    models: Option<PathBuf>,
    cache: impl FnOnce() -> Result<PathBuf>,
) -> Result<Config> {
    let base = if db.is_none() || library.is_none() || models.is_none() {
        Some(cache()?)
    } else {
        None
    };
    let fallback = |sub: &str| {
        base.as_ref()
            .expect("base is resolved whenever a field is unset")
            .join(sub)
    };

    Ok(Config {
        paths: Paths {
            library_dir: library.unwrap_or_else(|| fallback("library")),
            models_cache: models.unwrap_or_else(|| fallback("models")),
        },
        database_path: db.unwrap_or_else(|| fallback("eidetic.db")),
    })
}

/// The per-platform base directory for an un-overridden install.
///
/// Unix keeps `~/.cache/eidetic` verbatim — changing it would orphan every
/// existing library. Windows has no `HOME` to speak of, so it uses the
/// documented local app-data location instead.
fn default_cache_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        const VARS: &str = "LOCALAPPDATA, USERPROFILE";
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join("eidetic"));
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return Ok(PathBuf::from(profile)
                .join("AppData")
                .join("Local")
                .join("eidetic"));
        }
        Err(Error::NoHome { vars: VARS })
    }

    #[cfg(not(windows))]
    {
        const VARS: &str = "HOME";
        match std::env::var("HOME") {
            Ok(home) => Ok(PathBuf::from(home).join(".cache").join("eidetic")),
            Err(_) => Err(Error::NoHome { vars: VARS }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_paths_are_under_cache_dir() {
        let cache = default_cache_dir().expect("test host has a home directory");
        assert!(cache.join("library").ends_with("library"));
        assert!(cache.join("models").ends_with("models"));
    }

    #[test]
    fn fully_overridden_config_never_resolves_a_default_directory() {
        // The property that matters on Windows: with every path given
        // explicitly, the one fallible part of startup must not run. The old
        // code built defaults first and panicked before reading a single
        // override, so every documented escape hatch was unreachable.
        let config = resolve(
            Some(PathBuf::from("/db/eidetic.db")),
            Some(PathBuf::from("/lib")),
            Some(PathBuf::from("/models")),
            || panic!("default directory resolved despite full overrides"),
        )
        .expect("fully overridden config resolves");

        assert_eq!(config.database_path, PathBuf::from("/db/eidetic.db"));
        assert_eq!(config.paths.library_dir, PathBuf::from("/lib"));
        assert_eq!(config.paths.models_cache, PathBuf::from("/models"));
    }

    #[test]
    fn a_partial_override_keeps_defaults_for_the_rest() {
        let config = resolve(None, Some(PathBuf::from("/lib")), None, || {
            Ok(PathBuf::from("/base"))
        })
        .expect("partial config resolves");

        assert_eq!(config.paths.library_dir, PathBuf::from("/lib"));
        assert_eq!(config.paths.models_cache, PathBuf::from("/base/models"));
        assert_eq!(config.database_path, PathBuf::from("/base/eidetic.db"));
    }

    #[test]
    fn an_unresolvable_home_is_an_error_not_a_panic() {
        let err = resolve(None, None, None, || Err(Error::NoHome { vars: "HOME" }));
        assert!(err.is_err(), "missing home surfaces as Err");
    }
}
