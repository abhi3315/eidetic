//! ML inference for Eidetic.
//!
//! Per ADR-0003 the runtime is `ort` (ONNX Runtime). Per ADR-0004 the
//! image embedder is SigLIP 2 base producing 768-dim vectors.
//!
//! Day-one scope is the [`Embedder`] trait and a [`MockEmbedder`] for
//! testing. The real `SiglipEmbedder` (model load + preprocessing +
//! inference) lands in subsequent commits along with the `ort` and
//! `hf-hub` dependencies.

pub mod embedder;
pub mod error;

pub use embedder::{Embedder, Embedding, MockEmbedder};
pub use error::{Error, Result};
