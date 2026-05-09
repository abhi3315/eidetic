# CoreML Acceleration for SigLIP 2 Inference

**Date:** 2026-05-09
**Status:** Approved, ready for plan

## Goal

Run SigLIP 2 ONNX inference on Apple Silicon's GPU + Neural Engine via the
CoreML execution provider, with an env-var escape hatch back to CPU.

## Motivation

Today both `vision_session` and `text_session` execute on the ONNX Runtime
CPU EP. Measured impact:

- Base model (siglip2-base-256), full COCO 5K eval: 35 min wall, ~3 cores busy.
- Large model (siglip2-large-384), full COCO 5K eval: 187 min wall.

CPU is the bottleneck. Apple's GPU and ANE are present and idle. CoreML is
the standard ort path to use them on macOS.

## Non-goals

- Other accelerators (CUDA, DirectML, ROCm, OpenVINO).
- Quantized model variants (`*_fp16.onnx`, `*_q4.onnx`, etc.).
- Converting models to native CoreML format. We use the existing ONNX exports
  and let ort's CoreML EP partition the graph.
- Per-session EP selection (i.e. CoreML for vision, CPU for text). The
  CoreML EP has automatic per-op CPU fallback; we let it decide.

## Behavior

A new env var `EIDETIC_ACCELERATOR` controls EP selection:

| Value | macOS | Linux/Windows |
|---|---|---|
| _unset_ (default) | CoreML EP + CPU fallback | CPU EP |
| `coreml` | CoreML EP + CPU fallback | Error: "CoreML not available on this platform" |
| `cpu` | CPU EP only | CPU EP only |

Logged at INFO on session creation: which EP was selected, which CoreML
compute units (`All` = CPU+GPU+ANE), and the model file being loaded.

Existing `info,ort=warn` log filter (committed earlier) already suppresses
ort's verbose graph-partition output. No change needed.

## Implementation

### Cargo

`Cargo.toml` (workspace) adds a target-gated feature on `ort`:

```toml
[workspace.dependencies]
ort = { version = "2.0.0-rc.12", features = ["download-binaries", "ndarray"] }

# macOS picks up the CoreML EP additionally.
[target.'cfg(target_os = "macos")'.dependencies]
ort = { version = "2.0.0-rc.12", features = ["coreml"] }
```

The `[target.'cfg(...)'.dependencies]` form merges feature flags rather than
replacing — ort keeps `download-binaries` and `ndarray` and adds `coreml`
on Mac.

### Code

`crates/eidetic-ml/src/siglip.rs` gets a `build_session` helper:

```rust
fn build_session(model_path: &Path) -> Result<Session> {
    let accelerator = std::env::var("EIDETIC_ACCELERATOR").ok();
    let mut builder = Session::builder().map_err(...)?;

    #[cfg(target_os = "macos")]
    {
        let use_coreml = accelerator.as_deref().is_none()
            || accelerator.as_deref() == Some("coreml");
        if use_coreml {
            tracing::info!(
                "Registering CoreML EP (compute_units=All) + CPU fallback"
            );
            builder = builder.with_execution_providers([
                CoreMLExecutionProvider::default()
                    .with_compute_units(CoreMLComputeUnits::All)
                    .build(),
                CPUExecutionProvider::default().build(),
            ]).map_err(...)?;
        } else {
            tracing::info!("Using CPU EP (EIDETIC_ACCELERATOR={})", "cpu");
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if accelerator.as_deref() == Some("coreml") {
            return Err(Error::ModelLoad(
                "EIDETIC_ACCELERATOR=coreml requested on non-macOS host".into()
            ));
        }
    }

    builder.commit_from_file(model_path)
        .map_err(|e| Error::ModelLoad(format!("{}: {e}", model_path.display())))
}
```

`SiglipEmbedder::load` calls `build_session(&vision_path)` and
`build_session(&text_path)` in place of the current inline `Session::builder`
chains.

### Validation

Add to the eval workflow, not as test code — the proof comes from the
benchmark numbers:

1. Re-run base eval with `EIDETIC_ACCELERATOR=cpu` to confirm baseline still
   matches the 49.71% R@1 we already measured.
2. Re-run base eval with default (CoreML). Two pass criteria:
   - **Accuracy:** Recall@1 within ±0.5% of the CPU baseline (49.21–50.21%).
     If outside this band, CoreML is producing materially different numerics
     and we investigate which op partitions to CPU.
   - **Speed:** wall time under 15 min (vs 35 min CPU baseline). Target is
     5-10 min, but anything under 15 means CoreML is doing real work.

3. If both pass, record numbers in commit message and move on.

## File changes

- `Cargo.toml` — target-gated ort feature.
- `crates/eidetic-ml/src/siglip.rs` — `build_session` helper, wired into
  both vision and text session creation, with the macOS/non-macOS cfg gates.

No DB schema, no CLI, no API surface change.

## Risks

- **CoreML produces different numerics for some op.** Mitigated by the
  per-op CPU fallback being the default behavior — ort only sends ops to
  CoreML if it can compile them. The bigger risk is op-level numerical
  drift (small differences in, e.g., layer norm or attention) that the eval
  catches.
- **CoreML compilation cost on first run.** ort caches compiled CoreML
  models alongside the ONNX file. First load slower; subsequent fast.
  Acceptable.
- **ANE size limit.** SigLIP 2 large's 1.3 GB vision model may not fit on
  ANE — `ComputeUnits::All` lets CoreML use GPU instead. Worst case the EP
  refuses the model and we fall back to CPU silently. Eval would surface
  this as "no speedup," which we'd then debug.

## Out of scope (separate work)

- Cross-platform GPU support (CUDA, DirectML, etc.).
- Quantized variants.
- Re-running the large eval with CoreML — interesting but a separate
  measurement, not part of this change's validation.
