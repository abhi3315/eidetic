# ADR-0010: Face stack = YuNet + AuraFace by default, InsightFace opt-in

Date: 2026-07-30
Status: accepted (amended same day — see "Amendment: AuraFace")

## Context

`goals.md` lists face grouping as the next phase, and `AGENTS.md` has recorded the intended stack as **SCRFD + ArcFace via `ort`** since the beginning. Before writing code, that plan was checked against two questions: is it still the right choice in 2026, and may this project actually use it?

The accuracy answer is yes — SCRFD + ArcFace is still the leader, and it is what every comparable self-hosted photo app ships (Immich `buffalo_l`, PhotoPrism SCRFD 0.5g, LibrePhotos `buffalo_sc`).

The licensing answer is **no**, and it changes the decision. InsightFace states plainly that while its *code* is MIT, "the training data containing the annotation (and the models trained with these data) are available for **non-commercial research purposes only**." That covers SCRFD, ArcFace, every `buffalo_*` pack, and `antelopev2`. Immich's own ML README records that Jia Guo granted them permission by email on 2023-03-18 and adds that "this permission does not extend to the redistribution or commercial use of their models by third parties" — so it is not transferable to eidetic. Every project surveyed ships this stack; every one of them ignores the licence.

The obvious alternatives are worse, not better:

| Candidate | Licence problem |
|---|---|
| YOLOv8 / YOLOv11-face | **AGPL-3.0**, and Ultralytics asserts trained weights are AGPL derivatives |
| YOLO5Face (what Ente ships) | GPL-3.0 |
| EdgeFace | CC-BY-NC-SA-4.0 (non-commercial *and* share-alike) |
| AdaFace, LVFace | MIT code, but research-only data lineage; no first-party ONNX export |

There is exactly one clean permissive pair, both from OpenCV Zoo and both already on HuggingFace under the `opencv` org:

- **YuNet** — MIT. Detection, bbox + 5 landmarks.
- **SFace** — Apache-2.0. Recognition, 128-dim embedding.

No permissively-licensed detector newer or better than YuNet surfaced for 2026.

## Decision

Ship **YuNet + SFace as the default**, and make the detector/embedder **pluggable** so InsightFace `buffalo_l` is an opt-in the user chooses and downloads themselves, with a licence notice at download time.

This is the same shape as ADR-0008's HEIC decision: the permissive, always-works option is the default; the better-but-encumbered option is available to a user who knowingly opts in. It keeps eidetic's own licence story clean without pretending the accuracy gap doesn't exist.

- **Detection:** `opencv/face_detection_yunet`, file `face_detection_yunet_2023mar.onnx`. Stride decoding and NMS are ours to implement, since we are not going through OpenCV's `FaceDetectorYN` wrapper.
- **Recognition:** `fal/AuraFace-v1`, file `glintr100.onnx` — see the amendment below. `opencv/face_recognition_sface` remains selectable as `sface`.

**Measured ONNX signatures** (verified by loading both files through `ort`, rather than trusting the model cards — the YuNet README is inconsistent and mentions a `2026may` file that is not actually published on HuggingFace, and describes `2023mar` as 320×320 when the published file is 640×640):

```
YuNet   in : input [1,3,640,640] f32   (fixed, not dynamic)
        out: cls_{8,16,32}  [1,{6400,1600,400},1]
             obj_{8,16,32}  [1,{6400,1600,400},1]
             bbox_{8,16,32} [1,{6400,1600,400},4]
             kps_{8,16,32}  [1,{6400,1600,400},10]
SFace   in : data  [1,3,112,112] f32
        out: fc1   [1,128] f32
```

One anchor per cell, priors at cell centres: 640/8=80 → 80²=6400, 640/16=40 → 1600, 640/32=20 → 400. `kps_*` is 10 values = 5 points × (x,y).

Decode per cell (row-major, `idx = r*cols + c`), matching OpenCV's `FaceDetectorYNImpl::postProcess`:

```
cx = (c + bbox[0]) * stride      w = exp(bbox[2]) * stride
cy = (r + bbox[1]) * stride      h = exp(bbox[3]) * stride
score = sqrt(clamp(cls) * clamp(obj))
kp_i  = ((c + kps[2i]) * stride, (r + kps[2i+1]) * stride)
```
- **Opt-in:** `EIDETIC_FACE_MODEL=buffalo_l` selects `immich-app/buffalo_l` (SCRFD 10G + ArcFace `w600k_r50`, 512-dim). Unknown values are rejected, matching `EIDETIC_MODEL`/`EIDETIC_ACCELERATOR`.
- Both paths run through `ort`, so the CUDA EP from ADR-0006 applies unchanged.

### Alignment (mandatory)

Every recognition model here expects a **similarity-transformed** 112×112 crop, not a raw bbox crop; skipping alignment costs several points of verification accuracy. The step is:

1. Take the detector's 5 landmarks. Order them **left eye, right eye, nose, left mouth, right mouth** — YuNet emits right-eye-first and must be reordered.
2. Solve the least-squares similarity transform (rotation + uniform scale + translation, 4 DoF — the Umeyama estimate) onto the standard ArcFace template:
   `[[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]]`
3. Warp to 112×112 with bilinear sampling.
4. Normalise `(px - 127.5) / 128.0`, NCHW.

The `image` crate has no affine warp. Use `imageproc::geometric_transformations::warp` with `Projection::from_matrix` built from our own 3×3 — **not** `from_control_points`, which fits a 4-point projective homography (wrong transform class; it shears faces).

### Clustering

Hand-rolled, ~200-300 lines, no new dependencies:

- **Incremental assignment** on import: compare each new face against stored **exemplars of confirmed people** (2-5 medoids per person, *not* a single centroid — a centroid drifts badly as someone ages). Attach at cosine distance ≤ **0.5**.
- **Periodic full pass** over the unassigned pool only, using Chinese Whispers on a kNN graph at ≤ **0.4**, minting a person when a component has ≥ **3** members.
- Thresholds are configurable. 0.5 / 0.4 / 3 are Immich's shipped `maxDistance` / tuned-library value / `minFaces`; PhotoPrism independently caps cluster radius at 0.42, which corroborates the range. There is no authoritative upstream threshold, so these are starting points to tune, not constants.
- **Filter by face quality before clustering** (detector score, pixel size, blur) and never let a low-quality face seed a cluster. Both PhotoPrism and Ente do this; it is the main defence against false bridges.

`petal-clustering` and `linfa-clustering` were considered and rejected. Both are maintained and could take a cosine metric, but they drag `ndarray`/`rayon`/`petal-neighbors` (with an `ndarray` version-conflict risk against the ML path) and — decisively — **neither supports the must-link / must-not-link constraints below**, which are the most valuable part of the design.

### User corrections are durable facts, not cluster state

The whole point: a re-cluster must always be safe to run. Immich's documented full reset "deletes all previously assigned names" — that is the failure to avoid. So:

- naming → a `persons` row, survives trivially
- "this face is Alice" → face pinned `assignment_source = 'user'`, frozen as an exemplar and a seed; **never re-assigned**
- "not Alice" → a negative constraint row the assigner must consult and re-clustering must honour
- merge → a merge/alias edge, so a later re-split gets re-merged
- split → a must-not-link constraint between the seed faces

A full re-cluster then runs as *constrained* clustering: user-pinned faces pre-seeded, must-link/must-not-link applied to the graph before propagation.

## Consequences

**Good:**

- eidetic's licence story stays clean and self-consistent; no reliance on a permission granted to somebody else.
- Same pluggable-default pattern as ADR-0008, so the codebase has one way of handling "permissive default, encumbered opt-in".
- Alignment and clustering code is identical for both model pairs, so the opt-in is a model swap rather than a second pipeline.
- Reuses what ADR-0005 and ADR-0006 already built: the separate-embeddings-table shape, the `VectorIndex` trait, and the verified CUDA EP.
- At personal scale the incremental path compares one face against ~100-500 exemplars — microseconds of exact brute force. No ANN index needed, and none of this changes the storage decision.

**Bad / accepted:**

- YuNet is weaker on very large close-up faces and >90° rotations (trained for ~10-300 px faces). It remains the best *permissive* detector with 5-point landmarks; two independent searches found nothing newer.
- ~~SFace is meaningfully weaker than ArcFace~~ — largely resolved by the AuraFace amendment below, which recovers the 512-dim ArcFace accuracy class without the licence problem. The residual gap is in detection, not recognition.
- Two model pairs to keep working, with different embedding dimensions. Mitigated by storing `dim` per row (same as the `embeddings` table) so a model change is detectable rather than silently mixing widths.
- We own YuNet's stride decoding and NMS, because we bypass OpenCV's wrapper. More code than calling a library, and it needs testing against the real ONNX output.
- Known-unfixable failure modes, consistent across every project surveyed: infants cluster across *different* children and drift as they age; siblings and twins often cannot be separated at all; the same adult across 15+ years usually splits. Face grouping will need user correction — which is why the constraint model above is load-bearing rather than a nice-to-have.

**Verified end-to-end (2026-07-30)** against a real portrait, via the
`detects_and_embeds_a_real_face` ignored test:

```
1 face, score 0.946, bbox x=982 y=229 w=657 h=914
right_eye (1163, 568)  left_eye (1493, 561)  nose (1328, 738)
```

That single run closes both of the assumptions unit tests cannot reach:

- **Input convention** — BGR with raw 0-255 values and no mean/std normalisation, matching OpenCV. A wrong channel order or missing normalisation would not detect at 0.946.
- **Keypoint order** — the subject's right eye lands at a smaller x than the left, and the nose sits horizontally between them, so YuNet really is right-eye-first and the reorder in `Landmarks::from_yunet_order` is correct. This mattered because a mirrored alignment produces a perfectly well-formed *worse* embedding, with no error to notice.
- Also confirms the stride decode and the letterbox scale inverse, since the box comes back in original-image coordinates at a plausible size.

**Still to verify:**

- Whether 128-dim SFace clusters acceptably on a real library. If not, the opt-in stops being optional in practice. This can only be answered by running the clustering over a real photo collection.

## Alternatives considered

- **SCRFD + ArcFace as the default** (the original `AGENTS.md` plan). Rejected on licence: non-commercial research only, and Immich's permission is explicitly non-transferable. Retained as the opt-in.
- **YOLO-family detectors.** Rejected: AGPL-3.0/GPL-3.0 weights are worse for a permissive project than non-commercial ones.
- **MediaPipe BlazeFace** (Apache-2.0, so licence-clean). Rejected: poor on small faces, which is most faces in a real photo library, and it emits 6 keypoints rather than the 5 the alignment template expects.
- **A clustering crate.** Rejected — see above; no constraint support is the deciding factor.
- **HDBSCAN.** Rejected: solves a variable-density problem we don't have, hardest to make incremental, and its `min_cluster_size` behaviour swallows the many-singleton case (background strangers) unpredictably. LibrePhotos uses it and cluster quality is a common complaint.

## Amendment: AuraFace (same day)

The original decision accepted SFace's 128-dim embedding as "the price of the
licence". A follow-up search — prompted by asking whether this really was the
best available option — surfaced **AuraFace** (`fal/AuraFace-v1`), which the
first research pass missed. It makes that concession unnecessary:

| | SFace | AuraFace |
|---|---|---|
| Licence | Apache-2.0 | Apache-2.0 |
| Architecture | MobileFaceNet | ResNet100 + ArcFace loss |
| Embedding | 128-dim | **512-dim** |
| LFW | ~0.994 (unspecified set) | **0.9965** |
| Training data | OpenCV Zoo | *deliberately* commercially-usable sources |

The last row is the point: AuraFace exists specifically to solve the problem
this ADR is built around — trained on commercially-available data so it does
not inherit InsightFace's research-only restriction.

**Measured, not assumed** (introspected the real 260 MB file):

```
IN   data   [-1, 3, 112, 112]     same input name and shape as SFace
OUT  1333   [1, 512]
```

So it is a drop-in for the embedding half: identical aligned-crop contract, no
pipeline change. Its output tensor is named `1333` rather than `fc1`, which the
analyzer already tolerates because it takes the session's sole output instead of
a hard-coded name. Verified end-to-end on the same portrait — 512-dim,
unit-norm, unchanged detection.

**Default becomes `auraface`** (YuNet detect + AuraFace embed). `sface` stays
selectable (smaller and faster, 38 MB vs 260 MB), `buffalo_l` remains the
encumbered opt-in.

**Licence caveat, recorded rather than glossed:** the AuraFace repo is labelled
Apache-2.0 but also ships files with *verbatim InsightFace names*
(`scrfd_10g_bnkps.onnx`, `2d106det.onnx`, `genderage.onnx`), and its README
never states which weights fal actually trained or addresses the detector's
provenance. fal cannot grant Apache-2.0 over weights they did not train. We
therefore use **only** `glintr100.onnx`, the recognition model that is their own
stated work and product pitch, and keep detection on YuNet — whose MIT licence
is unambiguous. A unit test asserts every non-`buffalo_l` pair detects with
YuNet, so a future edit cannot quietly promote an encumbered detector into a
default path.

## References

- InsightFace licence: https://github.com/deepinsight/insightface
- Immich ML README (the non-transferable permission): https://github.com/immich-app/immich/blob/main/machine-learning/README.md
- YuNet (MIT): https://github.com/opencv/opencv_zoo/blob/main/models/face_detection_yunet/README.md · https://huggingface.co/opencv/face_detection_yunet
- SFace (Apache-2.0): https://huggingface.co/opencv/face_recognition_sface
- ArcFace alignment template: https://github.com/deepinsight/insightface/issues/1154
- Immich thresholds: https://docs.immich.app/features/facial-recognition/ · https://docs.immich.app/guides/better-facial-clusters/
- PhotoPrism face pipeline: https://github.com/photoprism/photoprism/blob/develop/internal/ai/face/README.md
- Ultralytics licence (AGPL on trained weights): https://www.ultralytics.com/license
