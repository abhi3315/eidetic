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
    #[error("image decode failed for {path}: {source}")]
    ImageDecode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("jpeg encode failed for {path}: {source}")]
    JpegEncode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("database error: {0}")]
    Db(#[source] eidetic_db::Error),
    #[error("dng preview extraction failed: {0}")]
    Dng(#[source] eidetic_core::dng::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
