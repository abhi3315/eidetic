# ADR-0009: Distribution = self-contained per-OS archives (no server)

Date: 2026-07-23
Status: accepted

## Context

Eidetic is moving from a Mac-only, Docker-backed setup to something a user can download and run on Linux, Windows, or macOS. The shorthand goal was "a single binary that works on different OSes." Two corrections to that framing:

1. **One binary can't run on multiple OSes.** The deliverable is one self-contained artifact *per* platform, produced by a CI matrix.
2. **"Single file" is closer than expected.** This ADR originally assumed `ort` ships `libonnxruntime` as a shared library beside the executable. **That was wrong** — measured on a real build (2026-07-30), `ort`'s `download-binaries` feature *statically links* ONNX Runtime: `ldd` on the produced binary shows no `libonnxruntime`, and ~19.6k ORT symbols are inside the executable. Combined with the libheif feature gate (ADR-0008), a `--no-default-features` Linux build has no non-system dynamic dependencies except OpenSSL. So the CPU artifact really is a single executable; the archive exists only to carry licences and a build-info note.

## Decision

Ship **self-contained per-OS archives** built by a CI release matrix using **cargo-dist** ("dist"). *(Amendment 2026-09-02: shipped as a hand-written matrix instead — see below.)*

- Targets: `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`.
- Each archive carries the eidetic binary (ONNX Runtime already inside it), licences, and a `BUILD-INFO.txt` recording whether HEIC is compiled in and what the build needs at runtime.
- **Windows ships `--no-default-features`** (no HEIC): libheif on MSVC needs a vcpkg toolchain. This is precisely the case the ADR-0008 feature gate exists for. Revisit if Windows HEIC becomes important.
- **Two Linux/Windows flavours:**
  - **CPU archive (default):** no GPU deps. The recommended download. **Requires libheif on the host** — see the amendment below.
  - **`cuda` archive:** documented as requiring CUDA 13 runtime + cuDNN 9 (per ADR-0006). Not self-contained — NVIDIA restricts redistribution of those libs.
- The SigLIP model (~1–4 GB depending on variant, ADR-0007) downloads at first run into the model cache — never bundled.
- Linux target is glibc, not musl: `ort`'s prebuilt ONNX Runtime is glibc-based, so musl static builds don't apply.

## Consequences

**Good:**

- `curl`-and-run per OS; no Docker, no Postgres, no connection string (ADR-0005).
- ~~cargo-dist (actively maintained as of 2026) generates the matrix, checksums, and install scripts.~~ *(Superseded — see amendment.)*
- Clear split between the portable CPU download and the GPU-power-user `cuda` download.

**Bad / accepted:**

- The Linux binary still links OpenSSL (`libssl`/`libcrypto`), pulled in by `hf-hub`'s default `native-tls` for model downloads. Switching `hf-hub` to `rustls-tls` would likely remove the last non-system dynamic dependency; not done yet because it needs an end-to-end model-download test to verify. Tracked as a follow-up.
- Windows releases have no HEIC support until a vcpkg libheif build is added.
- The `cuda` archive depends on a correct host CUDA/cuDNN install; inherent to GPU ML, can't be shipped around.
- First run needs network for the model download. One-time.
- CI currently builds Linux/CPU only; the matrix (macOS runner, Windows MSVC, a GPU smoke test) is new work.

## Amendment (2026-09-02): what actually shipped differs in three ways

1. **No cargo-dist.** The release workflow is a hand-written matrix
   (`release.yml` says why: it is small enough to read at a glance and needs
   per-target feature differences that are awkward in dist's config).
   Checksums are emitted directly; there are no generated install scripts.
2. **Fewer flavours than promised.** "Two Linux/Windows flavours" shipped as
   Linux CPU + Linux CUDA + macOS CPU + one Windows `--no-default-features`
   artifact. A Windows CUDA archive does not exist.
3. **The portability claim was wrong** — the rest of this amendment.

## Amendment (2026-09-02): the CPU archive is not fully portable

The "fully portable" claim above described a `--no-default-features` build — the one the Context section measured. The release matrix that shipped does the opposite: Linux and macOS CPU archives build with default features, which means `heic`, which means the binary **dynamically links `libheif.so.1`** and the archive does not carry it.

The consequence is worse than "HEIC photos don't open." libheif is resolved by the loader at process start, so on a host without it *every* command fails before `main` runs — `eidetic --version` included. `README.md`'s description of this failure as happening "when a HEIC is encountered" was wrong for the same reason.

Considered and rejected:

- **Drop HEIC from the default archive** (portable default, separate HEIC archive). Rejected: it makes the recommended download the one that cannot read iPhone photos, on a personal photo library, to preserve a sentence that could simply be corrected.
- **Bundle libheif and its plugins into the archive.** Rejected: vendoring a media parser means owning its CVE stream on a solo project, relocating libheif's runtime plugin search for x265/de265, and attaching LGPL obligations to the artifact — a permanent cost to save users one package install.

Decided: keep HEIC in the default archive and state the prerequisite honestly, in `BUILD-INFO.txt`, the README, and here. Every staged archive is now extracted and launched in CI (`eidetic --version`), so an archive that cannot start can no longer be published — that gate is the durable half of this fix and is independent of which option was chosen.

The lazily-loaded HEIC decoder (`dlopen` on first HEIC rather than link-time binding) is the shape that would make the README's original claim true, and would let the archive be both portable and HEIC-capable. It is real work, comparable to bundling, and is deferred rather than rejected.

## Alternatives considered

- **Static single binary (musl + static ORT).** Rejected: `ort` prebuilts are dynamic + glibc; Microsoft discourages static-linking ONNX Runtime. Not worth fighting for a marginal "one file."
- **Docker image as the distribution unit.** Rejected: Docker is precisely the setup step we're removing; `goals.md` treats server/cloud dependence as an anti-goal.
- **Bundle CUDA/cuDNN into the `cuda` archive.** Rejected: NVIDIA license restricts redistribution; document the requirement instead.

## References

- ADR-0005 (SQLite — removes the DB server), ADR-0006 (CUDA runtime requirement).
- cargo-dist: https://github.com/axodotdev/cargo-dist
