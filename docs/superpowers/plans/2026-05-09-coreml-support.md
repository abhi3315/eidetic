# CoreML Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire ort's CoreML execution provider into `SiglipEmbedder` on macOS so SigLIP 2 inference uses Apple's GPU + Neural Engine, with `EIDETIC_ACCELERATOR=cpu` as the opt-out and unknown values failing loudly.

**Architecture:** A `build_session` helper in `crates/eidetic-ml/src/siglip.rs` reads `EIDETIC_ACCELERATOR`, parses it into an exhaustive `Mode` enum, and registers the appropriate execution provider on the `Session::builder`. CoreML is feature-gated to macOS via `[target.'cfg(target_os = "macos")'.dependencies]` in `crates/eidetic-ml/Cargo.toml`. Validation is empirical via the existing `eidetic eval` COCO 5K harness — no new unit tests for inference itself; one unit test for env-var parsing.

**Tech Stack:** Rust 2024 edition, `ort` v2.0.0-rc.12 with `coreml` feature on macOS, `cfg(target_os = "macos")` gates, existing `tracing` for structured logging.

**Spec:** `docs/superpowers/specs/2026-05-09-coreml-support-design.md` (commit `560e773`).

---

## Task 1: Verify ort 2.0.0-rc.12 CoreML API surface

**Files:** none — research task.

The spec flagged that ort 2.0-rc has shifted CoreML EP types/methods across rc versions. Pin the exact symbols before writing code, so Task 3 doesn't compile-error and force a redo.

- [ ] **Step 1: Find the ort source in the local cargo registry**

```bash
find ~/.cargo/registry/src -maxdepth 3 -type d -name "ort-2.0.0-rc.12" 2>/dev/null
```

Expected: a single path printed.

- [ ] **Step 2: Locate the CoreML execution provider definition**

```bash
ORT_DIR=$(find ~/.cargo/registry/src -maxdepth 3 -type d -name "ort-2.0.0-rc.12" | head -1)
grep -rn "CoreMLExecutionProvider\|CoreMLComputeUnits\|coreml" "$ORT_DIR/src/execution_providers/" | head -30
```

Expected: list of files declaring `CoreMLExecutionProvider`, the compute-units enum (may be `CoreMLComputeUnits` or `CoreMLExecutionProviderComputeUnits` or similar), and a builder pattern.

- [ ] **Step 3: Record the exact public API to use**

Open the relevant source file (e.g. `$ORT_DIR/src/execution_providers/coreml.rs`) and note for use in Task 3:

1. Full module path of `CoreMLExecutionProvider` (e.g. `ort::execution_providers::CoreMLExecutionProvider`).
2. Builder method names actually present — `with_compute_units`, `with_ane_only`, `with_subgraphs`, `with_model_format`, etc. The spec assumes `with_compute_units(CoreMLComputeUnits::All)`; if the rc.12 API uses different names, use what's actually there.
3. The exact enum/value for "use CPU + GPU + ANE" (commonly `All`, but may be `CpuAndGpuAndAne` or split across multiple boolean toggles in some ort versions).
4. The terminator: is it `.build()` returning a registerable `ExecutionProviderDispatch`, or do you pass the builder directly to `with_execution_providers`?

- [ ] **Step 4: Confirm builder accepts a slice/array of EPs**

```bash
grep -n "with_execution_providers" "$ORT_DIR/src/session/builder.rs" | head -5
```

Expected: a method signature taking something `IntoIterator<Item = ExecutionProviderDispatch>` or equivalent. Confirm Task 3's `[CoreMLExecutionProvider::default().…build()]` call shape is valid.

- [ ] **Step 5: No commit — research only**

Move on to Task 2 with the API names recorded.

---

## Task 2: Target-gate the `coreml` ort feature on macOS

**Files:**
- Modify: `crates/eidetic-ml/Cargo.toml`

The spec is explicit: this goes in the consumer crate's manifest, not the workspace root. `[target.*.dependencies]` is a package-level key.

- [ ] **Step 1: Read the current state**

```bash
cat crates/eidetic-ml/Cargo.toml
```

Confirm there is currently a `[dependencies]` section with `ort = { workspace = true }` and **no** `[target.'cfg(target_os = "macos")'.dependencies]` block.

- [ ] **Step 2: Add the target-gated feature line**

Append to `crates/eidetic-ml/Cargo.toml` (after the existing `[dependencies]` block):

```toml

[target.'cfg(target_os = "macos")'.dependencies]
ort = { workspace = true, features = ["coreml"] }
```

The blank line before the new section header is required for TOML readability.

- [ ] **Step 3: Verify cargo accepts the manifest and resolves features correctly on macOS**

```bash
cargo tree -p eidetic-ml -e features --target aarch64-apple-darwin 2>&1 | grep -E "^├── ort|^│   ├── feature \"coreml\"|^│   ├── feature \"download-binaries\"|^│   ├── feature \"ndarray\"" | head -10
```

Expected: `ort` appears with all three features active: `coreml`, `download-binaries`, `ndarray`.

If the host is not aarch64-darwin (e.g. running on x86_64-darwin), substitute the appropriate triple — `x86_64-apple-darwin`. The point is to verify feature unification works on a macOS target.

- [ ] **Step 4: Build to confirm CoreML compiles**

```bash
cargo build --release -p eidetic-ml 2>&1 | tail -5
```

Expected: `Finished release` line, no errors. First build will recompile `ort` with the new feature, taking 1-2 minutes.

If it fails with "feature `coreml` does not exist" or similar, ort's feature was renamed in the rc — check `cargo doc -p ort --open` for the actual feature name, or grep the ort `Cargo.toml`:
```bash
grep -A1 "^\[features\]" "$ORT_DIR/Cargo.toml" | head -40
```

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ml/Cargo.toml Cargo.lock
git commit -m "build(ml): enable ort coreml feature on macOS

Adds [target.'cfg(target_os = \"macos\")'.dependencies] block to
crates/eidetic-ml/Cargo.toml so ort picks up the coreml execution
provider feature on Mac builds. Linux and Windows builds remain
unchanged (CPU EP only).

The change goes in the consumer crate's manifest, not the workspace
root — [target.*.dependencies] is a package-level key and would be
silently ignored in the virtual workspace manifest."
```

---

## Task 3: Add `Mode` enum and unit-test env-var parsing

**Files:**
- Modify: `crates/eidetic-ml/src/siglip.rs`

Build the `Mode` enum and `parse_accelerator` function before wiring them into session construction. This is the only piece of new logic that benefits from a unit test: env-var validation, exhaustively. The session builder itself is exercised by the eval.

- [ ] **Step 1: Add the `Mode` enum and parse function with #[cfg(test)] tests**

Open `crates/eidetic-ml/src/siglip.rs`. After the existing imports (around line 7) but before the constants block, add:

```rust
/// Which execution provider to register on a `Session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// User did not set `EIDETIC_ACCELERATOR`. Platform default applies:
    /// CoreML on macOS, CPU elsewhere.
    Default,
    /// Explicitly requested CoreML. Errors on non-macOS.
    CoreML,
    /// Explicitly requested CPU. Always valid.
    Cpu,
}

/// Parse the `EIDETIC_ACCELERATOR` env var. Unknown non-empty values are
/// rejected so a typo (`metal`, `cuda`, etc.) fails loudly rather than
/// quietly selecting CPU.
fn parse_accelerator(raw: Option<&str>) -> Result<Mode> {
    match raw {
        None => Ok(Mode::Default),
        Some("coreml") => Ok(Mode::CoreML),
        Some("cpu") => Ok(Mode::Cpu),
        Some(other) => Err(Error::ModelLoad(format!(
            "Unknown EIDETIC_ACCELERATOR={other:?}; valid values: coreml, cpu"
        ))),
    }
}
```

At the bottom of the file, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn parse_accelerator_unset_is_default() {
        assert_eq!(parse_accelerator(None).unwrap(), Mode::Default);
    }

    #[test]
    fn parse_accelerator_coreml_ok() {
        assert_eq!(parse_accelerator(Some("coreml")).unwrap(), Mode::CoreML);
    }

    #[test]
    fn parse_accelerator_cpu_ok() {
        assert_eq!(parse_accelerator(Some("cpu")).unwrap(), Mode::Cpu);
    }

    #[test]
    fn parse_accelerator_unknown_value_errors() {
        let err = parse_accelerator(Some("metal")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("metal"), "expected error to mention the bad value, got: {msg}");
        assert!(msg.contains("coreml"), "expected error to list valid values, got: {msg}");
        assert!(msg.contains("cpu"), "expected error to list valid values, got: {msg}");
    }

    #[test]
    fn parse_accelerator_empty_string_is_unknown() {
        // Empty string is "set but empty" — treat as a typo, not as unset.
        let err = parse_accelerator(Some("")).unwrap_err();
        assert!(err.to_string().contains("Unknown EIDETIC_ACCELERATOR"));
    }
```

If a `tests` module doesn't yet exist in `siglip.rs`, add one:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // … the parse_accelerator tests above …
}
```

- [ ] **Step 2: Run the tests to verify they all pass**

```bash
cargo test -p eidetic-ml siglip::tests::parse_accelerator -- --nocapture 2>&1 | tail -20
```

Expected: 5 tests pass (`parse_accelerator_unset_is_default`, `parse_accelerator_coreml_ok`, `parse_accelerator_cpu_ok`, `parse_accelerator_unknown_value_errors`, `parse_accelerator_empty_string_is_unknown`).

If `parse_accelerator_empty_string_is_unknown` fails because empty-string is being parsed as `None` somewhere upstream, that's fine — but here we're testing the function in isolation with explicit `Some("")`, so it should hit the `Some(other)` arm.

- [ ] **Step 3: Commit**

```bash
git add crates/eidetic-ml/src/siglip.rs
git commit -m "feat(ml): add Mode enum and EIDETIC_ACCELERATOR parser

Mode::{Default, CoreML, Cpu} captures the three valid accelerator
selections from the spec. parse_accelerator() rejects unknown
values explicitly so typos (\"metal\", \"cuda\", \"\") produce a
loud error listing the valid options instead of silently selecting
CPU.

Five unit tests cover the parse function exhaustively. Session
wiring lands in a follow-up commit."
```

---

## Task 4: Wire `build_session` into `SiglipEmbedder::load`

**Files:**
- Modify: `crates/eidetic-ml/src/siglip.rs`

Refactor the two inline `Session::builder()…commit_from_file(…)` chains into a single helper that consumes a `Mode` and registers the appropriate EP.

- [ ] **Step 1: Read the current `load()` body to know what's being replaced**

```bash
grep -n "Session::builder\|commit_from_file\|vision_session\|text_session" crates/eidetic-ml/src/siglip.rs
```

Expected: two `Session::builder()…commit_from_file()` chains in `load()`, around lines 80-90 (vision) and 90-100 (text). Both will be replaced with `build_session(&path, mode)?`.

- [ ] **Step 2: Add the cfg-gated `use` statements at the top of the file**

Below the existing `use` block (around line 6), add the macOS-only import:

```rust
#[cfg(target_os = "macos")]
use ort::execution_providers::{CoreMLExecutionProvider, /* compute-units type from Task 1 */};
```

Replace `/* compute-units type from Task 1 */` with the exact symbol you recorded in Task 1, Step 3 (e.g. `CoreMLComputeUnits` or whatever rc.12 actually exports).

- [ ] **Step 3: Add the `build_session` helper above `preprocess_image`**

After the existing `download` function (~line 175) and before `preprocess_image`:

```rust
/// Build an ort `Session` from a model file, registering the execution
/// provider implied by `mode`. CoreML is only registered on macOS — on
/// other targets, `Mode::Default` and `Mode::Cpu` both fall through to
/// the CPU EP, and `Mode::CoreML` errors.
fn build_session(model_path: &Path, mode: Mode) -> Result<Session> {
    let mut builder =
        Session::builder().map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?;

    #[cfg(target_os = "macos")]
    {
        if matches!(mode, Mode::Default | Mode::CoreML) {
            tracing::info!(
                accelerator = "coreml",
                model = %model_path.display(),
                "registering CoreML EP"
            );
            builder = builder
                .with_execution_providers([
                    // Adjust the builder method names below to whatever
                    // Task 1 recorded for ort 2.0.0-rc.12. Below assumes
                    // the spec's expected shape — replace as needed.
                    CoreMLExecutionProvider::default()
                        .with_compute_units(CoreMLComputeUnits::All)
                        .build(),
                ])
                .map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?;
        } else {
            tracing::info!(
                accelerator = "cpu",
                model = %model_path.display(),
                "using CPU EP"
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if matches!(mode, Mode::CoreML) {
            return Err(Error::ModelLoad(
                "EIDETIC_ACCELERATOR=coreml requested on non-macOS host".into(),
            ));
        }
        // Default and Cpu both mean CPU EP on non-macOS; no registration needed
        // because ort always builds in the CPU EP.
        let _ = mode; // silence unused warning when only Cpu/Default are reachable
        tracing::info!(
            accelerator = "cpu",
            model = %model_path.display(),
            "using CPU EP"
        );
    }

    builder
        .commit_from_file(model_path)
        .map_err(|e| Error::ModelLoad(format!("{}: {e}", model_path.display())))
}
```

The closure pattern `|e: ort::Error| Error::ModelLoad(e.to_string())` matches what already exists in `load()` — preserve it for consistency with the rest of the module's error handling.

- [ ] **Step 4: Replace the inline session construction in `load()`**

Find this block (currently around lines 80-95):

```rust
        let vision_session = Session::builder()
            .map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?
            .commit_from_file(&vision_path)
            .map_err(|e| Error::ModelLoad(format!("vision model: {e}")))?;

        let text_session = Session::builder()
            .map_err(|e: ort::Error| Error::ModelLoad(e.to_string()))?
            .commit_from_file(&text_path)
            .map_err(|e| Error::ModelLoad(format!("text model: {e}")))?;
```

Replace with:

```rust
        let mode = parse_accelerator(std::env::var("EIDETIC_ACCELERATOR").ok().as_deref())?;

        let vision_session = build_session(&vision_path, mode)?;
        let text_session = build_session(&text_path, mode)?;
```

`std::env::var` returns `Result<String, _>`; `.ok()` converts to `Option<String>`; `.as_deref()` borrows down to `Option<&str>` to match `parse_accelerator`'s signature.

- [ ] **Step 5: Build to confirm everything compiles**

```bash
cargo build --release -p eidetic-cli 2>&1 | tail -5
```

Expected: `Finished release`, no errors.

If errors mention undefined `CoreMLComputeUnits` or `with_compute_units`, the API names from Task 1 didn't match. Re-check the ort source and update the helper.

- [ ] **Step 6: Run the unit tests to verify the refactor didn't break parse_accelerator**

```bash
cargo test -p eidetic-ml 2>&1 | tail -10
```

Expected: all tests pass, including the five from Task 3 plus any pre-existing tests in the module.

- [ ] **Step 7: Smoke-test the binary loads the model successfully (non-blocking sanity)**

```bash
EIDETIC_ACCELERATOR=cpu ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test \
  --limit 5 2>&1 | tail -15
```

Expected: model loads (you'll see the "using CPU EP" tracing line for both vision and text sessions), eval runs to completion on 5 images, prints Recall metrics. Don't worry about the absolute numbers — 5 images is far too small to be meaningful. The point is the code path runs end to end.

Then verify CoreML mode also loads (no eval, just confirm the session builds):

```bash
./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test \
  --limit 2 2>&1 | tail -15
```

Expected: "registering CoreML EP" appears in the tracing output, eval completes on 2 images.

If CoreML registration errors (e.g. "ane unsupported on this device"), that's a real bug — investigate before moving on. If it succeeds but Recall@1 is wildly different on these tiny pools, that's noise; ignore until the full eval in Task 6.

- [ ] **Step 8: Commit**

```bash
git add crates/eidetic-ml/src/siglip.rs
git commit -m "feat(ml): wire CoreML EP into SigLIP session construction

Adds a build_session helper that registers the execution provider
implied by EIDETIC_ACCELERATOR. On macOS the default and explicit
'coreml' both register the CoreML EP with compute_units=All; 'cpu'
registers nothing (ort uses its built-in CPU EP). On non-macOS,
'coreml' errors and everything else falls back to CPU.

Both vision and text sessions go through the same helper — ort's
per-op CPU fallback handles cases where one model isn't entirely
accelerator-friendly. Error messages now include the model file
path; the previous 'vision model:' / 'text model:' labels are
encoded in the path itself."
```

---

## Task 5: Run CPU baseline eval (control)

**Files:** none — verification only.

The existing 49.71% R@1 baseline came from a binary built before this refactor. Re-run with the new code to confirm the CPU path still produces the same number. If it doesn't, the refactor introduced a regression and we stop before touching CoreML.

- [ ] **Step 1: Confirm the model cache is warm (so timing isn't dominated by download)**

```bash
ls -lh ~/.cache/eidetic/models/models--onnx-community--siglip2-base-patch16-256-ONNX/snapshots/*/onnx/*.onnx 2>&1
```

Expected: both `vision_model.onnx` and `text_model.onnx` are present, no fresh download needed.

- [ ] **Step 2: Run the CPU baseline eval**

```bash
EIDETIC_ACCELERATOR=cpu /usr/bin/time -p ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test \
  2>&1 | tee /tmp/eidetic-eval/coreml-task5-cpu-baseline.log
```

Expected wall time: ~30-40 minutes. Run in the foreground or background as preferred; if backgrounding, wait for completion before Step 3.

- [ ] **Step 3: Check the result**

```bash
tail -15 /tmp/eidetic-eval/coreml-task5-cpu-baseline.log
```

Expected:
- `Recall@1` is `49.71%` ± 0.1% (sampling variance is essentially zero — the eval is deterministic for a given binary, so a small drift indicates a real change).
- `Recall@5`, `Recall@10`, `MRR` match the previous baseline within the same tolerance.

If `Recall@1` differs from `49.71%` by more than ~0.1%, the refactor changed inference somewhere. Stop and investigate before Task 6 — likely culprit is the env-var parsing returning a different `Mode` than expected, or `build_session` constructing the builder differently than the original inline chain.

- [ ] **Step 4: No commit — verification only**

Move to Task 6 once the baseline matches.

---

## Task 6: Run CoreML eval (experiment) and verify pass criteria

**Files:** none — verification only.

This is the actual proof that CoreML works. Two pass criteria from the spec:

- **Accuracy:** Recall@1 within ±0.5% of the CPU baseline (49.21–50.21%).
- **Speed:** wall time under 15 min.

- [ ] **Step 1: Run the eval with CoreML (default, no env var)**

```bash
/usr/bin/time -p ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test \
  2>&1 | tee /tmp/eidetic-eval/coreml-task6-coreml.log
```

Expected first-run wall time: under 15 min, ideally 5-10 min. First run includes one-time CoreML graph compilation; that compilation overhead is part of the wall-time measurement, but for a 35→<15 min target it's still a clear win.

- [ ] **Step 2: Check accuracy**

```bash
grep -E "Recall@1|Recall@5|Recall@10|MRR" /tmp/eidetic-eval/coreml-task6-coreml.log
```

Pass: `Recall@1` between 49.21% and 50.21%.

If outside that band (typically lower):
- Inspect the log for "fallback to CPU" or "could not run on CoreML" messages — `EIDETIC_LOG=info,ort=info` re-runs ort at info level if the default `ort=warn` filter hides them.
- The likely cause is a specific op (often layer norm, softmax, or attention) producing different numerics on ANE/GPU. Decision point: either accept the new number (document the trade), narrow `compute_units` to skip ANE (typically the source of fp16 drift), or investigate the specific op via ort's graph partition logs.

If outside the band by more than 5%, something is broken (wrong EP registered, wrong compute units, broken commit_from_file path). Don't proceed — debug.

- [ ] **Step 3: Check speed**

```bash
grep -E "^real" /tmp/eidetic-eval/coreml-task6-coreml.log
```

Pass: `real` value is under `900` seconds (15 min).

If real time is greater than 15 min but accuracy passes, CoreML is doing some work but most ops are falling back to CPU. Worth investigating with ort's graph partition logs (rerun once with `EIDETIC_LOG=info,ort=info` and grep for "assigned to" or "compiled to CoreML"), but the change still isn't a regression — log the result and decide whether to accept or dig further.

- [ ] **Step 4: Confirm the second run is faster (cache warm)**

Quick re-run on a 100-image subset to see whether the compiled CoreML artifact is cached:

```bash
/usr/bin/time -p ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test \
  --limit 100 2>&1 | tail -5
```

Compare against a CPU-mode 100-image run:

```bash
EIDETIC_ACCELERATOR=cpu /usr/bin/time -p ./target/release/eidetic eval \
  --coco-csv /tmp/coco-5k.csv \
  --coco-images /tmp/eidetic-eval/images/images_mscoco_2014_5k_test \
  --limit 100 2>&1 | tail -5
```

The CoreML 100-image run should be materially faster than the CPU 100-image run. If it isn't, the compile cache is being rebuilt on every load — an optimization opportunity (point at a stable cache dir), but not blocking.

- [ ] **Step 5: No commit — verification only; numbers go in Task 7's commit**

---

## Task 7: Update README and commit final results

**Files:**
- Modify: `README.md`

The README mentions environment variables but doesn't document `EIDETIC_ACCELERATOR`. Add it.

- [ ] **Step 1: Find the env-var table in README.md**

```bash
grep -n "EIDETIC_DATABASE_URL\|EIDETIC_LIBRARY_DIR\|EIDETIC_LOG" README.md
```

Expected: a markdown table around lines 50-55.

- [ ] **Step 2: Add the new env var to the table**

Open `README.md` and find the row for `EIDETIC_LOG`. After it, add (preserving the existing table formatting — same column alignment):

```markdown
| `EIDETIC_ACCELERATOR` | _unset_ (CoreML on macOS, CPU elsewhere) | Inference EP: `coreml` or `cpu`. Mac default is CoreML; set `cpu` to force CPU |
| `EIDETIC_MODEL` | `base` | SigLIP 2 variant: `base` (768-dim, fast) or `large` (1024-dim, slower) |
```

`EIDETIC_MODEL` was added in commit `2438f2b` (variant switcher) and never made it to the README. Documenting both at once.

- [ ] **Step 3: Verify the table renders correctly**

```bash
grep -A 8 "EIDETIC_DATABASE_URL" README.md
```

Expected: cleanly formatted table with the four original rows plus the two new ones, each with three pipe-separated columns.

- [ ] **Step 4: Commit with the empirical results in the message**

Replace `<R@1 number>`, `<wall time>`, etc. with the actual numbers from Task 6's log:

```bash
git add README.md
git commit -m "docs: document EIDETIC_ACCELERATOR and EIDETIC_MODEL

CoreML acceleration now ships on macOS by default. README's env
var table picks up EIDETIC_ACCELERATOR (coreml|cpu) and the
previously undocumented EIDETIC_MODEL (base|large).

Empirical CoreML vs CPU on COCO 5K Karpathy split, base model:

  CPU baseline:    Recall@1 49.71%, real <CPU wall>
  CoreML default:  Recall@1 <CoreML R@1>%, real <CoreML wall>

Accuracy: <±x.xx>% from baseline (within the ±0.5% pass band).
Speedup: <ratio>x wall time."
```

- [ ] **Step 5: Final verification — show the branch's commits**

```bash
git log --oneline audit-fixes-2026-05-09 -10
```

Expected: a chain of commits ending with this README update, with the CoreML-related commits grouped together at the top.

---

## Self-review notes (kept for the executing engineer)

**Spec coverage:**
- Cargo target-gated feature → Task 2
- Mode enum + parse_accelerator → Task 3
- build_session helper, vision/text wiring → Task 4
- Behavior table including unknown-value error → Task 3 (parse), Task 4 (cfg gates)
- Logging at INFO with structured fields → Task 4 (build_session)
- CPU baseline eval (control) → Task 5
- CoreML eval (experiment) → Task 6
- Documentation → Task 7
- ort 2.0.0-rc.12 API verification → Task 1 (research)
- CoreML compile-cache vs hf-hub snapshot → Task 6, Step 4 (informal probe; spec listed as open question, this plan investigates empirically rather than restructuring caching prematurely)

**Things deliberately deferred:**
- Setting an explicit CoreML cache dir if the default writes inside hf-hub's snapshots. Plan probes for this in Task 6, Step 4. If it's fine (compiled artifact persists across runs), no work needed. If it's broken (recompiles every load), follow-up plan.
- Op-level numerical drift investigation. Only triggered if Task 6, Step 2 fails the accuracy band.
- Cross-platform GPU support. Out of scope per spec.
