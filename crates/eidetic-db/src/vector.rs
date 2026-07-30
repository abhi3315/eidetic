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

use eidetic_core::AssetId;

/// Ranking strategy over stored embeddings.
///
/// An approximate implementation would keep its own persistent index and use
/// `stored` only to resolve ids; the exact implementation below reads it
/// directly.
pub trait VectorIndex: Send + Sync {
    /// Return up to `limit` `(asset, score)` pairs, highest score first.
    ///
    /// `query` and every stored vector are expected to be L2-normalised, so
    /// the score is a cosine similarity in `[-1.0, 1.0]`.
    fn top_k(
        &self,
        query: &[f32],
        stored: &[(AssetId, Vec<f32>)],
        limit: usize,
    ) -> Vec<(AssetId, f32)>;
}

/// Exact nearest-neighbour search by full scan.
#[derive(Debug, Clone, Copy, Default)]
pub struct BruteForce;

impl VectorIndex for BruteForce {
    fn top_k(
        &self,
        query: &[f32],
        stored: &[(AssetId, Vec<f32>)],
        limit: usize,
    ) -> Vec<(AssetId, f32)> {
        if limit == 0 {
            return Vec::new();
        }

        let mut scored: Vec<(AssetId, f32)> = stored
            .iter()
            // Skip rows whose dimension doesn't match the query. That happens
            // only mid-migration between embedding models (ADR-0007); ranking
            // them against a different-width query would be meaningless.
            .filter(|(_, v)| v.len() == query.len())
            .map(|(id, v)| (*id, dot(query, v)))
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
pub(crate) fn encode(vector: &[f32]) -> Vec<u8> {
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
pub(crate) fn decode(blob: &[u8]) -> Option<Vec<f32>> {
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

    fn id() -> AssetId {
        AssetId::new()
    }

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
        let (near, mid, far) = (id(), id(), id());
        let stored = vec![
            (far, vec![0.0, 1.0]),
            (near, vec![1.0, 0.0]),
            (mid, vec![diag, diag]),
        ];

        let hits = BruteForce.top_k(&[1.0, 0.0], &stored, 2);

        assert_eq!(hits.len(), 2, "limit must truncate");
        assert_eq!(hits[0].0, near);
        assert_eq!(hits[1].0, mid);
        assert!((hits[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn top_k_skips_mismatched_dimensions() {
        let (ok, wrong) = (id(), id());
        let stored = vec![(wrong, vec![1.0, 0.0, 0.0]), (ok, vec![1.0, 0.0])];

        let hits = BruteForce.top_k(&[1.0, 0.0], &stored, 10);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, ok);
    }

    #[test]
    fn top_k_zero_limit_is_empty() {
        let stored = vec![(id(), vec![1.0, 0.0])];
        assert!(BruteForce.top_k(&[1.0, 0.0], &stored, 0).is_empty());
    }
}
