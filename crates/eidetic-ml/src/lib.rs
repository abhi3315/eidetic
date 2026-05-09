//! ML inference for Eidetic.

mod embedder;
mod error;
mod siglip;

pub use embedder::{Embedder, Embedding};
pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
