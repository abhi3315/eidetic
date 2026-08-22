# ADR-0006: GPU acceleration — CUDA EP + pinned ONNX Runtime ≥1.27

Date: 2026-07-23
Status: accepted (extends ADR-0003)

## Context

ADR-0003 chose `ort` (ONNX Runtime) and noted "CPU + CUDA + CoreML backends... GPU acceleration for free when available." Eidetic was developed on an M1 Pro Mac, where CoreML measured *slower* than CPU for SigLIP 2, so it shipped CPU-only in practice.

The hardware changed: development moved to a PC with an **NVIDIA RTX 5070 Ti** (Blackwell, compute capability **sm_120**, 16 GB), on Linux (and Windows). This is the first time real GPU acceleration is on the table — but "just enable the `cuda` feature" turned out to be a trap.

`ort` 2.0.0-rc.12 bundles ONNX Runtime **1.23.x**, whose official CUDA binaries only ship kernels up to **sm_89/sm_90** (Ada/Hopper). sm_120 is not covered — `download-binaries` + `cuda` on this card PTX-JIT-fails or silently falls back to CPU (confirmed: onnxruntime issues #26177, #26245). The whole GPU premise rested on an unverified assumption, so we verified it.

## Decision

Add a CUDA execution provider, but **pin the ONNX Runtime version explicitly and link it via the `system` strategy — not `download-binaries`.**

- Extend the `Mode` enum in `eidetic-ml` with `Cuda`; accept `EIDETIC_ACCELERATOR=cuda`; register the CUDA EP on non-macOS in `build_session`. CoreML stays `#[cfg(target_os = "macos")]`-gated; CPU remains the universal fallback.
- **Require ONNX Runtime ≥ 1.27 built for CUDA 13**, provided at build/run time, with CUDA 13 runtime + cuDNN 9 present.
- TensorRT EP is available in the same build as an alternative; DirectML is the vendor-agnostic option on Windows.

**Verification (2026-07-23, on the actual card):** ONNX Runtime 1.27.0 (CUDA-13 build) runs on the 5070 Ti — a chained-MatMul microbenchmark measured **8.2× faster than CPU** (11.2 ms → 1.4 ms), `CUDAExecutionProvider` active, `TensorrtExecutionProvider` also available. So sm_120 works; the constraint was purely the ORT version `ort` bundles. This was proven via Python `onnxruntime-gpu`.

**Rust V-gate closed (2026-07-30, on the actual card):** the Rust path works end-to-end. Built `eidetic-ml --features cuda`, pointed `ORT_DYLIB_PATH` at ONNX Runtime 1.27.0 **gpu_cuda12** (this host's runtime is CUDA 12) with cuDNN 9 on `LD_LIBRARY_PATH`, and ran the real SigLIP embed test with the CUDA EP registered via `.error_on_failure()` — pass therefore proves genuine CUDA registration, not a silent CPU fallback. Negative test: removing cuDNN from the loader path makes the same request fail loudly with `libcudnn.so.9: cannot open shared object file`. Note both ORT GPU flavours work on this card (cuda12 build under Rust, cuda13 build under Python — the driver supports both). **Still open before flipping any default:** the throughput number via `eidetic eval` (single-image latency is startup-dominated and meaningless as a benchmark).

## Consequences

**Good:**

- Real GPU acceleration is confirmed on this hardware (~8× on a microbench; SigLIP, transformer-heavy, should benefit similarly or more).
- Unlocks defaulting to a larger, more accurate model (ADR-0007) without a latency penalty.
- The EP abstraction already exists; this is additive, not a rewrite.

**Bad / accepted:**

- We give up `ort`'s convenient `download-binaries` for GPU builds and own an explicit ORT version pin. The bundled binary is simply too old for Blackwell.
- The CUDA build is **not self-contained**: it needs CUDA 13 runtime + cuDNN 9 on the machine (NVIDIA restricts redistribution). Reflected in ADR-0009 as a separate `cuda` archive.
- The dev box is bleeding-edge (Fedora 44: gcc 16 rejects the CUDA 12.9 `nvcc`; Python 3.14 has no CUDA wheels yet). Toolchains must be pinned deliberately; expect environment friction.
- Rust-path verification is still open — only the Python path is proven.

## Alternatives considered

- **`ort` `download-binaries` + `cuda` (the naive plan).** Rejected: bundled ORT 1.23.x has no sm_120 kernels — silent CPU fallback or PTX-JIT crash.
- **TensorRT EP as primary.** Kept as an option, but CUDA EP is verified on 1.27 and is simpler; TensorRT adds engine-build latency and its own pins.
- **DirectML (Windows).** Vendor-agnostic, sidesteps sm_120 — the fallback for the Windows build if CUDA setup fights us.
- **Stay CPU-only.** Rejected: the dedicated GPU is the point of the hardware move, and it unlocks ADR-0007.

## References

- ADR-0003 (ml runtime = `ort`) — this ADR extends it.
- ONNX Runtime Blackwell/sm_120 reports: https://github.com/microsoft/onnxruntime/issues/26177 , https://github.com/microsoft/onnxruntime/issues/26245
- ort cargo features / linking: https://ort.pyke.io/setup/cargo-features
- Verified stack recorded in project memory (`eidetic-ort-blackwell-constraint`).

## Amendment: Blackwell V-gate verified closed (2026-08-22)

The runtime gate this ADR left open — "does an out-of-tree ONNX Runtime drive
sm_120 through ort's load-dynamic path?" — has been verified on the real
hardware. ONNX Runtime **1.29.0** (`onnxruntime-linux-x64-gpu_cuda12`) plus
**cuDNN 9.25** via `ORT_DYLIB_PATH`/`LD_LIBRARY_PATH` registers the CUDA EP
and executes SigLIP 2 on an RTX 5070 Ti: embedding CPU time drops ~22×
(133 s → 6 s user time on an 80-asset library) and search rankings are
identical to the CPU path. The working recipe is in the README ("Running the
GPU build").

One trap found and fixed on the way: with `load-dynamic` active and
`ORT_DYLIB_PATH` unset, ort falls back to its `download-binaries` feature and
silently fetches a CPU-only runtime from the network at first session build —
observed as a multi-minute stall with no output. `build_session` now refuses
to run a cuda-feature binary without `ORT_DYLIB_PATH`, with instructions in
the error.
