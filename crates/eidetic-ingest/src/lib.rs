//! File ingestion for Eidetic.

pub mod error;
pub mod hasher;
pub mod import;
pub mod meta;
pub mod repo;
pub mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use repo::{AssetIndex, NewAsset};
pub use store::store_file;
