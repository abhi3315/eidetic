//! File ingestion for Eidetic.
//!
//! Day-one scope: a streaming SHA-256 hasher for content-addressable
//! deduplication. The watcher, content-addressable storage layer,
//! and thumbnail generator land in subsequent commits as their callers
//! appear.

pub mod error;
pub mod hasher;

pub use error::{Error, Result};
pub use hasher::hash_file;
