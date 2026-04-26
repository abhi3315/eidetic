# ADR-0003: ML runtime = `ort` (ONNX Runtime)

Date: 2026-04-26
Status: accepted

## Context

Eidetic runs three model families:

- **Image-text embedding** for semantic search (SigLIP 2 — see ADR-0004)
- **Face detection** (SCRFD)
- **Face recognition embedding** (ArcFace)

Picking one inference runtime up front matters because each runtime locks in:

- The model format (ONNX vs Candle's safetensors vs PyTorch)
- The build-time dependency story (pure-Rust vs native lib)
- The set of models we can run without porting

The original planning docs (April 2025) called for **Candle for CLIP, `ort` for face models** — split runtime. STACK_AUDIT.md re-evaluated that in April 2026 and found a blocking issue: SigLIP 2 isn't implemented in `candle-transformers`. Verified by inspecting `candle-transformers/src/models` directly — no SigLIP 2, no SigLIP either, only CLIP.

So the choice is now: stay on CLIP (Candle works) or move to SigLIP 2 (Candle requires us to write the model ourselves).

## Decision

Use **`ort` (ONNX Runtime)** as the single ML runtime. All models — image embedding, face detection, face recognition — run through `ort` from ONNX format files.

Crate version: pin to a specific `ort = "2.0.0-rcN"` since ort 2.x is at release-candidate stage as of this ADR. Production-ready, but API-stability is not yet guaranteed across rc bumps.

## Consequences

**Good:**

- One runtime to learn, one set of preprocessing/tensor conventions, one model loading path. Less surface area in `eidetic-ml`.
- ONNX is the de facto interchange format. Any model HuggingFace publishes an ONNX export for is reachable. SigLIP 2 has official ONNX exports.
- ONNX Runtime is mature (years in production at scale), with CPU + CUDA + CoreML backends. We get GPU acceleration for free when available.
- The portfolio framing shifts from "I used Candle, the cool pure-Rust framework" to "I shipped real production-grade Rust services running ML via ONNX." The second framing is honestly stronger — ort is what you'd use at work.

**Bad / accepted:**

- Loss of the "single static Rust binary" property. ort dynamically links to the ONNX Runtime native library. Distribution gets slightly more involved (bundle the .so/.dylib, or rely on system install).
- ort 2.x being in RC means API churn between versions. Pin exactly, bump deliberately.
- ONNX preprocessing (image resize, normalize, batch) is on us — Candle would have handled some of this. We use the `image` + `fast_image_resize` crates plus manual tensor construction.

## Alternatives considered

- **Candle.** Original plan. Rejected because `candle-transformers` does not implement SigLIP 2 as of April 2026. Going Candle means either (a) staying on CLIP and accepting older/weaker embeddings, or (b) implementing SigLIP 2 in Candle ourselves — a multi-week side project with high maintenance burden. Neither is acceptable.
- **`tract`.** Pure-Rust ONNX runtime. Rejected: model coverage is narrower than ONNX Runtime, and CUDA support is missing.
- **`tflite-rs`.** Rejected: limits us to TFLite models, which excludes most of the ONNX-first model zoo we want to draw from.
- **Split runtime (Candle for CLIP, `ort` for faces).** This was the original plan. Rejected because it doubles preprocessing code, doubles model-loading code, doubles error types, and the SigLIP-2 issue means Candle is no longer earning its complexity cost.

## References

- STACK_AUDIT.md (2026-04-26) — "candle-transformers does not have SigLIP 2"
- ort: https://github.com/pykeio/ort
- ONNX Runtime: https://onnxruntime.ai/
