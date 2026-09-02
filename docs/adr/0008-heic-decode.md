# ADR-0008: HEIC/HEIF decode = libheif default, pure-Rust opt-in

Date: 2026-07-23
Status: accepted

## Context

Eidetic imports iPhone photos, which are HEIC (HEVC in a HEIF container). Decoding uses `libheif-rs` (the C library libheif + the libde265 codec), registered as an `image`-crate decoder hook via `ensure_heic_registered()` in `eidetic-ingest` and `eidetic-ml`.

The cross-platform single-artifact goal (ADR-0009) put this under scrutiny: libheif is a C/C++ library with codec deps, awkward to bundle and requiring a system install (`libheif-dev`) — friction, especially on Windows.

Pure-Rust HEIC decoders now exist (early 2026): `imazen/heic` and `ente-io/heic-decoder`, both with `image`-crate integration and no C deps. Tempting — a truly no-system-dep binary. But two facts decide against making them the default:

1. **License.** Both are **AGPL-3.0**. libheif/libde265 are **LGPL** — dynamically linking them from an MIT/Apache binary is already distribution-clean. Swapping to AGPL would encumber every released HEIC-enabled binary (colliding with ADR-0009's release artifacts). The real cost of libheif was never licensing; it's the system dependency.
2. **Maturity.** `imazen/heic` decodes ~118/162 HEIF test files and is ~73% pixel-exact (fine for embeddings/thumbnails, but some real files won't decode at all). libheif is the reference decoder.

(An earlier draft of this plan had it backwards — defaulting to pure-Rust "because AGPL is fine for personal use." Correct for personal use; wrong as a shipped default.)

## Decision

- **Default HEIC path: libheif** (via `libheif-rs`), unchanged. Mature, correct, LGPL — clean to distribute.
- **Add an opt-in `heic-pure` Cargo feature** that swaps in the pure-Rust AGPL decoder for a no-system-dependency build. That build artifact is AGPL; the default build stays MIT/Apache. *(Amendment 2026-09-02: never implemented — see below.)*
- HEVC decoding is patent-encumbered (Access Advance pool) regardless of implementation — same exposure either way; noted, not mitigated, for a personal self-hosted tool.

## Consequences

**Good:**

- Best-correctness default; iPhone HEIC keeps working exactly as today.
- Released default binaries stay permissively licensed and `cargo-deny`-clean.
- ~~Users who want zero system deps (and accept AGPL + occasional decode misses) build `--features heic-pure`.~~ *(Not implemented — see amendment.)*

**Bad / accepted:**

- The default binary still needs libheif installed to decode HEIC — the exact friction we wanted gone, especially on Windows. Accepted because correctness of the primary format wins, and the pure-Rust option exists for those who prefer no-install over completeness.
- Two decode paths behind one hook. Contained: both register through `ensure_heic_registered()`.
- The HEIC-encoding test fixture (currently made via libheif/x265) stays libheif-side; the `heic-pure` (decode-only) build uses a static fixture file.

## Amendment (2026-09-02): `heic-pure` was never implemented

The libheif default shipped; the `heic-pure` feature did not. No manifest,
source path, or CI job for it exists — `cargo build --features heic-pure`
is an invalid command, and a portable build simply drops HEIC via
`--no-default-features`. The option is **deferred, not accepted**: implement
it only when someone actually wants the AGPL no-system-dep build, and add a
CI lane for it in the same change. Until then the decision above should be
read as "libheif, feature-gated" and nothing more.

## Alternatives considered

- **Pure-Rust by default.** Rejected: AGPL-encumbers released binaries and the decoders are immature. Available as opt-in instead.
- **Skip HEIC entirely.** Rejected: it's an iPhone photo library; HEIC is a primary format, not an edge case.
- **Static-bundle libheif (vcpkg / `embedded-libheif`).** Deferred: possible, but libheif's deps (libde265) don't statically link automatically, so it's a C/C++ static-build pipeline to own per OS. Revisit only if the system-install friction proves unacceptable.

## References

- imazen/heic (AGPL, pure Rust): https://github.com/imazen/heic
- ente-io/heic-decoder (AGPL, pure Rust): https://github.com/ente-io/heic-decoder
- libheif (LGPL): https://github.com/strukturag/libheif
