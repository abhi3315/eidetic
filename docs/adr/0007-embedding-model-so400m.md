# ADR-0007: Default image embedding model = SigLIP 2 so400m/384 (1152-dim)

Date: 2026-07-23
Status: accepted (supersedes ADR-0004)

## Context

ADR-0004 chose SigLIP 2 **base** (768-dim), explicitly because base "is small enough for CPU inference at acceptable latency," and named So400m as the deliberate future upgrade "when we want quality." It also rejected `large` (1024-dim) as an "awkward middle ground."

Two of its own preconditions for the upgrade are now met:

1. **GPU available** (ADR-0006, verified ~8× on the card) — the CPU-latency reason for defaulting to base is gone. 16 GB VRAM is ample for a ~400M-param ViT.
2. **Vector store no longer dimension-constrained** (ADR-0005) — brute-force cosine over 1152-dim is trivially fast at personal scale; the wider column costs ~4.5 KB/asset.

External confirmation: Immich ships exactly `ViT-SO400M-16-SigLIP2-384__webli` for this same job, and `onnx-community` publishes so400m ONNX exports.

## Decision

Default the image embedding model to **SigLIP 2 so400m/384**, producing **1152-dim** L2-normalised embeddings.

- Model: `onnx-community/siglip2-so400m-patch14-384-ONNX` (or patch16-512 if the eval favours it), loaded via `ort` per ADR-0003/0006.
- Keep `base` selectable via `EIDETIC_MODEL` for CPU-only machines and macOS.
- **Decide the exact variant with data:** run base vs so400m through the existing `eidetic eval` COCO harness (R@1/5/10, MRR) on the GPU and pick on the measured retrieval delta — do not assume.

## Consequences

**Good:**

- Meaningfully better retrieval quality than base (SigLIP 2 scales with size) — the whole point of search quality.
- Executes ADR-0004's own documented migration path — no new architecture, just activation.
- Matches Immich's production choice for the same use case.

**Bad / accepted:**

- 1152-dim widens the embeddings table (~4.5 KB/asset vs 3 KB). Immaterial at personal scale.
- Re-embedding the library on the switch is unavoidable; the separate embeddings table (ADR-0005) makes running two dims side-by-side during the swap easy.
- so400m is CPU-slow — hence `base` stays available for CPU/macOS.
- SigLIP 2 has a known family-wide weakness on fine-grained attribute queries; going larger doesn't fix it. Out of scope for v0.

## Alternatives considered

- **Stay on base (ADR-0004).** Rejected: its only justification was CPU latency, now moot with the GPU.
- **SigLIP 2 large (1024-dim).** Rejected again, consistent with ADR-0004: smaller quality gain than so400m at similar GPU inference cost.
- **jina-clip-v2 / EVA-CLIP.** Viable and multilingual, but heavier and more DIY for a clean ONNX export. so400m is the lower-risk, socially-proven pick. Revisit if the eval disappoints.

## References

- ADR-0004 (supersedes it) — already named so400m as the upgrade path.
- SigLIP 2: https://huggingface.co/blog/siglip2
- so400m ONNX export: https://huggingface.co/onnx-community/siglip2-so400m-patch14-384-ONNX
- Immich model list: https://huggingface.co/immich-app/ViT-SO400M-16-SigLIP2-384__webli

## Amendment: the eval ran — `base` stays the default (2026-08-22)

The measurement this ADR required before flipping any default now exists.
`eidetic eval` on the first 1000 images of the COCO Karpathy test split
(5001 captions, text→image retrieval, both models on the CUDA EP):

| metric | base 256px/768d | so400m 384px/1152d | gain |
|---|---|---|---|
| Recall@1  | 68.61% | 70.53% | +1.9 pts |
| Recall@5  | 89.80% | 91.10% | +1.3 pts |
| Recall@10 | 94.54% | 95.20% | +0.7 pts |
| MRR       | 0.7802 | 0.7953 | +0.015 |

(Absolute numbers run higher than the paper's full-5K figures because a 1K
gallery has 5× fewer distractors; the *relative* comparison is the decision
input, and both models saw the identical subset.)

**Decision: `base` remains the default.** A ~2-point Recall@1 gain does not
pay for a 4.2 GB model download (vs 1.4 GB), ~3.5× the per-image compute, a
1.5× wider vector (storage + scan cost on every search), and a mandatory
full-library re-embed on switch. so400m stays exactly what it is today: the
`EIDETIC_MODEL=so400m` opt-in for someone who wants the last two points and
accepts the cost.
