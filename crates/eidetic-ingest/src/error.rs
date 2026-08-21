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
    Db(#[from] eidetic_db::Error),
    #[error("dng preview extraction failed: {0}")]
    Dng(#[source] eidetic_core::dng::Error),
    #[error("ffprobe failed for {path}: {detail}")]
    VideoProbe { path: PathBuf, detail: String },
    #[error("frame extraction failed for {path} at {ts_secs:.1}s: {detail}")]
    FrameExtract {
        path: PathBuf,
        ts_secs: f64,
        detail: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
