//! Index trait: storage-layer abstraction for the asset catalog.
//!
//! Lives here (not in `eidetic-ingest`) so any DB / HTTP / test
//! implementation can depend only on `eidetic-core`.

use crate::AssetId;
use chrono::{DateTime, Utc};
use std::path::PathBuf;

/// Boxed error returned by [`AssetIndex`] implementations.
///
/// Each impl chooses its own concrete error type (e.g. `sqlx::Error`)
/// and boxes it; callers that need to inspect the cause downcast.
pub type IndexError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type IndexResult<T> = std::result::Result<T, IndexError>;

#[derive(Debug)]
pub enum InsertOutcome {
    Inserted(AssetId),
    Existing(AssetId),
}

#[derive(Clone, Debug)]
pub struct NewAsset {
    pub hash: String,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: u64,
    pub mime_type: Option<String>,
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

#[allow(async_fn_in_trait)]
pub trait AssetIndex {
    async fn find_by_hash(&self, hash: &str) -> IndexResult<Option<AssetId>>;
    async fn insert_asset(&self, asset: NewAsset) -> IndexResult<InsertOutcome>;
}
