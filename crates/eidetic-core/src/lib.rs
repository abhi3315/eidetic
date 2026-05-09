//! Shared types and configuration for Eidetic.
//!
//! This crate has zero dependencies on `tokio`, `sqlx`, or `ort` — it's the
//! lightweight foundation that every other crate builds on. Adding heavy
//! dependencies here pollutes the dependency graph for everyone.

pub mod config;
pub mod ids;
pub mod index;

pub use config::{Config, Paths};
pub use ids::{AssetId, Sha256};
pub use index::{AssetIndex, IndexError, IndexResult, InsertOutcome, NewAsset};
