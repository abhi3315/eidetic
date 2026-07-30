# ADR-0009: Distribution = self-contained per-OS archives (no server)

Date: 2026-07-23
Status: accepted

## Context

Eidetic is moving from a Mac-only, Docker-backed setup to something a user can download and run on Linux, Windows, or macOS. The shorthand goal was "a single binary that works on different OSes." Two corrections to that framing:

1. **One binary can't run on multiple OSes.** The deliverable is one self-contained artifact *per* platform, produced by a CI matrix.
2. **"Single file" isn't literally achievable here, and that's fine.** `ort` ships `libonnxruntime` as a shared library beside the executable (Microsoft discourages static-linking it), so even the CPU build is an archive, not one file. The honest goal is **"no server process, no Docker, no manual setup"** — which ADR-0005 (SQLite) already delivers on the data side.

## Decision

Ship **self-contained per-OS archives** built by a CI release matrix using **cargo-dist** ("dist").

- Targets: `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`.
- Each archive bundles the eidetic binary + the ONNX Runtime shared lib + license files.
- **Two Linux/Windows flavours:**
  - **CPU archive (default):** fully portable, no GPU deps. The recommended download.
  - **`cuda` archive:** documented as requiring CUDA 13 runtime + cuDNN 9 (per ADR-0006). Not self-contained — NVIDIA restricts redistribution of those libs.
- The SigLIP model (~1–4 GB depending on variant, ADR-0007) downloads at first run into the model cache — never bundled.
- Linux target is glibc, not musl: `ort`'s prebuilt ONNX Runtime is glibc-based, so musl static builds don't apply.

## Consequences

**Good:**

- `curl`-and-run per OS; no Docker, no Postgres, no connection string (ADR-0005).
- cargo-dist (actively maintained as of 2026) generates the matrix, checksums, and install scripts.
- Clear split between the portable CPU download and the GPU-power-user `cuda` download.

**Bad / accepted:**

- Not literally one file — an archive (binary + `libonnxruntime`). Acceptable; documented plainly.
- The `cuda` archive depends on a correct host CUDA/cuDNN install; inherent to GPU ML, can't be shipped around.
- First run needs network for the model download. One-time.
- CI currently builds Linux/CPU only; the matrix (macOS runner, Windows MSVC, a GPU smoke test) is new work.

## Alternatives considered

- **Static single binary (musl + static ORT).** Rejected: `ort` prebuilts are dynamic + glibc; Microsoft discourages static-linking ONNX Runtime. Not worth fighting for a marginal "one file."
- **Docker image as the distribution unit.** Rejected: Docker is precisely the setup step we're removing; `goals.md` treats server/cloud dependence as an anti-goal.
- **Bundle CUDA/cuDNN into the `cuda` archive.** Rejected: NVIDIA license restricts redistribution; document the requirement instead.

## References

- ADR-0005 (SQLite — removes the DB server), ADR-0006 (CUDA runtime requirement).
- cargo-dist: https://github.com/axodotdev/cargo-dist
