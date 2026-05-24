//! Zero dependencies on `tokio`, `sqlx`, or `ort`. Every other crate builds
//! on top of this one, so heavy deps here pollute the graph for everyone.

pub mod config;
pub mod dng;
pub mod exif;
pub mod geocoder;
pub mod ids;

pub use config::{Config, Paths};
pub use ids::{AssetId, Sha256};
