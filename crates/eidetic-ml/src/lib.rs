//! ML inference for Eidetic.

mod error;
mod siglip;

pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
