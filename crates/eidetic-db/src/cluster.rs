//! Face clustering (ADR-0010).
//!
//! Two paths, deliberately different in cost:
//!
//! * **Incremental** — on import, each new face is compared against the stored
//!   exemplars of people who already exist. That is a handful of dot products,
//!   so it runs inline with no graph and no full scan.
//! * **Periodic** — Chinese Whispers over the *unassigned* pool only, to
//!   propose new people. This is the expensive pass and runs on demand.
//!
//! Nothing here touches `sqlx`: the caller loads faces, calls these functions,
//! and writes the results back. That keeps the whole algorithm unit-testable
//! against hand-built vectors instead of a database.

use eidetic_core::{FaceId, PersonId};
use std::collections::{HashMap, HashSet};

/// Tunable thresholds. Defaults come from ADR-0010.
#[derive(Debug, Clone, Copy)]
pub struct ClusterParams {
    /// Cosine *distance* at which a face joins an existing person.
    pub attach_distance: f32,
    /// Cosine *distance* for edges in the new-person graph. Tighter than
    /// `attach_distance`: inventing a person should need more confidence than
    /// extending one.
    pub cluster_distance: f32,
    /// A new person needs at least this many mutually-close faces, which stops
    /// every passing stranger from becoming a named candidate.
    pub min_faces: usize,
    /// Detector score below which a face may still be *assigned* but must
    /// never *seed* a cluster. Low-quality embeddings are what bridge two
    /// distinct people together.
    pub min_seed_score: f32,
    /// Exemplars kept per person. More than one because a single averaged
    /// centroid drifts badly as someone ages.
    pub max_exemplars: usize,
}

impl Default for ClusterParams {
    fn default() -> Self {
        Self {
            attach_distance: 0.5,
            cluster_distance: 0.4,
            min_faces: 3,
            min_seed_score: 0.8,
            max_exemplars: 5,
        }
    }
}

/// The user's corrections, in the form clustering needs to honour them.
#[derive(Debug, Default, Clone)]
pub struct Constraints {
    /// `(face, person)` pairs the user has explicitly rejected.
    rejected: HashSet<(FaceId, PersonId)>,
    /// Face pairs the user says are the same person.
    must_link: Vec<(FaceId, FaceId)>,
    /// Face pairs the user says are different people. Stored unordered.
    must_not_link: HashSet<(FaceId, FaceId)>,
}

impl Constraints {
    pub fn new(
        rejected: Vec<(FaceId, PersonId)>,
        must_link: Vec<(FaceId, FaceId)>,
        must_not_link: Vec<(FaceId, FaceId)>,
    ) -> Self {
        Self {
            rejected: rejected.into_iter().collect(),
            must_link,
            must_not_link: must_not_link.into_iter().map(order).collect(),
        }
    }

    fn forbids(&self, a: FaceId, b: FaceId) -> bool {
        self.must_not_link.contains(&order((a, b)))
    }

    fn rejects(&self, face: FaceId, person: PersonId) -> bool {
        self.rejected.contains(&(face, person))
    }

    /// Every face the user says is a different person from `face`. A linear
    /// scan: links are made by hand and stay few.
    fn forbidden_partners(&self, face: FaceId) -> impl Iterator<Item = FaceId> + '_ {
        self.must_not_link.iter().filter_map(move |&(a, b)| {
            if a == face {
                Some(b)
            } else if b == face {
                Some(a)
            } else {
                None
            }
        })
    }
}

/// Canonical ordering so a pair is the same key either way round.
fn order((a, b): (FaceId, FaceId)) -> (FaceId, FaceId) {
    if a.as_uuid() <= b.as_uuid() {
        (a, b)
    } else {
        (b, a)
    }
}

/// A person's stored exemplars, as used by the incremental path.
#[derive(Debug, Clone)]
pub struct PersonExemplars {
    pub person: PersonId,
    /// Representative embeddings. Several, not an average.
    pub exemplars: Vec<Vec<f32>>,
}

/// Minimum a face needs to expose to be clustered.
pub trait Face {
    fn face_id(&self) -> FaceId;
    fn embedding(&self) -> &[f32];
    fn score(&self) -> f32;
}

impl Face for crate::FaceEmbedding {
    fn face_id(&self) -> FaceId {
        self.id
    }
    fn embedding(&self) -> &[f32] {
        &self.embedding
    }
    fn score(&self) -> f32 {
        self.score
    }
}

/// So a `&[&Face]` works too — callers that group faces by person end up with
/// slices of references and should not have to clone to cluster them.
impl<T: Face> Face for &T {
    fn face_id(&self) -> FaceId {
        (*self).face_id()
    }
    fn embedding(&self) -> &[f32] {
        (*self).embedding()
    }
    fn score(&self) -> f32 {
        (*self).score()
    }
}

/// Cosine distance for L2-normalised vectors: `1 - dot`.
///
/// Returns `f32::INFINITY` for mismatched widths so a dimension change between
/// embedding models can never look like a close match.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::INFINITY;
    }
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

/// Try to attach one face to an existing person.
///
/// Returns the closest person within `attach_distance` that the user has not
/// rejected for this face and that holds no face it must not be linked with,
/// or `None` to leave the face for the periodic pass. `members` maps every
/// attributed face to its person, exemplar or not: exemplars carry no face
/// ids, so membership is the only way to see who belongs to whom. A person
/// skipped for either reason does not stop the face joining the next-nearest
/// allowed one.
pub fn assign_to_existing<F: Face>(
    face: &F,
    people: &[PersonExemplars],
    members: &HashMap<FaceId, PersonId>,
    constraints: &Constraints,
    params: &ClusterParams,
) -> Option<PersonId> {
    let blocked: HashSet<PersonId> = constraints
        .forbidden_partners(face.face_id())
        .filter_map(|partner| members.get(&partner).copied())
        .collect();
    let mut best: Option<(PersonId, f32)> = None;

    for person in people {
        if constraints.rejects(face.face_id(), person.person) || blocked.contains(&person.person) {
            continue;
        }
        // Distance to a person is distance to their *nearest* exemplar, not to
        // an average: someone photographed across fifteen years is better
        // modelled by several points than by their midpoint.
        let Some(d) = person
            .exemplars
            .iter()
            .map(|e| cosine_distance(face.embedding(), e))
            .min_by(f32::total_cmp)
        else {
            continue;
        };

        if d <= params.attach_distance && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((person.person, d));
        }
    }

    best.map(|(p, _)| p)
}

/// Run [`assign_to_existing`] over a batch of unassigned faces, in order.
///
/// Returns one entry per input face. Each attachment counts as a member for
/// the faces after it, so two faces the user split can never both join the
/// same person in one run; exemplars stay fixed for the whole batch. The
/// result therefore depends on input order, and `unassigned` must hold only
/// faces with no person yet.
pub fn assign_all_to_existing<F: Face>(
    unassigned: &[F],
    people: &[PersonExemplars],
    members: impl IntoIterator<Item = (FaceId, PersonId)>,
    constraints: &Constraints,
    params: &ClusterParams,
) -> Vec<Option<PersonId>> {
    let mut members: HashMap<FaceId, PersonId> = members.into_iter().collect();
    unassigned
        .iter()
        .map(|face| {
            let pick = assign_to_existing(face, people, &members, constraints, params);
            if let Some(person) = pick {
                members.insert(face.face_id(), person);
            }
            pick
        })
        .collect()
}

/// A proposed new person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedPerson {
    pub faces: Vec<FaceId>,
}

/// Group unassigned faces into candidate people.
///
/// Chinese Whispers over a similarity graph, then must-link merging, then the
/// `min_faces` filter. Components smaller than `min_faces` are dropped rather
/// than returned as one-face people — a library is full of strangers in
/// backgrounds, and each one becoming a named candidate would be noise.
pub fn propose_people<F: Face>(
    faces: &[F],
    constraints: &Constraints,
    params: &ClusterParams,
) -> Vec<ProposedPerson> {
    if faces.is_empty() {
        return Vec::new();
    }

    let labels = chinese_whispers(faces, constraints, params);

    // Group by final label.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &label) in labels.iter().enumerate() {
        groups.entry(label).or_default().push(i);
    }

    // Apply must-link by unioning the groups that contain each linked pair.
    let index: HashMap<FaceId, usize> = faces
        .iter()
        .enumerate()
        .map(|(i, f)| (f.face_id(), i))
        .collect();
    let mut uf = UnionFind::new(faces.len());
    for group in groups.values() {
        for pair in group.windows(2) {
            uf.union(pair[0], pair[1]);
        }
    }
    // A must-link union is refused when it would put any must-not-linked pair
    // into one component — directly (the same pair carries both links) or
    // through a chain of must-links. Must-not wins: proposing two faces the
    // user explicitly split is worse than ignoring one merge hint, and the
    // skipped link is reported so the conflict is visible instead of silent.
    // Unions apply in stored link order, so the outcome is deterministic.
    for &(a, b) in &constraints.must_link {
        let (Some(&ia), Some(&ib)) = (index.get(&a), index.get(&b)) else {
            continue;
        };
        let (ra, rb) = (uf.find(ia), uf.find(ib));
        if ra == rb {
            continue;
        }
        let violates = constraints.must_not_link.iter().any(|&(x, y)| {
            let (Some(&ix), Some(&iy)) = (index.get(&x), index.get(&y)) else {
                return false;
            };
            let (rx, ry) = (uf.find(ix), uf.find(iy));
            (rx == ra && ry == rb) || (rx == rb && ry == ra)
        });
        if violates {
            tracing::warn!(
                face_a = %a,
                face_b = %b,
                "must-link skipped: joining these would group a pair marked \
                 as different people (must-not-link wins)"
            );
            continue;
        }
        uf.union(ia, ib);
    }

    let mut merged: HashMap<usize, Vec<FaceId>> = HashMap::new();
    for (i, face) in faces.iter().enumerate() {
        merged.entry(uf.find(i)).or_default().push(face.face_id());
    }

    // A cluster must contain at least one face good enough to seed it.
    let seedable: HashSet<FaceId> = faces
        .iter()
        .filter(|f| f.score() >= params.min_seed_score)
        .map(|f| f.face_id())
        .collect();

    let mut out: Vec<ProposedPerson> = merged
        .into_values()
        .filter(|group| group.len() >= params.min_faces)
        .filter(|group| group.iter().any(|f| seedable.contains(f)))
        .map(|mut faces| {
            // Stable order so repeated runs produce identical output.
            faces.sort_unstable_by_key(|f| f.as_uuid());
            ProposedPerson { faces }
        })
        .collect();

    out.sort_unstable_by(|a, b| {
        b.faces
            .len()
            .cmp(&a.faces.len())
            .then_with(|| a.faces[0].as_uuid().cmp(&b.faces[0].as_uuid()))
    });
    out
}

/// Label propagation over the similarity graph.
///
/// Deliberately deterministic, unlike dlib's original: the visit order comes
/// from a fixed-seed generator rather than real randomness. Re-running
/// clustering on unchanged data must produce the same people, otherwise the
/// user's grouping shuffles under them for no reason.
fn chinese_whispers<F: Face>(
    faces: &[F],
    constraints: &Constraints,
    params: &ClusterParams,
) -> Vec<usize> {
    let n = faces.len();

    // Sparse adjacency: only pairs close enough to matter, and never a pair the
    // user has said are different people. Forbidden peers are also collected
    // per node: dropping the edge alone is not enough, because a transitive
    // chain (a~x~b) can still deliver one label to both ends of a forbidden
    // pair — label adoption below has to check them.
    let mut adj: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    let mut forbidden: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            if constraints.forbids(faces[i].face_id(), faces[j].face_id()) {
                forbidden[i].push(j);
                forbidden[j].push(i);
                continue;
            }
            let d = cosine_distance(faces[i].embedding(), faces[j].embedding());
            if d <= params.cluster_distance {
                // Weight by closeness, so a tight pair outvotes a loose one.
                let w = 1.0 - d;
                adj[i].push((j, w));
                adj[j].push((i, w));
            }
        }
    }

    let mut labels: Vec<usize> = (0..n).collect();
    let mut rng = Lcg::new(0x5EED);

    // Converges well before this on realistic data; the cap just bounds the
    // pathological case.
    for _ in 0..20 {
        let mut order: Vec<usize> = (0..n).collect();
        rng.shuffle(&mut order);

        let mut changed = false;
        for &i in &order {
            if adj[i].is_empty() {
                continue;
            }
            // A label currently held by a must-not-linked peer is off limits:
            // adopting it is exactly how two faces the user split end up
            // proposed together again through a bridge face.
            let banned: HashSet<usize> = forbidden[i].iter().map(|&f| labels[f]).collect();
            let mut weight_per_label: HashMap<usize, f32> = HashMap::new();
            for &(j, w) in &adj[i] {
                if banned.contains(&labels[j]) {
                    continue;
                }
                *weight_per_label.entry(labels[j]).or_insert(0.0) += w;
            }
            // Ties break on the smaller label so the result stays stable.
            if let Some((&best, _)) = weight_per_label
                .iter()
                .max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(a.0)))
                && labels[i] != best
            {
                labels[i] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    labels
}

/// Choose up to `k` exemplars spanning a person's faces.
///
/// Starts at the medoid (the face closest to everything else, i.e. the most
/// typical), then repeatedly adds whichever remaining face is *furthest* from
/// everything already chosen. Picking spread-out points rather than the k most
/// typical ones is what lets a person still match after their appearance
/// drifts.
pub fn select_exemplars<F: Face>(faces: &[F], k: usize) -> Vec<usize> {
    let n = faces.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    if n <= k {
        return (0..n).collect();
    }

    // Medoid: minimum summed distance to all others.
    let medoid = (0..n)
        .min_by(|&a, &b| {
            let sum = |i: usize| -> f32 {
                (0..n)
                    .filter(|&j| j != i)
                    .map(|j| cosine_distance(faces[i].embedding(), faces[j].embedding()))
                    .sum()
            };
            sum(a).total_cmp(&sum(b))
        })
        .expect("n > 0");

    let mut chosen = vec![medoid];
    while chosen.len() < k {
        let next = (0..n).filter(|i| !chosen.contains(i)).max_by(|&a, &b| {
            let nearest_chosen = |i: usize| -> f32 {
                chosen
                    .iter()
                    .map(|&c| cosine_distance(faces[i].embedding(), faces[c].embedding()))
                    .min_by(f32::total_cmp)
                    .unwrap_or(0.0)
            };
            nearest_chosen(a).total_cmp(&nearest_chosen(b))
        });
        match next {
            Some(i) => chosen.push(i),
            None => break,
        }
    }
    chosen.sort_unstable();
    chosen
}

/// Disjoint-set with path compression. Small enough not to warrant a crate.
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

/// Tiny linear congruential generator, purely to get a reproducible shuffle.
/// Not for anything security-adjacent.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u32(&mut self) -> u32 {
        // Numerical Recipes constants.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next_u32() as usize) % (i + 1);
            items.swap(i, j);
        }
    }
}

/// Fingerprint of a clustering run's inputs.
///
/// Clustering is deterministic (fixed-seed shuffle, stable orderings), so
/// identical inputs give identical output — and the periodic pass is the
/// expensive all-pairs one. The caller stores this after a run and skips the
/// next run when it matches: repeated `eidetic faces` over an unchanged
/// library costs nothing, while any new face, assignment, pin, constraint,
/// parameter or model change re-runs everything.
///
/// Only the active model's faces are hashed. Attachment also consults members
/// embedded by other models (for must-not-link), so a change confined to those
/// rows does not trigger a run; at worst that leaves a face unattached, never
/// in a person it was split from.
///
/// Embeddings are deliberately not hashed: a face row's vector is written
/// once at detection and never mutated, so the face id (plus `model_id` for
/// the model that produced it) already stands for it.
pub fn fingerprint(
    faces: &[crate::FaceEmbedding],
    rejections: &[(FaceId, PersonId)],
    must_link: &[(FaceId, FaceId)],
    must_not_link: &[(FaceId, FaceId)],
    params: &ClusterParams,
    model_id: &str,
) -> String {
    use sha2::{Digest, Sha256};

    let mut lines: Vec<String> = Vec::with_capacity(
        faces.len() + rejections.len() + must_link.len() + must_not_link.len() + 1,
    );
    for f in faces {
        lines.push(format!(
            "f {} {} {} {}",
            f.id,
            f.person_id
                .map_or_else(|| "-".to_string(), |p| p.to_string()),
            f.pinned,
            f.ts_secs.is_some(), // photo/video decides seed eligibility
        ));
    }
    for (face, person) in rejections {
        lines.push(format!("r {face} {person}"));
    }
    // Link pairs are unordered facts; normalize so (a,b) and (b,a) match.
    let pair = |tag: &str, a: &FaceId, b: &FaceId| {
        let (lo, hi) = if a.as_uuid() <= b.as_uuid() {
            (a, b)
        } else {
            (b, a)
        };
        format!("{tag} {lo} {hi}")
    };
    for (a, b) in must_link {
        lines.push(pair("l", a, b));
    }
    for (a, b) in must_not_link {
        lines.push(pair("n", a, b));
    }
    lines.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(model_id.as_bytes());
    hasher.update(
        format!(
            "\nparams {} {} {} {} {}\n",
            params.attach_distance,
            params.cluster_distance,
            params.min_faces,
            params.min_seed_score,
            params.max_exemplars
        )
        .as_bytes(),
    );
    for line in &lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&hasher.finalize());
    eidetic_core::Sha256::from_bytes(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    mod fingerprint {
        use super::super::*;
        use crate::FaceEmbedding;
        use eidetic_core::AssetId;

        fn fe(person: Option<PersonId>, pinned: bool) -> FaceEmbedding {
            FaceEmbedding {
                id: FaceId::new(),
                asset_id: AssetId::new(),
                person_id: person,
                score: 0.9,
                embedding: vec![1.0, 0.0],
                pinned,
                ts_secs: None,
            }
        }

        #[test]
        fn is_order_independent_and_input_sensitive() {
            let params = ClusterParams::default();
            let a = fe(None, false);
            let b = fe(Some(PersonId::new()), false);
            let link = (a.id, b.id);

            let fp1 = fingerprint(
                &[a.clone(), b.clone()],
                &[],
                &[link],
                &[],
                &params,
                "auraface-v1",
            );
            // Same facts, different orderings: face order and pair order.
            let fp2 = fingerprint(
                &[b.clone(), a.clone()],
                &[],
                &[(link.1, link.0)],
                &[],
                &params,
                "auraface-v1",
            );
            assert_eq!(fp1, fp2, "ordering must not matter");

            // Every kind of input change must change the fingerprint —
            // otherwise a run that should happen gets skipped.
            let mut assigned = a.clone();
            assigned.person_id = Some(PersonId::new());
            let mut pinned = a.clone();
            pinned.pinned = true;
            let mut other_params = ClusterParams::default();
            other_params.min_faces += 1;
            let changed = [
                fingerprint(
                    std::slice::from_ref(&a),
                    &[],
                    &[link],
                    &[],
                    &params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[assigned, b.clone()],
                    &[],
                    &[link],
                    &[],
                    &params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[pinned, b.clone()],
                    &[],
                    &[link],
                    &[],
                    &params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[a.clone(), b.clone()],
                    &[],
                    &[],
                    &[],
                    &params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[a.clone(), b.clone()],
                    &[],
                    &[],
                    &[link],
                    &params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[a.clone(), b.clone()],
                    &[(a.id, PersonId::new())],
                    &[link],
                    &[],
                    &params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[a.clone(), b.clone()],
                    &[],
                    &[link],
                    &[],
                    &other_params,
                    "auraface-v1",
                ),
                fingerprint(
                    &[a.clone(), b.clone()],
                    &[],
                    &[link],
                    &[],
                    &params,
                    "arcface-buffalo-l",
                ),
            ];
            for (i, fp) in changed.iter().enumerate() {
                assert_ne!(*fp, fp1, "variant {i} must change the fingerprint");
            }
        }
    }

    /// Minimal test face.
    struct TestFace {
        id: FaceId,
        embedding: Vec<f32>,
        score: f32,
    }

    impl Face for TestFace {
        fn face_id(&self) -> FaceId {
            self.id
        }
        fn embedding(&self) -> &[f32] {
            &self.embedding
        }
        fn score(&self) -> f32 {
            self.score
        }
    }

    fn face(embedding: Vec<f32>) -> TestFace {
        TestFace {
            id: FaceId::new(),
            embedding: normalize(embedding),
            score: 0.95,
        }
    }

    fn normalize(mut v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            v.iter_mut().for_each(|x| *x /= n);
        }
        v
    }

    /// A tight group of near-identical unit vectors around `base`.
    fn cluster_around(base: [f32; 3], n: usize, jitter: f32) -> Vec<TestFace> {
        (0..n)
            .map(|i| {
                let d = jitter * (i as f32);
                face(vec![base[0] + d, base[1] + d * 0.5, base[2]])
            })
            .collect()
    }

    #[test]
    fn cosine_distance_basics() {
        let a = normalize(vec![1.0, 0.0]);
        assert!(cosine_distance(&a, &a).abs() < 1e-6, "identical is 0");

        let b = normalize(vec![0.0, 1.0]);
        assert!(
            (cosine_distance(&a, &b) - 1.0).abs() < 1e-6,
            "orthogonal is 1"
        );
    }

    #[test]
    fn cosine_distance_rejects_mismatched_widths() {
        // A model change must never look like a close match.
        assert_eq!(
            cosine_distance(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
            f32::INFINITY
        );
    }

    #[test]
    fn assign_to_existing_picks_the_nearest_person() {
        let target = normalize(vec![1.0, 0.0, 0.0]);
        let other = normalize(vec![0.0, 1.0, 0.0]);
        let alice = PersonId::new();
        let bob = PersonId::new();
        let people = vec![
            PersonExemplars {
                person: bob,
                exemplars: vec![other],
            },
            PersonExemplars {
                person: alice,
                exemplars: vec![target.clone()],
            },
        ];

        let f = face(vec![1.0, 0.05, 0.0]);
        let got = assign_to_existing(
            &f,
            &people,
            &HashMap::new(),
            &Constraints::default(),
            &ClusterParams::default(),
        );
        assert_eq!(got, Some(alice));
    }

    #[test]
    fn assign_to_existing_returns_none_when_nothing_is_close() {
        let people = vec![PersonExemplars {
            person: PersonId::new(),
            exemplars: vec![normalize(vec![1.0, 0.0, 0.0])],
        }];
        // Orthogonal: distance 1.0, well beyond the 0.5 attach threshold.
        let f = face(vec![0.0, 1.0, 0.0]);
        assert!(
            assign_to_existing(
                &f,
                &people,
                &HashMap::new(),
                &Constraints::default(),
                &ClusterParams::default()
            )
            .is_none()
        );
    }

    #[test]
    fn assign_to_existing_honours_a_rejection() {
        let alice = PersonId::new();
        let exemplar = normalize(vec![1.0, 0.0, 0.0]);
        let people = vec![PersonExemplars {
            person: alice,
            exemplars: vec![exemplar],
        }];
        let f = face(vec![1.0, 0.02, 0.0]);

        // Without the rejection it would attach.
        assert_eq!(
            assign_to_existing(
                &f,
                &people,
                &HashMap::new(),
                &Constraints::default(),
                &ClusterParams::default()
            ),
            Some(alice)
        );

        let constraints = Constraints::new(vec![(f.face_id(), alice)], vec![], vec![]);
        assert!(
            assign_to_existing(
                &f,
                &people,
                &HashMap::new(),
                &constraints,
                &ClusterParams::default()
            )
            .is_none(),
            "a rejected pairing must not be re-proposed"
        );
    }

    #[test]
    fn assign_to_existing_uses_nearest_exemplar_not_an_average() {
        // Two far-apart exemplars for one person (e.g. child and adult photos).
        // Their midpoint is close to neither, so an averaging implementation
        // would fail to match a face sitting right on top of one of them.
        let young = normalize(vec![1.0, 0.0, 0.0]);
        let older = normalize(vec![0.0, 0.0, 1.0]);
        let person = PersonId::new();
        let people = vec![PersonExemplars {
            person,
            exemplars: vec![young, older.clone()],
        }];

        let f = TestFace {
            id: FaceId::new(),
            embedding: older,
            score: 0.95,
        };
        assert_eq!(
            assign_to_existing(
                &f,
                &people,
                &HashMap::new(),
                &Constraints::default(),
                &ClusterParams::default()
            ),
            Some(person)
        );
    }

    /// P with one exemplar on `[1,0,0]`, plus Q with one exemplar on `q` when
    /// given. A face near `[1,0.05,0]` sits ~0.001 from P and ~0.26 from
    /// `q = [1,1,0]`: both within the default 0.5 attach distance, P nearer.
    fn p_and_q(q: Option<[f32; 3]>) -> (PersonId, PersonId, Vec<PersonExemplars>) {
        let p = PersonId::new();
        let q_id = PersonId::new();
        let mut people = vec![PersonExemplars {
            person: p,
            exemplars: vec![normalize(vec![1.0, 0.0, 0.0])],
        }];
        if let Some(q) = q {
            people.push(PersonExemplars {
                person: q_id,
                exemplars: vec![normalize(q.to_vec())],
            });
        }
        (p, q_id, people)
    }

    fn split(a: FaceId, b: FaceId) -> Constraints {
        Constraints::new(vec![], vec![], vec![(a, b)])
    }

    #[test]
    fn assign_to_existing_skips_a_person_holding_a_forbidden_member() {
        // A belongs to P without being one of its exemplars (a video face, say).
        let (p, _, people) = p_and_q(None);
        let a = FaceId::new();
        let members = HashMap::from([(a, p)]);
        let b = face(vec![1.0, 0.05, 0.0]);
        let params = ClusterParams::default();

        assert_eq!(
            assign_to_existing(&b, &people, &members, &Constraints::default(), &params),
            Some(p),
            "without the split, B joins P"
        );
        assert_eq!(
            assign_to_existing(&b, &people, &members, &split(a, b.face_id()), &params),
            None,
            "B must not join the person holding A"
        );
    }

    #[test]
    fn assign_to_existing_blocked_face_falls_back_to_next_nearest() {
        let a = FaceId::new();
        let b = face(vec![1.0, 0.05, 0.0]);
        let params = ClusterParams::default();

        let (p, q, people) = p_and_q(Some([1.0, 1.0, 0.0]));
        let members = HashMap::from([(a, p)]);
        assert_eq!(
            assign_to_existing(&b, &people, &members, &split(a, b.face_id()), &params),
            Some(q)
        );

        // Q exists but is beyond attach_distance: nothing allowed is close.
        let (p, _, people) = p_and_q(Some([0.0, 1.0, 0.0]));
        let members = HashMap::from([(a, p)]);
        assert_eq!(
            assign_to_existing(&b, &people, &members, &split(a, b.face_id()), &params),
            None
        );
    }

    #[test]
    fn assign_to_existing_unrelated_must_not_link_does_not_block() {
        let (p, _, mut people) = p_and_q(Some([1.0, 1.0, 0.0]));
        let r = PersonId::new();
        people.push(PersonExemplars {
            person: r,
            exemplars: vec![normalize(vec![0.0, 0.0, 1.0])],
        });
        let b = face(vec![1.0, 0.05, 0.0]);
        let params = ClusterParams::default();

        // Partner has no person at all.
        let loose = FaceId::new();
        assert_eq!(
            assign_to_existing(
                &b,
                &people,
                &HashMap::new(),
                &split(loose, b.face_id()),
                &params
            ),
            Some(p)
        );

        // Partner belongs to a far-away person B was never going to join.
        let r1 = FaceId::new();
        let members = HashMap::from([(r1, r)]);
        assert_eq!(
            assign_to_existing(&b, &people, &members, &split(r1, b.face_id()), &params),
            Some(p)
        );
    }

    #[test]
    fn assign_all_to_existing_counts_faces_attached_earlier_in_the_run() {
        let c1 = face(vec![1.0, 0.05, 0.0]);
        let c2 = face(vec![1.0, 0.03, 0.0]);
        let constraints = split(c1.face_id(), c2.face_id());
        let params = ClusterParams::default();

        let (p, q, people) = p_and_q(Some([1.0, 1.0, 0.0]));
        assert_eq!(
            assign_all_to_existing(&[&c1, &c2], &people, [], &constraints, &params),
            vec![Some(p), Some(q)]
        );
        assert_eq!(
            assign_all_to_existing(&[&c2, &c1], &people, [], &constraints, &params),
            vec![Some(p), Some(q)],
            "whichever comes first takes the nearer person"
        );

        let (p, _, people) = p_and_q(None);
        assert_eq!(
            assign_all_to_existing(&[&c1, &c2], &people, [], &constraints, &params),
            vec![Some(p), None]
        );
    }

    #[test]
    fn assign_all_to_existing_keeps_rejections_and_plain_nearest() {
        let (p, q, people) = p_and_q(Some([1.0, 1.0, 0.0]));
        let rejected = face(vec![1.0, 0.05, 0.0]);
        let plain = face(vec![1.0, 0.04, 0.0]);
        let existing = FaceId::new();
        let constraints = Constraints::new(vec![(rejected.face_id(), p)], vec![], vec![]);

        assert_eq!(
            assign_all_to_existing(
                &[&rejected, &plain],
                &people,
                [(existing, p)],
                &constraints,
                &ClusterParams::default()
            ),
            vec![Some(q), Some(p)]
        );
    }

    #[test]
    fn propose_people_finds_two_distinct_groups() {
        let mut faces = cluster_around([1.0, 0.0, 0.0], 4, 0.01);
        faces.extend(cluster_around([0.0, 1.0, 0.0], 4, 0.01));

        let proposed = propose_people(&faces, &Constraints::default(), &ClusterParams::default());
        assert_eq!(proposed.len(), 2, "expected two people, got {proposed:?}");
        assert!(proposed.iter().all(|p| p.faces.len() == 4));
    }

    #[test]
    fn propose_people_drops_groups_below_min_faces() {
        // Four of one person, one lone stranger.
        let mut faces = cluster_around([1.0, 0.0, 0.0], 4, 0.01);
        faces.push(face(vec![0.0, 0.0, 1.0]));

        let proposed = propose_people(&faces, &Constraints::default(), &ClusterParams::default());
        assert_eq!(proposed.len(), 1, "the singleton must not become a person");
        assert_eq!(proposed[0].faces.len(), 4);
    }

    #[test]
    fn propose_people_is_deterministic() {
        let mut faces = cluster_around([1.0, 0.0, 0.0], 5, 0.01);
        faces.extend(cluster_around([0.0, 1.0, 0.0], 5, 0.01));

        let a = propose_people(&faces, &Constraints::default(), &ClusterParams::default());
        let b = propose_people(&faces, &Constraints::default(), &ClusterParams::default());
        assert_eq!(a, b, "re-running must not reshuffle the user's people");
    }

    #[test]
    fn must_not_link_keeps_two_faces_apart() {
        // Three near-identical faces would normally form one group.
        let faces = cluster_around([1.0, 0.0, 0.0], 3, 0.001);
        let params = ClusterParams {
            min_faces: 2,
            ..Default::default()
        };

        let together = propose_people(&faces, &Constraints::default(), &params);
        assert_eq!(together.len(), 1, "baseline: they cluster together");

        // Forbid every pair, so no edges survive at all.
        let forbidden = vec![
            (faces[0].face_id(), faces[1].face_id()),
            (faces[0].face_id(), faces[2].face_id()),
            (faces[1].face_id(), faces[2].face_id()),
        ];
        let constraints = Constraints::new(vec![], vec![], forbidden);
        let apart = propose_people(&faces, &constraints, &params);
        assert!(
            apart.is_empty(),
            "with every edge forbidden no group should reach min_faces, got {apart:?}"
        );
    }

    /// Dropping the forbidden edge alone is not enough: with a bridge face
    /// close to both, label propagation used to deliver one label to both
    /// ends of the forbidden pair anyway — regrouping exactly the two faces
    /// the user split.
    #[test]
    fn must_not_link_survives_a_transitive_bridge() {
        let faces = cluster_around([1.0, 0.0, 0.0], 3, 0.001);
        let params = ClusterParams {
            min_faces: 2,
            ..Default::default()
        };

        // Forbid only the outer pair; face 1 stays a bridge to both.
        let (a, c) = (faces[0].face_id(), faces[2].face_id());
        let constraints = Constraints::new(vec![], vec![], vec![(a, c)]);

        let proposed = propose_people(&faces, &constraints, &params);
        for group in &proposed {
            assert!(
                !(group.faces.contains(&a) && group.faces.contains(&c)),
                "a forbidden pair reached one group through the bridge: {proposed:?}"
            );
        }
    }

    /// A must-link union that would join a must-not-linked pair — directly,
    /// or through a chain of must-links — is refused. Must-not wins.
    #[test]
    fn must_not_link_wins_over_a_must_link_chain() {
        // Two tight pairs, far apart: {0,1} and {2,3}.
        let mut faces = cluster_around([1.0, 0.0, 0.0], 2, 0.001);
        faces.extend(cluster_around([0.0, 1.0, 0.0], 2, 0.001));
        let params = ClusterParams {
            min_faces: 2,
            ..Default::default()
        };

        // The user links one face across the groups but has split another
        // pair across the same two groups. Honouring the link would union
        // the groups and regroup the split pair.
        let (f1, f3) = (faces[1].face_id(), faces[3].face_id());
        let constraints = Constraints::new(
            vec![],
            vec![(faces[0].face_id(), faces[2].face_id())],
            vec![(f1, f3)],
        );

        let proposed = propose_people(&faces, &constraints, &params);
        assert_eq!(
            proposed.len(),
            2,
            "the conflicting must-link must be skipped, got {proposed:?}"
        );
        for group in &proposed {
            assert!(
                !(group.faces.contains(&f1) && group.faces.contains(&f3)),
                "split pair regrouped: {proposed:?}"
            );
        }

        // Direct conflict: the same pair both linked and split. Must-not wins.
        let (f0, f2) = (faces[0].face_id(), faces[2].face_id());
        let direct = Constraints::new(vec![], vec![(f0, f2)], vec![(f0, f2)]);
        let proposed = propose_people(&faces, &direct, &params);
        for group in &proposed {
            assert!(
                !(group.faces.contains(&f0) && group.faces.contains(&f2)),
                "directly conflicted pair regrouped: {proposed:?}"
            );
        }
    }

    #[test]
    fn must_link_joins_otherwise_separate_groups() {
        let mut faces = cluster_around([1.0, 0.0, 0.0], 2, 0.001);
        faces.extend(cluster_around([0.0, 1.0, 0.0], 2, 0.001));
        let params = ClusterParams {
            min_faces: 2,
            ..Default::default()
        };

        assert_eq!(
            propose_people(&faces, &Constraints::default(), &params).len(),
            2,
            "baseline: two separate groups"
        );

        // The user says one face from each group is the same person.
        let constraints = Constraints::new(
            vec![],
            vec![(faces[0].face_id(), faces[2].face_id())],
            vec![],
        );
        let joined = propose_people(&faces, &constraints, &params);
        assert_eq!(joined.len(), 1, "must_link should merge the groups");
        assert_eq!(joined[0].faces.len(), 4);
    }

    #[test]
    fn a_cluster_of_only_low_quality_faces_is_not_seeded() {
        let mut faces = cluster_around([1.0, 0.0, 0.0], 4, 0.01);
        for f in &mut faces {
            f.score = 0.5; // below min_seed_score
        }
        let proposed = propose_people(&faces, &Constraints::default(), &ClusterParams::default());
        assert!(
            proposed.is_empty(),
            "blurry faces must not seed a person: {proposed:?}"
        );
    }

    #[test]
    fn one_good_face_is_enough_to_seed_a_cluster() {
        let mut faces = cluster_around([1.0, 0.0, 0.0], 4, 0.01);
        for f in faces.iter_mut().skip(1) {
            f.score = 0.5;
        }
        let proposed = propose_people(&faces, &Constraints::default(), &ClusterParams::default());
        assert_eq!(proposed.len(), 1);
        assert_eq!(
            proposed[0].faces.len(),
            4,
            "weak faces still join the group"
        );
    }

    #[test]
    fn select_exemplars_returns_everything_when_under_k() {
        let faces = cluster_around([1.0, 0.0, 0.0], 3, 0.01);
        assert_eq!(select_exemplars(&faces, 5), vec![0, 1, 2]);
    }

    #[test]
    fn select_exemplars_spans_the_extremes() {
        // Two tight lumps far apart. Chosen exemplars must cover both, which is
        // the property that keeps matching working as appearance drifts.
        let mut faces = cluster_around([1.0, 0.0, 0.0], 4, 0.001);
        faces.extend(cluster_around([0.0, 0.0, 1.0], 4, 0.001));

        let picked = select_exemplars(&faces, 2);
        assert_eq!(picked.len(), 2);
        let first_lump = picked.iter().filter(|&&i| i < 4).count();
        assert_eq!(
            first_lump, 1,
            "one exemplar per lump expected, picked {picked:?}"
        );
    }

    #[test]
    fn select_exemplars_handles_empty_and_zero_k() {
        let faces = cluster_around([1.0, 0.0, 0.0], 3, 0.01);
        assert!(select_exemplars(&faces, 0).is_empty());
        let none: Vec<TestFace> = vec![];
        assert!(select_exemplars(&none, 3).is_empty());
    }

    #[test]
    fn propose_people_on_empty_input() {
        let none: Vec<TestFace> = vec![];
        assert!(
            propose_people(&none, &Constraints::default(), &ClusterParams::default()).is_empty()
        );
    }

    #[test]
    fn union_find_merges_transitively() {
        let mut uf = UnionFind::new(5);
        uf.union(0, 1);
        uf.union(1, 2);
        assert_eq!(uf.find(0), uf.find(2));
        assert_ne!(uf.find(0), uf.find(3));
    }

    #[test]
    fn lcg_shuffle_is_reproducible_and_keeps_every_element() {
        let mut a: Vec<usize> = (0..20).collect();
        let mut b = a.clone();
        Lcg::new(0x5EED).shuffle(&mut a);
        Lcg::new(0x5EED).shuffle(&mut b);
        assert_eq!(a, b, "same seed must give the same order");

        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..20).collect::<Vec<_>>(), "nothing lost");
    }
}
