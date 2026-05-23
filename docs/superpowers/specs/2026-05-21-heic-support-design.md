# HEIC Support Design

**Status:** Verified 2026-05-23 (V1–V6 all PASS — see table). All load-bearing claims confirmed by source reading + runtime probes in `tmp/heic-verify`. V6 has a slight mitigation: instead of Path D (committed fixture), we install Ubuntu's `libheif-plugin-x265` package on CI to keep Path C (runtime encode) tests viable.

**Original design draft:** 2026-05-21, verification-gated. The architecture rests on six claims about `libheif-rs` and the `image` crate's hook API that were written without running code. Plan Task 1 verifies all six; tasks 2+ proceed only on green. If verification fails on V1 (API existence) the spec is rewritten; V2/V3/V6 each have an explicit mitigation path documented below.

**Scope:** Make HEIC (HEIF + HEVC) images first-class in Eidetic — thumbnail generation and SigLIP embedding both work on `.heic` / `.heif` files. Triggered by a real-data finding: importing ~779 photos from a Dalhousie trip, **420 of them HEICs from an iPhone failed both thumbnailing and embedding** because the `image` crate doesn't decode HEIC. The landing-page grid filled with broken-image icons until PR #22 filtered to `thumbnails_generated = TRUE`; the underlying issue stayed.

---

## Verification gate

Six load-bearing claims; verify before any production-code task fires. Each has a stated mitigation if the claim turns out false, so an unfavourable outcome on V2/V3/V4/V5/V6 adjusts the implementation rather than killing the design. V1 is the only "rewrite the spec" outcome.

| # | Claim | If false | Verified 2026-05-23 |
|---|---|---|---|
| **V1** | `libheif_rs::integration::image::register_all_decoding_hooks()` exists at that exact path in `libheif-rs = "2.2"`. | Spec is wrong about the architecture; fall back to an explicit `decode_image` wrapper that branches on file extension. Likely a new `eidetic-media` crate after all. | **PASS.** API exists at the documented path. Cargo resolves `2.2` → `libheif-rs 2.7.0` (latest in 2.x), linked against `libheif-sys 5.2.0+1.21.2`. |
| **V2** | `register_all_decoding_hooks` is safe to call twice in a single process (idempotent / replacing / no-op on second call — not append-only or panicking). | Move `ensure_heic_registered` from per-crate to `eidetic-core` (which both ingest and ml already depend on), gated by a single process-wide `Once`. Implementation cost: trivial. | **PASS — idempotent.** Source: `image::hooks::register_decoding_hook` (image 0.25.10) uses `HashMap::entry`; vacant inserts, occupied returns `false`. `libheif-rs` only re-registers brands when the underlying `register_decoding_hook` returns `true`. Double-call runtime confirmed: no panic. → **Path A** (per-crate `Once`). |
| **V3** | `image::Limits` (`max_alloc = 512 MB`, `max_image_width/height = 16384`) propagate through the registered libheif hook and short-circuit before pixel allocation. | Add an explicit pre-decode dimension check via libheif's metadata API (`HeifContext::primary_image_handle().width()` / `.height()`) before triggering pixel decode. ~10 lines per call site. | **PASS.** Source: `ImageReader::decode` runs `limits.reserve(decoder.total_bytes())` and `decoder.set_limits(limits)` AFTER `make_decoder` returns, BEFORE `DynamicImage::from_decoder` triggers `read_image`. `HeifDecoder::new` parses container metadata only; no pixel allocation. Runtime confirmed: 2000×2000 HEIC + `max_image_width = 1000` → `Err("Image size exceeds limit")`. → no V3 mitigation needed. |
| **V4** | (a) `libheif-rs = "2.2"` exposes a feature named `v1_17` (the crate's docs reference `v1_17` / `v1_18` / `v1_19` / `latest`; we want the floor pin, but feature names can shift between minor versions). (b) `default-features = false, features = ["image", "v1_17"]` produces a working build. | (a) Pick whichever `v1_NN` feature actually exists in 2.2; record. (b) Read the crate's `Cargo.toml`, adjust the feature list. Build fail-fasts either way. | **PASS.** `libheif-rs 2.7.0` Cargo.toml exposes `v1_17` through `v1_21` features; `default = ["latest"]` where `latest = v1_21`; `image = ["dep:image"]` is NOT in defaults. `default-features = false, features = ["image", "v1_17"]` builds clean. |
| **V5** | `cargo deny check` passes with the new transitive tree (libheif-rs + libheif-sys + any bindgen / build-time deps). | Classify each complaint as fixable (allow-list with justification, ignore advisory pointing to upstream) or blocking. Blocking → escalate before further work. | **PASS.** `advisories ok, bans ok, licenses ok, sources ok` on the scratch project with workspace deny.toml. New transitives: `libheif-sys` (MIT), `libheif-rs` (MIT). |
| **V6** | Ubuntu 24.04's `libheif-dev` package and Homebrew's `libheif` formula both bundle the x265 encoder, so the synthetic-HEIC-at-test-setup pattern works in dev and CI. | If absent on **either** environment, switch all tests to a committed `tests/fixtures/tiny.heic` (4×4, ~200 bytes, generated once on a machine that has x265) read via `include_bytes!`. Tests run unconditionally — **no `#[ignore]` on missing-encoder**. The fixture is patent-irrelevant at four pixels of synthetic colour data. | **PASS macOS, PASS-with-plugin Ubuntu.** Homebrew `libheif 1.22.0` directly links `libx265.216.dylib`; runtime probe produced `x265 HEVC encoder (4.2+1-e444744)`. Ubuntu Noble `libheif1 1.17.6` does NOT depend on x265 — only suggests `libheif-plugin-x265` (separate package, exists in Noble). **Mitigation chosen: install plugin in CI** (one extra apt package) rather than the spec's Path D fixture. Tests stay on Path C (runtime encode). |

The plan's Task 1 is exactly these six checks. Each verified fact gets a `<!-- Verified 2026-MM-DD: ... -->` annotation back into the relevant section of this spec.

---

## Goals

- `eidetic import` of an HEIC produces both small and medium JPEG thumbnails, same as a JPEG would.
- `eidetic embed` produces a SigLIP embedding for an HEIC, same as a JPEG would.
- iPhone EXIF orientation is honoured — a portrait HEIC shot in landscape mode renders right-side-up.
- No new code in the `import` pipeline. The fix is entirely at the decode layer.
- Cross-platform: macOS (where the user runs) and Linux (where CI runs). Windows isn't a target today but the dep choice doesn't paint us into a corner.
- A single Cargo dependency, plus one system package (`libheif`), plus a one-line registration call.

## Non-goals

- A pure-Rust HEIC decoder. The `heic` crate (imazen/heic 0.1.3) is pure Rust, but its license is **AGPL-3.0-or-commercial**. Eidetic is MIT/Apache-2.0 (root `Cargo.toml:15`); an AGPL transitive would either taint the workspace license or require paying for a commercial license. **License is the disqualifier on its own.** Correctness isn't the issue: the crate passes 49/49 ITU-T HEVC conformance vectors — i.e. the codec iPhones actually emit decodes cleanly. (The lower "118/162" number sometimes cited is over `av1 + unci` features that don't apply to HEIC.)
- `embedded-libheif`. Compiling libheif from source on every fresh machine costs 2–5 min plus a cmake/nasm/libde265 toolchain. Personal use doesn't justify the build-time tax. Switchable in one Cargo.toml line later if Eidetic ever ships as a downloadable binary.
- AVIF support. libheif decodes AVIF too (it's HEIF + AV1) but Eidetic's library is iPhone-shaped, not AVIF-shaped. Deferred.
- HEIC sequences (the motion track of a Live Photo). v1 only decodes the primary still.
- Auxiliary images (depth maps, alpha masks) and wide-gamut colour profiles (Display P3, Rec. 2020). v1 returns sRGB.
- A new `eidetic-media` workspace crate. See the crate-layout section below — `libheif-rs` 2.2's `image` integration makes a wrapper crate unnecessary.
- HEIC encoding. Eidetic generates JPEG thumbnails (it always has). HEIC stays read-only.
- A migration. No schema change. The 420 already-imported HEICs have rows with `thumbnails_generated = false` and no embedding row; `eidetic thumbnail` and `eidetic embed` already iterate exactly those.

---

## Architecture overview

The `image` crate has a `decoder_hooks` registration API that lets third-party crates plug in formats `image` doesn't natively decode. `libheif-rs` 2.2 ships `integration::image::register_all_decoding_hooks()` against that API. After one call per process, every existing `image::ImageReader::open(p).with_guessed_format().decode()` site routes HEIC magic bytes through libheif transparently and returns a `DynamicImage` — same type, same shape, same error-handling.

<!-- Verified 2026-05-23: V1 PASS. Cargo resolved `2.2` → libheif-rs 2.7.0; the API exists at exactly that path. V3 PASS — image::Limits propagate; the Limits live above the hook layer claim is correct. Source: `ImageReader::decode` runs `limits.reserve(decoder.total_bytes())` and `decoder.set_limits(limits)` after `make_decoder` builds the HeifDecoder (metadata-only) but before `DynamicImage::from_decoder` triggers `read_image` (pixel decode). Runtime probe confirmed: oversize HEIC → `Err("Image size exceeds limit")`. -->


That means:
- `crates/eidetic-ingest/src/thumbnail.rs:108` (`load_image_with_limits`) keeps working unchanged for JPEG/PNG **and** now decodes HEIC.
- `crates/eidetic-ml/src/siglip.rs:267` (`preprocess_image`) keeps working unchanged for JPEG/PNG **and** now decodes HEIC.
- The two callers don't need to know HEIC exists. The decompression-bomb limits (`max_alloc = 512 MB`, `max_image_width/height = 16384`) still apply — `image::Limits` lives in the `image` crate, above the format-specific hook layer.

```
   ingest::thumbnail::load_image_with_limits()         ml::siglip::preprocess_image()
                       │                                              │
                       │ first line of each function:                 │
                       │   crate::ensure_heic_registered()            │
                       └──────────────┬───────────────────────────────┘
                                      │ per-crate `Once` guards each call.
                                      │ A process invokes the underlying
                                      │ registration at most twice — once
                                      │ when ingest first decodes, once when
                                      │ ml first decodes. V2 confirms
                                      │ double-call is safe.
                                      ▼
                  ┌─────────────────────────────────────────┐
                  │  libheif_rs::integration::image         │
                  │  ::register_all_decoding_hooks()        │
                  │  (mutates image crate global state —    │
                  │  registers HEIC magic bytes as          │
                  │  routing to a libheif-backed decoder)   │
                  └─────────────────────────────────────────┘

                          ┌───────────────────────┐
   thumbnail.rs ─────────►│ image::ImageReader    │
                          │   .open(path)         │      magic bytes
   siglip.rs    ─────────►│   .with_guessed_      ├─────► JPEG/PNG/WEBP ─► image crate
                          │       format()        │
                          │   .decode()           ├─────► HEIC/HEIF ────► libheif
                          │   → DynamicImage      │      (via hook)        (C library)
                          └───────────────────────┘
```

The arrow at the bottom is the entire mechanism: one global hook table, one path through `ImageReader::decode()`, two formats served. No detection logic in Eidetic, no `decode_image` wrapper, no routing — the `image` crate's existing `with_guessed_format` does it.

**Registration is lazy, not eager.** The call happens inside each decode function the first time that function runs, not at binary entry points. This makes tests transparent (a test that calls `load_image_with_limits` directly registers the hook on its way through, no setup ceremony needed). The atomic-load cost of `Once::call_once` after first registration is a relaxed load — negligible against the cost of decoding an image. Trade-off: a hypothetical new decode site added in a third crate must remember to call `ensure_heic_registered` itself; HEIC inputs to that site would silently fail otherwise. Acceptable today (only two decoders exist); flag in AGENTS.md so future contributors know.

---

## Crate layout: no new crate

**Original open question:** new `eidetic-media` workspace crate, or duplicate a `decode_image` helper across `eidetic-ingest` and `eidetic-ml`?

**Answer:** neither. The `image` integration approach makes both moot.

### Argument for a new `eidetic-media` crate (rejected)

- Centralises libheif and image-decoding logic in one place.
- Frees `eidetic-ingest` and `eidetic-ml` from needing to know libheif exists.
- Removes the duplicated decompression-bomb limits (currently in `thumbnail.rs:108-128` and `siglip.rs:267-283`).
- Future face-detection crate would be a third caller, satisfying AGENTS.md's two-callers rule for a new abstraction.
- The libheif dep lives in one Cargo.toml, isolating C-toolchain churn from the rest of the build graph.

### Argument against (chosen)

- The `image` integration feature reduces "HEIC support" to one workspace dep + one registration function call. There is no `decode_image` wrapper to write. The decompression-bomb limits stay where they are because `image::Limits` is applied at the `ImageReader` level, above any HEIC-specific code. The "centralise" pitch evaporates when there's nothing meaningful to centralise.
- AGENTS.md rule: "no new crate without a concrete caller in another crate." A wrapper crate would have one or two callers and ~10 lines of logic. That's not enough mass to justify a workspace member.
- The existing 30-line duplication of bomb-limit setup across `thumbnail.rs` and `siglip.rs` was always small. Touching it as a side quest in this PR would inflate scope; touching it later is also fine.
- Eidetic ML and ingest already depend on `image`. Adding `libheif-rs` is symmetric to that — one more decoding library, declared next to the existing one.

### What the change looks like

Two crates gain a `libheif-rs` dep and a small public helper:

```rust
// crates/eidetic-ingest/src/lib.rs (and equivalent in eidetic-ml/src/lib.rs)
use std::sync::Once;
static HEIC_INIT: Once = Once::new();

/// Register libheif as a decoder hook on the `image` crate. Idempotent;
/// safe to call from every entry point.
pub fn ensure_heic_registered() {
    HEIC_INIT.call_once(|| {
        libheif_rs::integration::image::register_all_decoding_hooks();
    });
}
```

The two existing decoders (`load_image_with_limits` in `thumbnail.rs`, `preprocess_image` in `siglip.rs`) gain one line at the top:

```rust
crate::ensure_heic_registered();
```

That's the entire user-facing code change in v1. Everything else is config (deps, CI, README) and a backfill run.

### Two `Once`s, one global registry

The setup above creates two independent `Once` statics — `eidetic_ingest::HEIC_INIT` and `eidetic_ml::HEIC_INIT`. Each guards exactly one call to `libheif_rs::integration::image::register_all_decoding_hooks()` per process. A process therefore invokes the underlying registration **twice** — once when ingest's first decode happens, once when ml's first decode happens. The two `Once`s are blind to each other.

This is safe iff `register_all_decoding_hooks` is one of:
- **Idempotent** — second call is a no-op (checks a flag, returns).
- **Replacing** — overwrites the prior hook table without error.

It is **not** safe if the function is:
- **Append-only** — each call pushes a new hook, leaving two competing HEIC decoders in the global registry.
- **Panicking** — second call asserts uniqueness and aborts.

V2 in the verification gate reads the libheif-rs source to determine which behaviour the implementation has. If idempotent or replacing, the per-crate-`Once` design is fine. If append-only or panicking, `ensure_heic_registered` moves to `eidetic-core` so a single process-wide `Once` guards the single global call — at the cost of `eidetic-core` gaining a heavy C-binding dep, violating its AGENTS.md "lightweight foundation" rule. That cost is accepted only if V2 forces it.

<!-- Verified 2026-05-23: V2 PASS — idempotent. The underlying `image::hooks::register_decoding_hook` (image 0.25.10) uses `HashMap::entry`; vacant inserts, occupied returns false. libheif-rs only re-registers magic-byte brands when this returns true. Per-crate Once (Path A) chosen. eidetic-core stays lightweight. -->


---

## Detection / dispatch

**There isn't any in Eidetic code.** The `image` crate already calls `with_guessed_format()` in both `thumbnail.rs` and `siglip.rs`, which sniffs magic bytes and picks the matching registered decoder. HEIC's ISO-BMFF `ftyp` brand bytes match libheif's hook; JPEG/PNG/WEBP match `image`'s built-in decoders.

Why this is the right strategy out of the three options I considered:

- **Extension-based** (`.heic` / `.heif` case-insensitive). Would have worked, but it duplicates what `with_guessed_format` already does — magic-byte sniffing — and breaks on renamed files (rare in personal-use but pointless to break). And the registered-hook approach removes the need to make any choice in Eidetic code at all.
- **MIME from the `infer` crate**. The ingest path already calls `meta::detect_mime` and stores `mime_type` in the DB. But thumbnail and ML decoders take `&Path`, not a row — re-reading bytes to MIME-sniff them is wasted work when `ImageReader` is going to read them anyway.
- **Magic bytes** in `with_guessed_format`. What we get for free. Chosen.

---

## EXIF rotation handling

iPhone HEICs carry an `irot` / `imir` transform property in the HEIF container (analogous to EXIF `Orientation` in JPEG). libheif applies these transforms by default during decode, so a portrait-oriented HEIC comes back as a portrait `DynamicImage` (height > width). This is the behaviour Eidetic wants.

**Asymmetry, documented honestly:** the `image` crate does **not** auto-apply EXIF orientation to JPEGs. A JPEG shot in portrait mode on a phone is stored as a landscape pixel buffer with `Orientation = 6` in EXIF, and Eidetic currently renders those JPEGs sideways. That bug exists today; it is not regressed by this PR and is not fixed by this PR. After HEIC support lands, the inconsistency becomes more visible: HEIC photos display correctly, JPEG-with-orientation photos display sideways. Filing a separate task for the JPEG side.

No code is needed in Eidetic to opt into libheif's rotation behaviour — it's the default. We explicitly do **not** call `set_ignore_transformations(true)` anywhere.

---

## Failure-mode semantics

Three failure cases for an HEIC file:

1. **Corrupt HEIC.** libheif returns a decode error → `image::ImageReader::decode()` propagates it as `image::ImageError` → existing `thumbnail::generate_thumbnails` returns `Err(Error::ImageDecode { path, source })` → existing import-pipeline branch at `crates/eidetic-ingest/src/import.rs:92-102` (logging block at lines 95-99) sets `thumbnails_generated = false` after logging an info-level warning. The asset row is still inserted. No new code path.
2. **Unsupported HEIC variant** (e.g. HEIC with an aux image type libheif doesn't know). Same as case 1 — manifests as a decode error, handled identically.
3. **libheif system library missing at runtime.** Process fails to start with a dynamic-linker error (`dyld: Library not loaded: libheif.dylib` on macOS, `error while loading shared libraries` on Linux). README documents the install step; nothing Eidetic can do at runtime.

Symmetric to JPEG behaviour: a corrupt `.jpg` produces the same `Error::ImageDecode` today, handled the same way at the same call site. HEIC piggybacks on existing infrastructure.

---

## Dependencies

### Workspace `Cargo.toml`

Add one entry to `[workspace.dependencies]`:

```toml
# HEIC/HEIF decoding (registers as a decoder hook on the `image` crate)
libheif-rs = { version = "2.2", default-features = false, features = ["image", "v1_17"] }
```

Feature notes:
- `image` — exposes the `integration::image::register_all_decoding_hooks` function. The whole reason we picked this crate.
- `v1_17` — matches the libheif library version 1.17, which is what `apt install libheif-dev` ships on Ubuntu 24.04 LTS and what `brew install libheif` currently installs on macOS. Newer is fine (libheif is conservative about API breaks); this just sets the minimum.
- `default-features = false` to drop any other features the crate enables by default. We can flip individual ones on later if we need them.

<!-- Verified 2026-05-23: V4 PASS. libheif-rs 2.7.0 (the version cargo resolves `2.2` to today) exposes `v1_17` through `v1_21` features. `default = ["latest"]` where `latest = v1_21`; `image = ["dep:image"]` is NOT in defaults so it MUST be specified explicitly. Build clean with `default-features = false, features = ["image", "v1_17"]`. -->


### Crate `Cargo.toml`s

`crates/eidetic-ingest/Cargo.toml` and `crates/eidetic-ml/Cargo.toml` each gain:

```toml
libheif-rs = { workspace = true }
```

No other crate touches HEIC. Server, CLI, DB, core: unchanged.

### Version compatibility note (verify during plan)

`libheif-rs` 2.2's `image` feature targets a specific major of the `image` crate. The workspace pins `image = "0.25"` (root `Cargo.toml:37`). The plan should `cargo add --dry-run libheif-rs` and confirm. If they mismatch, the resolution is a workspace `image` bump or a `libheif-rs` version pin. Not anticipated to be hard either way, but flagged.

---

## CI changes

`.github/workflows/ci.yml` already runs `clippy` and `test` on Ubuntu (current contents at `lines 26-64`). Both jobs need libheif headers + library present so the linker step in `cargo clippy --all-targets` and `cargo test` finds `libheif.so`.

Add this step to both the `clippy` and `test` jobs, after `actions/checkout` and before the rust-cache action:

```yaml
      - name: Install libheif
        run: sudo apt-get update && sudo apt-get install -y libheif-dev libheif-plugin-x265
```

`libheif-plugin-x265` is the x265 (HEVC) encoder plugin. Ubuntu 24.04's stock `libheif1` does not pull it in (only suggests it), so the HEIC encode→decode round-trip in our unit tests would fail without it. macOS Homebrew's `libheif` formula bundles x265 directly, so dev machines need no extra step.

The `fmt` and `deny` jobs don't link, so they don't need libheif. Skip.

Cost: ~10 seconds per job, ~20 seconds total CI runtime. No caching needed; apt's package cache plus rust-cache already do their thing.

No changes to `cargo-deny` config — `libheif-rs` is MIT, `libheif-sys` is MIT, libheif itself is LGPL-3.0 but it's a system library we link to, not a Rust crate cargo-deny audits.

<!-- Verified 2026-05-23: V5 PASS. `cargo deny check` on the scratch project with workspace `deny.toml`: advisories ok, bans ok, licenses ok, sources ok. No new license needs adding to the allow-list. V6 mitigation: install `libheif-plugin-x265` on Ubuntu CI alongside `libheif-dev` (one extra apt package); see "Testing approach" note. -->


---

## README updates

Add a "System dependencies" subsection to the existing "Quick start" or "Build" section. Place it before the `cargo install` line, after the docker compose line.

Content (literal):

```markdown
### System dependencies

Eidetic uses `libheif` to decode HEIC/HEIF photos (the format iPhones produce by default). Install it once:

- **macOS:** `brew install libheif`
- **Ubuntu / Debian:** `sudo apt install libheif-dev libheif-plugin-x265`
- **Other Linux:** install the `libheif` development package and the x265 encoder plugin via your distribution's package manager.

If `libheif` isn't installed, Eidetic builds fine but fails at runtime with a dynamic-linker error when a HEIC file is encountered.
```

---

## Testing approach

The honest constraint: HEIC fixtures are awkward to ship. Patent-encumbered codec, binary files, dozens of variants. We can't realistically commit a representative HEIC corpus.

**Strategy:** generate a tiny synthetic HEIC at test setup via libheif's encoder, then decode it back. Tests the full encode→decode round-trip without checked-in binary fixtures.

`libheif-rs` exposes `Context::new()` + `HeifContext::write_to_bytes()` etc. for encoding. The encoder path uses x265 by default; tests should pick a tiny image size (e.g. 16×16) to keep encode latency negligible.

<!-- Verified 2026-05-23: V6 PASS macOS — Homebrew libheif 1.22.0 links libx265.216 directly; runtime probe produced `x265 HEVC encoder (4.2+1-e444744)` and a 64x64 HEIC encode→decode round-trip via `image::ImageReader` returned the correct dimensions. V6 PASS-with-plugin Ubuntu — `libheif1` does NOT depend on x265 by default (only suggests `libheif-plugin-x265`, which is a separate package that exists in Noble). Chosen mitigation: install both `libheif-dev` AND `libheif-plugin-x265` on Ubuntu CI (deviates from the spec's Path D fixture mitigation, but cheaper and keeps the encode→decode test design symmetric across platforms). Path C (runtime encoder) retained. -->


### Test 1 — `crates/eidetic-ingest/src/thumbnail.rs` (in-file `#[cfg(test)] mod`)

```rust
#[test]
fn generate_writes_thumbnails_from_heic_source() {
    crate::ensure_heic_registered();
    let tmp = tempfile::tempdir().unwrap();
    let src = make_synthetic_heic(tmp.path(), 64, 64);  // helper
    let library = tmp.path().join("library");
    let hash = fixture_hash();
    generate_thumbnails(&src, &hash, &library).expect("generate from HEIC");
    assert!(thumbnail_path(&library, &hash, ThumbSize::Small).exists());
    assert!(thumbnail_path(&library, &hash, ThumbSize::Medium).exists());
}
```

`make_synthetic_heic` is a test-module helper that uses `libheif-rs` to encode a deterministic RGB pattern to an HEIC byte stream, writes it to disk, returns the path. Lives next to the existing `make_real_jpeg` helper at `thumbnail.rs:142`.

### Test 2 — `crates/eidetic-ml/src/siglip.rs` (in-file `#[cfg(test)] mod`)

```rust
#[test]
fn preprocess_image_accepts_heic_source() {
    crate::ensure_heic_registered();
    let tmp = tempfile::tempdir().unwrap();
    let src = make_synthetic_heic(tmp.path(), 64, 64);
    let pixels = preprocess_image(&src, 224).expect("preprocess HEIC");
    assert_eq!(pixels.len(), 3 * 224 * 224);
}
```

Re-uses the same encoding helper. To avoid duplicating `make_synthetic_heic` in two crates, place it in a small test-only module — option A: a `dev-dependencies` test-helper crate (overkill); option B: copy-paste the ~15-line helper into both `#[cfg(test)]` modules (chosen — DRY does not apply to test fixtures of trivial size). Plan will spell out option B literally.

### Test 3 — round-trip rotation

```rust
#[test]
fn heic_with_irot_transform_decodes_rotated() {
    // Encode a 64x32 landscape HEIC with irot=90° (portrait when displayed).
    // Decode via image::ImageReader; assert resulting DynamicImage is 32 wide × 64 tall.
}
```

Confirms the rotation default isn't accidentally disabled. Lives wherever the encoder helper lives; choosing `eidetic-ingest` so the assertion is on `DynamicImage::width()` / `height()` rather than the SigLIP tensor layout.

### What we are NOT testing

- Real iPhone HEICs. Those need a manual smoke test (see below); they're too varied + too patent-encumbered to ship as CI fixtures.
- AVIF, JPEG-XL, or other HEIF-family formats. Not in scope.
- libheif version sensitivity (1.17 vs 1.21). The plugin API is stable; we trust it.

### Manual smoke test (post-merge, on Abhishek's Mac)

1. `eidetic thumbnail` → expect "Generating thumbnails for 420 images…" → all 420 succeed.
2. `eidetic embed` → embeds the same 420 HEICs.
3. `eidetic serve` → landing-page grid shows HEIC thumbnails instead of broken-image icons.
4. Click an HEIC → detail page shows it right-side-up, EXIF block correct.
5. `eidetic search "people sitting outside"` → HEIC photos from the friend's library now appear in results.

---

## Backfill plan

No new command, no migration, no SQL touch. After the PR merges:

```bash
eidetic thumbnail   # picks up rows WHERE thumbnails_generated = FALSE; now succeeds on HEIC
eidetic embed       # picks up rows missing embedding_id; now succeeds on HEIC
```

The 420 currently-skipped HEICs from the Dalhousie import become discoverable and have thumbnails.

Both commands already iterate via `repo.fetch_unthumbnailed` and the embed equivalent. Both already log per-asset progress. Both already exit non-zero on any failure, so accidental regressions on already-working JPEGs surface loudly.

---

## Risks

### C-library attack surface

Adding libheif (transitively libde265, optionally x265 for tests) widens Eidetic's C-attack surface. libheif has shipped CVEs in malformed-HEIC parsing — CVE-2023-49463, CVE-2023-49464, CVE-2024-25269 are recent examples, all memory-safety bugs reachable from a crafted input file. The rest of the workspace is memory-safe Rust above two existing C/C++ boundaries (`ort`'s ONNX Runtime, `sqlx`/`libpq`); libheif is a third.

**Personal-use containment.** The threat model contains this risk: Eidetic decodes files the user has explicitly imported from local disk (photos copied from a phone, files dragged in). It does not decode network-supplied bytes. A malicious HEIC would have to reach the user's filesystem first — through a phishing attachment, a hostile download, a compromised photo-share — at which point libheif is one of several attack surfaces alongside the OS image viewer, Photos app, etc.

**Reassessment trigger.** If Eidetic ever (a) accepts uploads from a federated or multi-user source, (b) opens an endpoint that receives image bytes from the network, or (c) is run with file-system access to untrusted directories (a shared photo bucket, a download-spool), this risk needs a fresh review. Mitigations available at that point: keep libheif at the latest stable, sandbox the decode call via OS facilities, or push the decode to a separate process with restricted privileges.

### Verification-gate uncertainty

Six load-bearing claims (V1–V6 above) were written without running code. Plan Task 1 verifies each before any production change. Mitigations for V2/V3/V4/V5/V6 are spelled out and rated as "implementation cost trivial." V1 (API existence) is the only outcome that would force a spec rewrite; the verification cost is a single `cargo doc --open`, so an unfavourable result surfaces immediately.

### New decode-site discoverability

The registration is lazy (called at the top of each existing decode function), which keeps tests transparent but means a future contributor who adds a third decode site in a new crate must remember to call `ensure_heic_registered` themselves. The AGENTS.md update in this PR notes this on the workspace-map rows for ingest and ml; future-decoder-site discipline depends on contributors reading it.

---

## Honest scope

### In v1 (this PR)

- HEIC and HEIF primary-image decode via libheif.
- libheif's default orientation handling (irot/imir applied).
- sRGB output via `DynamicImage`.
- Workspace dep declared in one place.
- Two crates use it (`eidetic-ingest`, `eidetic-ml`).
- Three round-trip unit tests (thumbnail, preprocess, rotation).
- CI step installs `libheif-dev`.
- README documents the install step.
- Backfill is "run two existing commands".

### Deferred

- **JPEG EXIF orientation.** Pre-existing bug; HEIC support makes it more visible. Separate task.
- **AVIF.** libheif can do it; we punt until there's an AVIF photo in the user's library.
- **HEIC sequences / Live Photo motion track.** v1 returns the primary still.
- **Aux images** (depth, alpha): not read.
- **Wide-gamut colour profiles** (Display P3, Rec. 2020): output is sRGB whatever the source claims.
- **Static distribution.** Dynamic-link only. If Eidetic ever ships as a downloadable binary, flip to `features = ["image", "v1_17", "embedded-libheif"]` and pay the cmake/nasm toolchain cost at build time. One-line change.
- **Windows.** Not exercised. The dep choice doesn't preclude it (vcpkg has libheif), but it's untested.

---

## Implementation order (hint for plan author)

1. **Verification gate** (Plan Task 1). Run V1–V6 from the table at the top. Each produces a verified fact recorded back into the relevant section of this spec. No production code touched yet. Halt and update spec if any verification fails.
2. Add `libheif-rs` to workspace `[workspace.dependencies]` and to `crates/eidetic-ingest/Cargo.toml` + `crates/eidetic-ml/Cargo.toml`.
3. Add `ensure_heic_registered()` (with `Once` guard). Location depends on V2: per-crate (ingest + ml) if idempotent; in `eidetic-core` if not. Call from the top of `load_image_with_limits` and `preprocess_image`.
4. Add the three unit tests. Fixture mechanism depends on V6: runtime libheif encoder if available everywhere; checked-in `include_bytes!` fixture otherwise.
5. Update `.github/workflows/ci.yml` with `apt install -y libheif-dev` step on `clippy` and `test` jobs.
6. Update README with system-deps subsection; update `AGENTS.md` workspace map to note HEIC support on the ingest + ml rows.
7. Open PR. On Abhishek's Mac, run `eidetic thumbnail && eidetic embed` to backfill the 420. Record decode failures (if any) in the PR description.

Each step ends with `cargo clippy --workspace --all-targets -- -D warnings` green. The pre-commit hook enforces this.

---

## Spec self-review

**Placeholder scan.** No TBDs, no "implement later". Every code-touching step has the literal code or the literal `cargo`/`apt`/`yaml` line.

**Internal consistency.**
- The "no new crate" decision in the crate-layout section matches the "Implementation order" not mentioning `eidetic-media`.
- The Once-guarded `ensure_heic_registered` shows up in: architecture diagram, code snippet under crate layout, both unit tests, and the implementation order.
- The "EXIF rotation" claim ("libheif applies by default") is the reason there's a rotation-specific unit test (test 3).
- The "dynamic link" stance shows up in: non-goals (`embedded-libheif` rejected), README (install step required), and runtime-failure mode (case 3).

**Scope.**
- Single PR. Two crate `Cargo.toml` lines, one root `Cargo.toml` line, two short `ensure_heic_registered` functions, three unit tests, one CI step, one README section. The "implementation order" lists seven sequential steps; each is small. Plausible in one focused PR.

**Ambiguity.**
- libheif version target: pinned to `v1_17` feature, matching Ubuntu 24.04 and current Homebrew.
- Default features off: explicit `default-features = false`.
- Test fixtures: synthetic-HEIC-at-test-setup, not checked-in binaries — explicit.
- Backfill mechanism: existing commands, no new code — explicit.

**Known risk.** See the dedicated "Risks" section above. Self-review intentionally does not restate it.

**Spec coverage check.**
Each item the user asked for in the conversation that started this spec:

- Final crate-layout decision with both sides argued and one recommended → §"Crate layout: no new crate" ✓
- Public API of the helper → §"Crate layout"; it's `ensure_heic_registered()`, not `decode_image()` ✓
- Detection strategy with justification → §"Detection / dispatch"; magic-byte sniffing via `with_guessed_format`, free ✓
- EXIF rotation handling → §"EXIF rotation handling" ✓
- Failure-mode semantics → §"Failure-mode semantics", three cases enumerated ✓
- Test strategy → §"Testing approach" ✓
- Backfill plan → §"Backfill plan" ✓
- CI changes → §"CI changes" ✓
- README install instructions → §"README updates" ✓
- Honest scope → §"Honest scope" ✓
