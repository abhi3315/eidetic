//! Image embedder trait + mock implementation.
//!
//! The real implementation (`SiglipEmbedder` via `ort`) lands in a
//! subsequent commit. This file establishes the contract every
//! embedder honors so the rest of the codebase (search, ingest)
//! can be built and tested against the trait, not the concrete
//! impl.

use crate::Result;
use std::path::Path;

/// A vector embedding produced by an [`Embedder`].
///
/// Newtype wrapper around `Vec<f32>` so callers can't accidentally
/// pass raw floats where an embedding is expected, and so we can
/// add metadata later (model version, normalization flag, etc.)
/// without breaking callers.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(Vec<f32>);

impl Embedding {
    pub fn new(values: Vec<f32>) -> Self {
        Self(values)
    }

    pub fn dim(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<f32> {
        self.0
    }
}

/// Produces vector embeddings from images.
///
/// Synchronous on purpose. Inference is CPU/GPU-bound, not I/O-bound,
/// so there's nothing for an async runtime to await on. Callers that
/// want to keep their tokio runtime threads free should wrap calls
/// in `tokio::task::spawn_blocking`.
pub trait Embedder: Send + Sync {
    /// Output dimension. Stable for the lifetime of an embedder
    /// instance — the model is fixed at construction time.
    fn dim(&self) -> usize;

    /// Compute an embedding for the image at `path`.
    fn embed(&self, path: &Path) -> Result<Embedding>;
}

/// Deterministic mock embedder for tests.
///
/// Returns an embedding of the configured dimension where every value
/// is `0.0`. Useful when a test needs an [`Embedder`] but doesn't
/// exercise the actual values.
pub struct MockEmbedder {
    dim: usize,
}

impl MockEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Embedder for MockEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, _path: &Path) -> Result<Embedding> {
        Ok(Embedding(vec![0.0; self.dim]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn mock_returns_correct_dim() {
        let embedder = MockEmbedder::new(768);
        assert_eq!(embedder.dim(), 768);
    }

    #[test]
    fn mock_returns_correct_size_vector() {
        let embedder = MockEmbedder::new(768);
        let path = PathBuf::from("/nonexistent");
        let emb = embedder.embed(&path).unwrap();
        assert_eq!(emb.dim(), 768);
        assert_eq!(emb.as_slice(), &[0.0; 768]);
    }

    #[test]
    fn embedding_round_trip() {
        let values = vec![1.0, 2.0, 3.0];
        let emb = Embedding::new(values.clone());
        assert_eq!(emb.dim(), 3);
        assert_eq!(emb.into_vec(), values);
    }
}
