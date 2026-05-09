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
| _unset_ (default) | CoreML EP (with built-in CPU fallback per op) | CPU EP |
| `coreml` | CoreML EP (with built-in CPU fallback per op) | Error: "CoreML not available on this platform" |
| `cpu` | CPU EP only | CPU EP only |
| any other value | Error: "Unknown EIDETIC_ACCELERATOR=`<value>`" | Same |

Unknown values error explicitly rather than silently selecting CPU. Catches
typos like `EIDETIC_ACCELERATOR=metal` instead of letting them quietly run
on CPU and look like CoreML didn't help.

Logged at INFO on session creation: which EP was selected, which CoreML
compute units (`All` = CPU+GPU+ANE), and the model file being loaded.

Existing `info,ort=warn` log filter (committed earlier) already suppresses
ort's verbose graph-partition output. No change needed.

## Implementation

### Cargo

The change goes in **`crates/eidetic-ml/Cargo.toml`** (the consuming package),
not the workspace root manifest. `[target.*.dependencies]` is a package-level
key — putting it in `eidetic/Cargo.toml` (a virtual workspace manifest with
no `[package]`) is silently ineffective.

`crates/eidetic-ml/Cargo.toml` already declares `ort = { workspace = true }`.
Add a target-gated companion line:

```toml
[dependencies]
ort = { workspace = true }
# … other deps …

[target.'cfg(target_os = "macos")'.dependencies]
ort = { workspace = true, features = ["coreml"] }
```

Cargo's feature unification merges the two `ort` declarations on macOS:
the workspace settings (`download-binaries`, `ndarray`) plus `coreml`. On
Linux/Windows the second line resolves to nothing and the workspace defaults
apply unchanged.

The workspace `Cargo.toml` does *not* change.

### Code

`crates/eidetic-ml/src/siglip.rs` gets a `build_session` helper. Sketch
below shows the control flow; the implementation step pins exact ort 2.0
type/method names against rc.12 and preserves the existing
`|e: ort::Error| Error::ModelLoad(e.to_string())` `map_err` pattern.

```rust
// Use statements (cfg-gated; exact module paths verified in implementation
// against ort 2.0.0-rc.12 — past rc versions have shuffled this surface).
#[cfg(target_os = "macos")]
use ort::execution_providers::{CoreMLExecutionProvider /*, CoreMLComputeUnits */};

fn build_session(model_path: &Path) -> Result<Session> {
    let accel = std::env::var("EIDETIC_ACCELERATOR").ok();
    let accel_ref = accel.as_deref();

    // Validate the env var the same way on every platform so a typo errors
    // loudly instead of silently picking CPU.
    let mode = match accel_ref {
        None => Mode::Default,
        Some("coreml") => Mode::CoreML,
        Some("cpu") => Mode::Cpu,
        Some(other) => return Err(Error::ModelLoad(
            format!("Unknown EIDETIC_ACCELERATOR={other:?}; \
                     valid values: coreml, cpu")
        )),
    };

    let mut builder = Session::builder()…?;

    #[cfg(target_os = "macos")]
    if matches!(mode, Mode::Default | Mode::CoreML) {
        tracing::info!(accelerator = "coreml", "registering CoreML EP");
        builder = builder.with_execution_providers([
            CoreMLExecutionProvider::default()
                .with_compute_units(CoreMLComputeUnits::All)
                .build(),
            // No explicit CPUExecutionProvider — ort auto-falls-back per op
            // to its built-in CPU implementation when an op isn't supported.
        ])…?;
    } else {
        tracing::info!(accelerator = ?accel_ref.unwrap_or("cpu"), "using CPU EP");
    }

    #[cfg(not(target_os = "macos"))]
    if matches!(mode, Mode::CoreML) {
        return Err(Error::ModelLoad(
            "EIDETIC_ACCELERATOR=coreml requested on non-macOS host".into()
        ));
    }
    // On non-macOS, Default and Cpu both mean CPU EP; no registration needed.

    builder.commit_from_file(model_path)
        .map_err(|e| Error::ModelLoad(format!("{}: {e}", model_path.display())))
}

enum Mode { Default, CoreML, Cpu }
```

Key points:

- **Unknown env-var values error explicitly.** Typo-friendly fail-loud
  behavior; documented in the Behavior table.
- **No redundant CPU EP registration.** ort's CoreML EP automatically
  delegates unsupported ops to its built-in CPU implementation; explicitly
  registering `CPUExecutionProvider` after CoreML is noise.
- **Logging uses tracing structured fields.** `accelerator = "coreml"` is
  filterable downstream; the CPU branch echoes the user-set value so a
  weird-but-valid request is visible in logs.
- **Error messages drop the "vision model:" / "text model:" labels** in
  favor of the full path. The path already contains `vision_model.onnx` or
  `text_model.onnx`, so origin is still obvious — and the path is more
  informative when debugging cache/permission issues. Deliberate change,
  not an oversight.

`SiglipEmbedder::load` calls `build_session(&vision_path)` and
`build_session(&text_path)` in place of the current inline `Session::builder`
chains. Both vision and text get the same EP — ort's per-op fallback handles
any case where one model isn't accelerator-friendly.

### Validation

Proof comes from the existing COCO eval, not unit tests. Reproducible
commands assume the dataset already lives at the paths we used in the
earlier base/large runs.

**Baseline (CPU EP):**
```bash
EIDETIC_ACCELERATOR=cpu /usr/bin/time -p ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test
```
Expected: Recall@1 = 49.71% (matches the value already on record from
commit `4f1d6dd`'s follow-up run on 2026-05-09). `real` time ≈ 35 min.

**CoreML run (default):**
```bash
/usr/bin/time -p ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test
```

Pass criteria:

- **Accuracy:** Recall@1 within ±0.5% of the CPU baseline (49.21–50.21%).
  Outside this band means CoreML is producing materially different numerics
  on some op; we investigate which op partitions to CPU and either accept
  the trade or set narrower compute units.
- **Speed:** `real` time under 15 min (vs ~35 min CPU baseline). Target is
  5-10 min; anything under 15 means CoreML is doing real work.

If both pass, record both numbers in the implementation's final commit
message and stop. No further validation needed.

## File changes

- `crates/eidetic-ml/Cargo.toml` — target-gated `ort` feature line for macOS.
- `crates/eidetic-ml/src/siglip.rs` — `build_session` helper, wired into
  both vision and text session creation, with the macOS/non-macOS cfg gates
  and exhaustive env-var validation.

Workspace `Cargo.toml` is **not** modified. No DB schema, no CLI, no public
API surface change.

## Risks

- **CoreML produces different numerics for some op.** Mitigated by the
  per-op CPU fallback being the default behavior — ort only sends ops to
  CoreML if it can compile them. The bigger risk is op-level numerical
  drift (small differences in, e.g., layer norm or attention) that the eval
  catches.
- **CoreML compilation cost on first run.** ort's CoreML EP compiles the
  ONNX graph (or a CoreML-supported subgraph) on first session creation.
  First load is slower; subsequent loads use the compiled artifact.
  *Open question for implementation:* the default cache path may write
  next to the `.onnx` file inside the hf-hub snapshot dir, which is not
  guaranteed writable. The implementation step verifies this and, if
  needed, points the cache at `EIDETIC_MODELS_CACHE` or a sibling dir.
- **ANE size limit.** SigLIP 2 large's 1.3 GB vision model may not fit on
  ANE — `ComputeUnits::All` lets CoreML use GPU instead. Worst case the EP
  refuses the model and we fall back to CPU silently. Eval would surface
  this as "no speedup," which we'd then debug.

## Out of scope (separate work)

- Cross-platform GPU support (CUDA, DirectML, etc.).
- Quantized variants.
- Re-running the large eval with CoreML — interesting but a separate
  measurement, not part of this change's validation.
