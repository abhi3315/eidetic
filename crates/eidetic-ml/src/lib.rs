//! ML inference for Eidetic.

pub mod embedder;
pub mod error;
pub mod siglip;

pub use embedder::{Embedder, Embedding};
pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
