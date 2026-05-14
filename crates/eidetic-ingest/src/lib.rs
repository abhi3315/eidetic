//! File ingestion for Eidetic.

mod error;
mod hasher;
mod import;
mod meta;
mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use store::{commit_staged, stage_file};
