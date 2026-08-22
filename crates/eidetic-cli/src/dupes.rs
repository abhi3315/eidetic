//! Near-duplicate detection over the embeddings the library already stores
//! (goals-v0.5.md #5, built ahead of its trigger because it turned out to be
//! nearly free).
//!
//! Exact duplicates never reach the database — import dedups by content hash
//! — so everything reported here is a *perceptual* duplicate: the same photo
//! re-exported at a different quality, or the same clip re-encoded/remuxed.
//! Detection deviates from the goals file's ffmpeg-signature sketch on
//! purpose: SigLIP embeddings are already sitting in SQLite, a re-encode has
//! near-identical vectors, and reusing them covers photos as well as videos
//! with zero new dependencies.
//!
//! Reporting only. Nothing is deleted; the user decides what a duplicate is.

use eidetic_core::AssetId;
use std::collections::HashMap;
use std::path::PathBuf;

/// Cosine similarity above which two *photos* are called duplicates. SigLIP
/// puts a re-export of the same pixels at ~0.99+; distinct shots of the same
/// scene land well below.
pub const PHOTO_THRESHOLD: f32 = 0.985;
/// Threshold for videos, applied to the mean best-frame match. Slightly
/// looser: re-encoding shifts frame embeddings a little more than photo
/// re-export does.
pub const VIDEO_THRESHOLD: f32 = 0.97;

pub struct DupePair {
    pub a: (AssetId, PathBuf),
    pub b: (AssetId, PathBuf),
    pub similarity: f32,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// All photo pairs above `threshold`. Quadratic scan — fine at personal
/// scale (10k photos ≈ seconds); an ANN prefilter is the escalation if a
/// library ever makes this slow.
pub fn photo_pairs(
    photos: &[(AssetId, PathBuf, Vec<f32>)],
    threshold: f32,
) -> Vec<DupePair> {
    let mut out = Vec::new();
    for i in 0..photos.len() {
        for j in i + 1..photos.len() {
            let (ia, pa, va) = &photos[i];
            let (ib, pb, vb) = &photos[j];
            if va.len() != vb.len() {
                continue; // mid-model-migration rows
            }
            let sim = dot(va, vb);
            if sim >= threshold {
                out.push(DupePair {
                    a: (*ia, pa.clone()),
                    b: (*ib, pb.clone()),
                    similarity: sim,
                });
            }
        }
    }
    out
}

/// Similarity of two videos: for each frame of the smaller set, its best
/// match in the other, averaged. A truncated copy therefore still scores
/// high — every frame it *has* matches — which is exactly the "partial
/// re-upload" case worth flagging.
fn video_similarity(a: &[Vec<f32>], b: &[Vec<f32>]) -> f32 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if small.is_empty() {
        return 0.0;
    }
    let sum: f32 = small
        .iter()
        .map(|fa| {
            large
                .iter()
                .filter(|fb| fb.len() == fa.len())
                .map(|fb| dot(fa, fb))
                .fold(f32::MIN, f32::max)
        })
        .filter(|s| s.is_finite())
        .sum();
    sum / small.len() as f32
}

/// All video pairs above `threshold`.
pub fn video_pairs(
    videos: &[(AssetId, PathBuf, Vec<Vec<f32>>)],
    threshold: f32,
) -> Vec<DupePair> {
    let mut out = Vec::new();
    for i in 0..videos.len() {
        for j in i + 1..videos.len() {
            let sim = video_similarity(&videos[i].2, &videos[j].2);
            if sim >= threshold {
                out.push(DupePair {
                    a: (videos[i].0, videos[i].1.clone()),
                    b: (videos[j].0, videos[j].1.clone()),
                    similarity: sim,
                });
            }
        }
    }
    out
}

/// Union pairs into groups for display: A≈B and B≈C print as one group.
pub fn group(pairs: &[DupePair]) -> Vec<Vec<(AssetId, PathBuf)>> {
    let mut parent: HashMap<AssetId, AssetId> = HashMap::new();
    fn find(parent: &mut HashMap<AssetId, AssetId>, x: AssetId) -> AssetId {
        let p = *parent.entry(x).or_insert(x);
        if p == x {
            x
        } else {
            let root = find(parent, p);
            parent.insert(x, root);
            root
        }
    }
    let mut paths: HashMap<AssetId, PathBuf> = HashMap::new();
    for pair in pairs {
        paths.insert(pair.a.0, pair.a.1.clone());
        paths.insert(pair.b.0, pair.b.1.clone());
        let (ra, rb) = (find(&mut parent, pair.a.0), find(&mut parent, pair.b.0));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }
    let mut groups: HashMap<AssetId, Vec<(AssetId, PathBuf)>> = HashMap::new();
    let members: Vec<AssetId> = paths.keys().copied().collect();
    for id in members {
        let root = find(&mut parent, id);
        groups.entry(root).or_default().push((id, paths[&id].clone()));
    }
    let mut out: Vec<Vec<(AssetId, PathBuf)>> = groups.into_values().collect();
    for g in &mut out {
        g.sort_by(|a, b| a.1.cmp(&b.1));
    }
    out.sort_by(|a, b| a[0].1.cmp(&b[0].1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    fn photo(name: &str, v: Vec<f32>) -> (AssetId, PathBuf, Vec<f32>) {
        (AssetId::new(), PathBuf::from(name), unit(v))
    }

    #[test]
    fn photo_pairs_finds_only_near_identical() {
        let photos = vec![
            photo("a.jpg", vec![1.0, 0.01, 0.0]),
            photo("a_copy.jpg", vec![1.0, 0.012, 0.0]),
            photo("other.jpg", vec![0.2, 1.0, 0.3]),
        ];
        let pairs = photo_pairs(&photos, PHOTO_THRESHOLD);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].similarity > 0.999);
    }

    #[test]
    fn truncated_video_still_matches_its_source() {
        // Full video: 5 frames. Truncated copy: its first 2 frames.
        let f = |seed: f32| unit(vec![seed, seed * 0.5, 1.0]);
        let full = vec![f(0.1), f(0.5), f(0.9), f(1.3), f(1.7)];
        let cut = vec![f(0.1), f(0.5)];
        let sim = video_similarity(&cut, &full);
        assert!(sim > 0.999, "every frame of the cut matches: {sim}");

        let unrelated = vec![unit(vec![1.0, -1.0, 0.2]); 3];
        assert!(video_similarity(&unrelated, &full) < VIDEO_THRESHOLD);
    }

    #[test]
    fn groups_are_transitive() {
        let (a, b, c) = (AssetId::new(), AssetId::new(), AssetId::new());
        let p = |id, n: &str| (id, PathBuf::from(n));
        let pairs = vec![
            DupePair { a: p(a, "a"), b: p(b, "b"), similarity: 0.99 },
            DupePair { a: p(b, "b"), b: p(c, "c"), similarity: 0.99 },
        ];
        let groups = group(&pairs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }
}
