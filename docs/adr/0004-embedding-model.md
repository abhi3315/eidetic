# ADR-0004: Image embedding model = SigLIP 2 base (768-dim)

Date: 2026-04-26
Status: accepted

## Context

Image-text embeddings power Eidetic's semantic search. Choosing the model now pins:

- The vector column dimension in the schema (`embedding vector(N)`)
- The HNSW/VectorChord index configuration
- Preprocessing pipeline (input size, normalization)
- Storage cost per asset (768-dim float32 = 3 KB; 1152-dim = 4.5 KB)

Switching later means either widening the column (re-embedding all assets) or running two columns side-by-side during a migration window.

The original planning docs disagreed with themselves on this — `IMPLEMENTATION_PLAN.md` said 768-dim (ViT-L/14), while `Now can you compile...md` and `TODO.md` said 512-dim (ViT-B/32). STACK_AUDIT.md flagged this and recommended SigLIP 2.

## Decision

Use **SigLIP 2 base** as the image-text embedding model, producing **768-dimensional embeddings**.

- Model: `google/siglip2-base-patch16-256` (HuggingFace)
- Format: ONNX (loaded via `ort` per ADR-0003)
- Input: 256x256 RGB, normalized per the model card
- Output: L2-normalized 768-dim float32 vector
- Schema: `assets.embedding vector(768)`

## Consequences

**Good:**

- Outperforms CLIP at every scale on zero-shot classification, image-text retrieval, and dense feature transfer (verified: HuggingFace blog, "SigLIP 2 models outperform the older SigLIP ones at all model scales").
- Multilingual. Critical for queries mixing English with Hindi / regional languages — relevant for an Indian user's photo library.
- Same model family Immich offers in production (their model list includes `ViT-SO400M-16-SigLIP2-384__webli`).
- Base variant (86M params) is small enough for CPU inference at acceptable latency. GPU optional.
- 768-dim is comfortable for pgvector + VectorChord at the scales we care about (low millions).

**Bad / accepted:**

- Base is the smallest SigLIP 2 variant. Quality is meaningfully worse than `So400m` (1152-dim, 400M params). If quality is an issue we'll add a migration to swap models — see "Future migration path" below.
- The 256x256 input means we throw away resolution from typical phone photos. Acceptable for whole-image semantic search; would not be acceptable for detail-level queries (and we don't aim for those at v0).
- Re-embedding the entire library on a model change is unavoidable cost. Mitigation: run both models in parallel during the swap, switch the index when re-embedding completes.

## Future migration path (when we want to upgrade)

Adding SigLIP 2 So400m (1152-dim) later:

1. Add a new column: `ALTER TABLE assets ADD COLUMN embedding_v2 vector(1152);`
2. Background-job re-embed every asset.
3. Build a VectorChord index on the new column.
4. Switch the search query to use `embedding_v2`.
5. Drop the old column once stable.

Cost is the re-embed time (estimate ~1 hour per 100k images on CPU, faster on GPU). No downtime.

## Alternatives considered

- **CLIP ViT-B/32 (512-dim).** The original plan in some docs. Rejected: smaller model, narrower performance, English-only, three years older than SigLIP 2.
- **CLIP ViT-L/14 (768-dim).** Same dim as our choice, but Candle-supported. Rejected because of ADR-0003 (we're not using Candle), and SigLIP 2 base outperforms it on the relevant benchmarks.
- **SigLIP 2 So400m (1152-dim).** Better quality, ~5x more parameters. Rejected for v0 because the cost/benefit doesn't justify it for a personal library — base is sufficient. Listed as a deliberate future upgrade above.
- **SigLIP 2 large (1024-dim).** Awkward middle ground. Slower than base, smaller quality gain than So400m at similar inference cost. Skip.

## References

- SigLIP 2 paper / blog: https://huggingface.co/blog/siglip2
- Model card: https://huggingface.co/google/siglip2-base-patch16-256
- STACK_AUDIT.md (2026-04-26) — "SigLIP 2 outperforms CLIP and original SigLIP at every scale"
