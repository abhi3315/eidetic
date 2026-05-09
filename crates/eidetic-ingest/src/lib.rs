//! File ingestion for Eidetic.

mod error;
mod hasher;
mod import;
mod meta;
mod repo;
mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use repo::{AssetIndex, InsertOutcome, NewAsset};
pub use store::{commit_staged, stage_file};
