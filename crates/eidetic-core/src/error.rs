use thiserror::Error;

/// Core error type for Eidetic.
///
/// Other library crates define their own error enums and convert to/from
/// this one via `#[from]`. Binary crates use `anyhow` and let errors
/// propagate as `anyhow::Error`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, Error>;
