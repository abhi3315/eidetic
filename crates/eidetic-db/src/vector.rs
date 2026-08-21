//! Vector search strategy (ADR-0005).
//!
//! Embeddings are stored as raw little-endian f32 blobs in the `embeddings`
//! table and ranked in-process. The strategy sits behind [`VectorIndex`] so it
//! can be swapped for an approximate index (usearch HNSW, or sqlite-vec's ANN
//! once it stabilises) without touching any caller.
//!
//! The default [`BruteForce`] is *exact*: it scores every stored vector, so
//! recall is 100%. At personal-library scale that is both more accurate than
//! an ANN index and fast enough — a few hundred thousand 1152-dim vectors is
//! tens of milliseconds of SIMD-friendly dot products.

/// Ranking strategy over stored embeddings.
///
/// Results are indices into `stored` rather than asset ids: since video
/// support (ADR-0011) one asset can own several frame vectors, and the
/// caller needs to know *which row* matched (it carries the timestamp). An
/// approximate implementation would keep its own persistent index and use
/// `stored` only for dimensions; the exact implementation reads it directly.
pub trait VectorIndex: Send + Sync {
    /// Return up to `limit` `(index_into_stored, score)` pairs, highest
    /// score first.
    ///
    /// `query` and every stored vector are expected to be L2-normalised, so
    /// the score is a cosine similarity in `[-1.0, 1.0]`.
    fn top_k(&self, query: &[f32], stored: &[Vec<f32>], limit: usize) -> Vec<(usize, f32)>;
}

/// Exact nearest-neighbour search by full scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct BruteForce;

impl VectorIndex for BruteForce {
    fn top_k(&self, query: &[f32], stored: &[Vec<f32>], limit: usize) -> Vec<(usize, f32)> {
        if limit == 0 {
            return Vec::new();
        }

        let mut scored: Vec<(usize, f32)> = stored
            .iter()
            .enumerate()
            // Skip rows whose dimension doesn't match the query. That happens
            // only mid-migration between embedding models (ADR-0007); ranking
            // them against a different-width query would be meaningless.
            .filter(|(_, v)| v.len() == query.len())
            .map(|(i, v)| (i, dot(query, v)))
            .collect();

        // Descending by score. `total_cmp` avoids the NaN panic that
        // `partial_cmp().unwrap()` would risk on a corrupt vector.
        scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit);
        scored
    }
}

/// Cosine similarity for L2-normalised inputs — i.e. a plain dot product.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Encode an embedding for storage as a little-endian f32 blob.
pub fn encode(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Decode a stored blob back into an embedding.
///
/// Returns `None` if the blob length isn't a whole number of f32s, which can
/// only happen if something outside this crate wrote the column.
pub fn decode(blob: &[u8]) -> Option<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trips() {
        let v = vec![0.0f32, 1.0, -0.5, 0.125];
        let decoded = decode(&encode(&v)).expect("well-formed blob");
        assert_eq!(v, decoded);
    }

    #[test]
    fn decode_rejects_ragged_blob() {
        assert!(decode(&[0u8, 1, 2]).is_none());
    }

    #[test]
    fn top_k_ranks_by_similarity_and_truncates() {
        // A unit vector at 45 degrees: equidistant from both axes.
        let diag = std::f32::consts::FRAC_1_SQRT_2;
        let stored = vec![
            vec![0.0, 1.0],     // far
            vec![1.0, 0.0],     // exact
            vec![diag, diag],   // middle
        ];

        let hits = BruteForce.top_k(&[1.0, 0.0], &stored, 2);

        assert_eq!(hits.len(), 2, "limit must truncate");
        assert_eq!(hits[0].0, 1, "index of the exact match");
        assert_eq!(hits[1].0, 2, "index of the diagonal");
        assert!((hits[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn top_k_skips_mismatched_dimensions() {
        let stored = vec![vec![1.0, 0.0, 0.0], vec![1.0, 0.0]];

        let hits = BruteForce.top_k(&[1.0, 0.0], &stored, 10);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 1, "only the dimension-matched row survives");
    }

    #[test]
    fn top_k_zero_limit_is_empty() {
        let stored = vec![vec![1.0, 0.0]];
        assert!(BruteForce.top_k(&[1.0, 0.0], &stored, 0).is_empty());
    }
}
