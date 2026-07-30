# ADR-0009: Distribution = self-contained per-OS archives (no server)

Date: 2026-07-23
Status: accepted

## Context

Eidetic is moving from a Mac-only, Docker-backed setup to something a user can download and run on Linux, Windows, or macOS. The shorthand goal was "a single binary that works on different OSes." Two corrections to that framing:

1. **One binary can't run on multiple OSes.** The deliverable is one self-contained artifact *per* platform, produced by a CI matrix.
2. **"Single file" is closer than expected.** This ADR originally assumed `ort` ships `libonnxruntime` as a shared library beside the executable. **That was wrong** — measured on a real build (2026-07-30), `ort`'s `download-binaries` feature *statically links* ONNX Runtime: `ldd` on the produced binary shows no `libonnxruntime`, and ~19.6k ORT symbols are inside the executable. Combined with the libheif feature gate (ADR-0008), a `--no-default-features` Linux build has no non-system dynamic dependencies except OpenSSL. So the CPU artifact really is a single executable; the archive exists only to carry licences and a build-info note.

## Decision

Ship **self-contained per-OS archives** built by a CI release matrix using **cargo-dist** ("dist").

- Targets: `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`.
- Each archive carries the eidetic binary (ONNX Runtime already inside it), licences, and a `BUILD-INFO.txt` recording whether HEIC is compiled in and what the build needs at runtime.
- **Windows ships `--no-default-features`** (no HEIC): libheif on MSVC needs a vcpkg toolchain. This is precisely the case the ADR-0008 feature gate exists for. Revisit if Windows HEIC becomes important.
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

- The Linux binary still links OpenSSL (`libssl`/`libcrypto`), pulled in by `hf-hub`'s default `native-tls` for model downloads. Switching `hf-hub` to `rustls-tls` would likely remove the last non-system dynamic dependency; not done yet because it needs an end-to-end model-download test to verify. Tracked as a follow-up.
- Windows releases have no HEIC support until a vcpkg libheif build is added.
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
