use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;
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
    #[error(
        "{program} on {} was killed after {elapsed:.1?} (deadline {deadline:?}){}",
        .subject.display(),
        tail(.stderr_tail)
    )]
    SubprocessTimeout {
        program: String,
        subject: PathBuf,
        elapsed: Duration,
        deadline: Duration,
        stderr_tail: String,
    },
    #[error(
        "{program} failed on {} ({status}) after {elapsed:.1?}{}",
        .subject.display(),
        tail(.stderr_tail)
    )]
    SubprocessFailed {
        program: String,
        subject: PathBuf,
        status: ExitStatus,
        elapsed: Duration,
        stderr_tail: String,
    },
}

/// `": <stderr tail>"`, or nothing when the subprocess printed nothing.
fn tail(stderr_tail: &str) -> String {
    if stderr_tail.is_empty() {
        String::new()
    } else {
        format!(": {stderr_tail}")
    }
}

pub type Result<T> = std::result::Result<T, Error>;
