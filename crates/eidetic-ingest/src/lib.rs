//! File ingestion for Eidetic.

pub mod error;
pub mod hasher;
pub mod repo;
pub mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use repo::{AssetIndex, NewAsset};
pub use store::store_file;
