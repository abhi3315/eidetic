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
/// rejected for this face, or `None` to leave the face for the periodic pass.
pub fn assign_to_existing<F: Face>(
    face: &F,
    people: &[PersonExemplars],
    constraints: &Constraints,
    params: &ClusterParams,
) -> Option<PersonId> {
    let mut best: Option<(PersonId, f32)> = None;

    for person in people {
        if constraints.rejects(face.face_id(), person.person) {
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
    for &(a, b) in &constraints.must_link {
        if let (Some(&ia), Some(&ib)) = (index.get(&a), index.get(&b)) {
            uf.union(ia, ib);
        }
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
    // user has said are different people.
    let mut adj: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            if constraints.forbids(faces[i].face_id(), faces[j].face_id()) {
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
            let mut weight_per_label: HashMap<usize, f32> = HashMap::new();
            for &(j, w) in &adj[i] {
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

#[cfg(test)]
mod tests {
    use super::*;

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
                &Constraints::default(),
                &ClusterParams::default()
            ),
            Some(alice)
        );

        let constraints = Constraints::new(vec![(f.face_id(), alice)], vec![], vec![]);
        assert!(
            assign_to_existing(&f, &people, &constraints, &ClusterParams::default()).is_none(),
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
                &Constraints::default(),
                &ClusterParams::default()
            ),
            Some(person)
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
