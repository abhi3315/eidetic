use crate::Result;
use std::path::Path;

/// A vector embedding produced by an [`Embedder`].
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

/// Produces vector embeddings from images and text.
///
/// Synchronous on purpose — inference is CPU/GPU-bound, not I/O-bound.
/// Callers that want to keep their tokio runtime threads free should wrap
/// calls in `tokio::task::spawn_blocking`.
pub trait Embedder: Send + Sync {
    /// Output dimension. Stable for the lifetime of an embedder instance.
    fn dim(&self) -> usize;

    /// Compute an embedding for the image at `path`.
    fn embed(&self, path: &Path) -> Result<Embedding>;

    /// Compute an embedding for a text string.
    fn embed_text(&self, text: &str) -> Result<Embedding>;
}

/// Deterministic mock embedder for tests. Returns zeros for all inputs.
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

    fn embed_text(&self, _text: &str) -> Result<Embedding> {
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

    #[test]
    fn mock_embed_text_returns_correct_dim() {
        let embedder = MockEmbedder::new(768);
        let emb = embedder.embed_text("dog on beach").unwrap();
        assert_eq!(emb.dim(), 768);
    }

    #[test]
    fn mock_embed_text_returns_zeros() {
        let embedder = MockEmbedder::new(4);
        let emb = embedder.embed_text("test").unwrap();
        assert_eq!(emb.as_slice(), &[0.0f32; 4]);
    }
}
