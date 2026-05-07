use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to store {path}: {source}")]
    StoreIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("asset index: {0}")]
    Index(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

pub type Result<T> = std::result::Result<T, Error>;
