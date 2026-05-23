# HEIC Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add HEIC/HEIF decoding to Eidetic so `eidetic import`, `eidetic thumbnail`, and `eidetic embed` all handle iPhone photos. After the PR merges, run `eidetic thumbnail && eidetic embed` on Abhishek's Mac to backfill the 420 currently-skipped HEICs from the Dalhousie import.

**Architecture:** Verify-then-implement. The spec rests on six load-bearing claims about `libheif-rs` 2.2's integration with the `image` crate (`docs/superpowers/specs/2026-05-21-heic-support-design.md`, "Verification gate"). Task 1 is exactly those six verifications — no production code touched. Tasks 2+ proceed only after the gate is green; mitigations are documented inline so unfavourable outcomes on V2/V3/V4/V5/V6 adjust the implementation without redesign.

The implementation itself, if all verifications pass: one workspace dep, one ~5-line `ensure_heic_registered()` helper per affected crate (each guarded by `std::sync::Once`), one call inserted at the top of each existing decode site. The existing `image::ImageReader::open(p).with_guessed_format().decode()` chain at `crates/eidetic-ingest/src/thumbnail.rs:108` and `crates/eidetic-ml/src/siglip.rs:267` then routes HEIC magic bytes through libheif transparently. No `decode_image` wrapper, no new crate.

**Tech Stack:** Rust 2024 workspace, `libheif-rs = "2.2"` with `features = ["image", "v1_17"]`, system `libheif` (dynamic link, install via `brew install libheif` / `apt install libheif-dev`). The `image` crate's `decoder_hooks` API does the routing.

**Spec reference:** `docs/superpowers/specs/2026-05-21-heic-support-design.md` (sections: "Verification gate", "Architecture overview", "Crate layout: no new crate", "EXIF rotation handling", "Failure-mode semantics", "Dependencies", "CI changes", "README updates", "Testing approach", "Backfill plan", "Honest scope").

---

## File map

**Create:**
- `tmp/heic-verify/` — scratch project for Task 1's verification probes. Deleted before final commit; not in the workspace.
- Possibly: `crates/eidetic-ingest/tests/fixtures/tiny.heic` (only if V6 fails — see Task 5).

**Modify:**
- `Cargo.toml` (workspace root) — add `libheif-rs = { version = "2.2", default-features = false, features = ["image", "v1_17"] }` to `[workspace.dependencies]`.
- `crates/eidetic-ingest/Cargo.toml` — add `libheif-rs = { workspace = true }`.
- `crates/eidetic-ml/Cargo.toml` — add `libheif-rs = { workspace = true }`.
- `crates/eidetic-ingest/src/lib.rs` — add `ensure_heic_registered` (or, if V2 mitigation triggers, call into `eidetic_core::ensure_heic_registered`).
- `crates/eidetic-ml/src/lib.rs` — same.
- `crates/eidetic-ingest/src/thumbnail.rs` — one `ensure_heic_registered()` call at the top of `load_image_with_limits`; add 2 unit tests + `make_synthetic_heic` helper (or `include_bytes!` if V6 fails).
- `crates/eidetic-ml/src/siglip.rs` — one `ensure_heic_registered()` call at the top of `preprocess_image`; add 1 unit test + helper (mirror of thumbnail.rs's).
- `.github/workflows/ci.yml` — add `apt install -y libheif-dev` step to `clippy` and `test` jobs.
- `README.md` — add "System dependencies" subsection.
- `AGENTS.md` — note HEIC support on the `eidetic-ingest` and `eidetic-ml` workspace-map rows.
- `docs/superpowers/specs/2026-05-21-heic-support-design.md` — annotate each verified claim with `<!-- Verified 2026-MM-DD: ... -->` notes.

**Conditional (V2 fails → registration is non-idempotent):**
- `crates/eidetic-core/src/lib.rs` — add `ensure_heic_registered` here; remove the per-crate copies in ingest and ml.
- `crates/eidetic-core/Cargo.toml` — gain `libheif-rs = { workspace = true }`.

**Conditional (V3 fails → `image::Limits` doesn't propagate):**
- `crates/eidetic-ingest/src/thumbnail.rs` — pre-decode dimension check via libheif metadata API before `ImageReader::decode()`.
- `crates/eidetic-ml/src/siglip.rs` — same.

**Conditional (V5 finds blocking license):**
- `deny.toml` — adjust allow-list with justification, or escalate.

---

## Task 1: Verification gate (no production code)

This task produces verified facts but writes no production code. All work lands in a throwaway `tmp/heic-verify/` directory (not part of the workspace, not committed) and as annotation comments back into the spec.

**Files:**
- Create: `tmp/heic-verify/` (scratch crate for verification)
- Modify: `docs/superpowers/specs/2026-05-21-heic-support-design.md` (annotations only)

**Outcomes:** the rest of this plan branches on the six verification results. Each step below ends by writing a short "Verified 2026-MM-DD: <fact>" comment into the relevant section of the spec.

- [ ] **Step 1: Create the scratch verification project**

```bash
mkdir -p tmp/heic-verify
cd tmp/heic-verify
cargo init --name heic-verify --bin
cat >> Cargo.toml <<'EOF'

[dependencies]
libheif-rs = { version = "2.2", default-features = false, features = ["image", "v1_17"] }
image = "0.25"
EOF
```

Verify cargo accepts the manifest:

```bash
cargo metadata --no-deps >/dev/null
```

If `cargo metadata` errors with "default features dropped a required feature," continue anyway — V4 will diagnose this in Step 5.

- [ ] **Step 2: V1 — confirm `register_all_decoding_hooks` exists at the documented path**

Inside `tmp/heic-verify/src/main.rs`, write:

```rust
fn main() {
    // Reference the symbol. If V1 is true, this compiles.
    let _ = libheif_rs::integration::image::register_all_decoding_hooks;
    println!("V1 ok: register_all_decoding_hooks at expected path");
}
```

Run:

```bash
cargo build 2>&1 | tee build.log
cargo run
```

Expected: `V1 ok: register_all_decoding_hooks at expected path`.

**If the build fails with `cannot find function `register_all_decoding_hooks` in module `image`` or similar:**

1. Open the crate docs locally:

```bash
cargo doc --no-deps --open
```

2. In the rendered docs, search the `libheif_rs` tree for any function whose name or signature looks like a decoder-hook registrar (e.g. `register_decoder`, `init_image_integration`).
3. If a function exists at a different path → V1 partial pass. Record the actual path in the spec under "Architecture overview" with a `<!-- Verified 2026-MM-DD: actual path is X -->` note, update the spec's code snippets accordingly, and proceed.
4. If no such function exists anywhere → **V1 full fail.** Stop. Update the spec's "Verification gate" V1 row to "failed" and rewrite the spec around an explicit `decode_image` wrapper (likely in a new `eidetic-media` crate). The plan from Task 2 onward becomes invalid; produce a follow-up plan.

Record the V1 outcome in the spec:

```markdown
<!-- Verified 2026-MM-DD: V1 pass. register_all_decoding_hooks exists at the documented path. -->
```

- [ ] **Step 3: V2 — confirm idempotency under double-call**

In `tmp/heic-verify/src/main.rs`, replace the body with:

```rust
fn main() {
    // Call the registration twice. If the second call panics, V2 fails (case d).
    libheif_rs::integration::image::register_all_decoding_hooks();
    libheif_rs::integration::image::register_all_decoding_hooks();
    println!("V2 step 1 ok: did not panic on second call");
}
```

Run `cargo run`. Expected: no panic. If panic — V2 case (d) — apply the V2 mitigation (see "If V2 fails" below).

That doesn't tell us if the two registrations stacked, though. To distinguish idempotent from append-only, read the source:

```bash
# In a separate shell, browse the GitHub source:
# https://github.com/Cykooz/libheif-rs (look at integration/image module)
# Or:
cargo vendor target/heic-verify-vendor 2>/dev/null || true
find ~/.cargo/registry/src -type d -name 'libheif-rs-*' -exec ls -la {}/src/integration {} \;
```

Open the file that defines `register_all_decoding_hooks`. Classify by what it does:

- **(a) idempotent** — checks if hooks are already registered; second call is a no-op. **V2 pass.**
- **(b) append-only** — pushes a new hook onto a list each call. **V2 fail.**
- **(c) replacing** — overwrites prior registration. **V2 pass** (functionally equivalent to idempotent for our needs).
- **(d) panicking** — already caught above.

Record the V2 outcome:

```markdown
<!-- Verified 2026-MM-DD: V2 pass — register_all_decoding_hooks is <idempotent|replacing>. -->
```

or

```markdown
<!-- Verified 2026-MM-DD: V2 fail — register_all_decoding_hooks is <append-only|panics>. Mitigation: move ensure_heic_registered to eidetic-core, single process-wide Once. See Task 3 conditional path. -->
```

**If V2 fails:** Task 3 takes the conditional path documented inline. No other tasks change.

- [ ] **Step 4: V3 — confirm `image::Limits` propagate through the hook**

This is the security-critical check. We need to know whether passing `max_image_width = 16384` via `ImageReader::limits()` blocks an oversized HEIC before pixel allocation.

The cleanest probe is to construct an HEIC whose metadata claims oversized dimensions, then attempt decode with limits. Constructing such a file is non-trivial without an HEIC mux tool — but `libheif-rs` exposes the encoder, so:

In `tmp/heic-verify/src/main.rs`, replace the body with:

```rust
use image::ImageReader;
use std::io::Cursor;

fn main() {
    libheif_rs::integration::image::register_all_decoding_hooks();

    // Encode a small (but tractable) HEIC, then attempt decode with a Limits
    // that's tighter than the encoded dimensions. If Limits propagate, decode
    // returns Err.
    let heic_bytes = encode_synthetic_heic(2000, 2000);

    let mut reader = ImageReader::new(Cursor::new(heic_bytes))
        .with_guessed_format()
        .expect("guess format");

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1000);    // tighter than encoded width
    limits.max_image_height = Some(1000);
    reader.limits(limits);

    match reader.decode() {
        Ok(img) => {
            println!("V3 FAIL: decode succeeded despite Limits — width={}, height={}",
                     img.width(), img.height());
            std::process::exit(1);
        }
        Err(e) => {
            println!("V3 ok: decode rejected by Limits — {e}");
        }
    }
}

fn encode_synthetic_heic(w: u32, h: u32) -> Vec<u8> {
    use libheif_rs::*;
    // Encoder construction is libheif-rs-version-specific. The skeleton:
    //   1) Build a HeifContext.
    //   2) Create an encoder with EncoderQuality::Lossless or similar.
    //   3) Create an Image, fill RGB plane with a deterministic pattern.
    //   4) ctx.encode_image(&img, &mut encoder, ...).
    //   5) ctx.write_to_bytes() → Vec<u8>.
    //
    // Refer to libheif-rs examples on the docs.rs page or the crate's
    // `examples/` directory.
    todo!("fill in per libheif-rs 2.2 encoder API; see crate examples")
}
```

`todo!()` is acceptable here because the body lives in a throwaway project — this is verification scaffolding, not production code. Concrete encoder code is one of the things V6 will incidentally confirm (libheif-rs encoder API works as documented).

Run:

```bash
cargo run
```

Expected: `V3 ok: decode rejected by Limits — <error message>`.

If you can't get the encoder API working in time, the alternative probe is to read the libheif-rs source for the `Decoder` impl that the hook registers, and trace whether the `Limits` get passed into the libheif call. Look for a `read_image_with_limits` or similar in the `integration::image::decoder` module.

Record:

```markdown
<!-- Verified 2026-MM-DD: V3 pass — image::Limits propagate; oversize HEIC rejected before pixel allocation. -->
```

or

```markdown
<!-- Verified 2026-MM-DD: V3 fail — Limits ignored by libheif hook. Mitigation: explicit pre-decode dimension check via HeifContext metadata API in load_image_with_limits and preprocess_image. -->
```

**If V3 fails:** Task 4 gains an extra step in each decode-site wiring (pre-decode dimension check via libheif metadata before invoking `ImageReader`).

- [ ] **Step 5: V4 — read `libheif-rs`'s `[features]` table (`v1_17` exists + default-feature set)**

```bash
# Browse the crate's Cargo.toml on docs.rs or GitHub:
# https://github.com/Cykooz/libheif-rs/blob/main/Cargo.toml
# (also visible via cargo vendor; see Step 3)
```

Two sub-checks:

**V4a — does `v1_17` exist as a feature on `libheif-rs = "2.2"`?**
Look for a `v1_17 = [...]` (or similar shape) entry under `[features]`. If absent, find the lowest `v1_NN` feature that does exist; that becomes the floor pin and the spec's "Dependencies" line + Task 2 Step 1 update accordingly.

**V4b — what's in the default-feature set?**
Find the `default = [...]` line under `[features]`. Three cases:

- **Defaults are empty (`default = []`)**: `default-features = false` is a no-op. Spec's line is harmless. **V4 pass.**
- **Defaults include `image` or `v1_17` only**: redundant with our explicit features. `default-features = false` is harmless. **V4 pass.**
- **Defaults include something we need but don't list (e.g. some `dynamic` feature is default-on; or `image` is a non-default feature)**: spec line is wrong. Adjust to include whatever default we need.

Final check — actually compile under the spec's exact feature set:

```bash
# In tmp/heic-verify/Cargo.toml, restore the libheif-rs dep line exactly as spec'd
# (or adjusted per V4a).
cargo build 2>&1 | tail -20
```

If build fails or produces missing-symbol errors at link time, V4 is more interesting than just reading the Cargo.toml.

Record:

```markdown
<!-- Verified 2026-MM-DD: V4 pass — libheif-rs 2.2 exposes v1_17 feature; defaults are <list>. default-features = false + [image, v1_17] builds clean. -->
```

- [ ] **Step 6: V5 — run `cargo deny check` on the scratch project**

```bash
# From the workspace root (NOT the tmp/heic-verify dir):
cp deny.toml tmp/heic-verify/deny.toml

cd tmp/heic-verify
cargo deny check 2>&1 | tee ../../tmp/heic-verify-deny.log
```

Expected: zero advisories, zero license complaints, zero ban complaints.

Triage any complaints:

- **License complaint** (e.g. "libheif-sys has unknown license") → check the actual license in the crate's repo. If it's something already on the allow-list under a different name, the issue is `confidence-threshold = 0.8` — bump to 0.7 or add the license to the allow-list explicitly with a justification comment.
- **Advisory complaint** (e.g. yanked version, security advisory) → check upstream. If patched in a newer libheif-rs, bump our pin. If unpatched but not affecting our usage, add to `[advisories.ignore]` with a comment.
- **Multiple-versions warning** → likely fine (the existing `paste` advisory pattern). Tolerate.

Record:

```markdown
<!-- Verified 2026-MM-DD: V5 pass — cargo deny check clean. New transitive deps: libheif-sys (MIT), <others>. -->
```

or, if mitigations were needed:

```markdown
<!-- Verified 2026-MM-DD: V5 pass with deny.toml adjustment — added <license/advisory> to <allow-list/ignore> with justification. -->
```

**If V5 surfaces a blocking license** (e.g. a GPL transitive that would taint the workspace): stop. Escalate. Do not proceed to Task 2.

- [ ] **Step 7: V6 — confirm libheif x265 encoder availability**

Two environments to check: Abhishek's Mac (Homebrew) and Ubuntu 24.04 (the CI image).

**Mac (local):**

```bash
brew install libheif      # idempotent if already installed
pkg-config --modversion libheif
# Look for x265 in the linked libs:
otool -L "$(brew --prefix libheif)/lib/libheif.dylib" | grep -i x265 || \
    echo "x265 not linked into libheif on macOS"
```

Both commands should succeed. If `x265 not linked` prints, run `brew info libheif` to check whether x265 is a separate formula.

**Ubuntu 24.04 (CI image, via Docker):**

```bash
docker run --rm ubuntu:24.04 bash -c '
    apt-get update -qq &&
    apt-get install -y -qq libheif-dev pkg-config &&
    pkg-config --modversion libheif &&
    dpkg -L libheif-dev libheif1 | grep -i "x265\|encoder" || \
        echo "no x265 encoder found in libheif-dev/libheif1"
'
```

Expected: pkg-config prints a version (1.17+), and `grep` finds at least one path mentioning `x265` or an encoder plugin.

If x265 isn't included, also check for the separate plugin package:

```bash
docker run --rm ubuntu:24.04 bash -c '
    apt-get update -qq &&
    apt-cache search libheif | grep -i x265
'
```

Record:

```markdown
<!-- Verified 2026-MM-DD: V6 pass — x265 encoder present in macOS Homebrew libheif and Ubuntu 24.04 libheif-dev. Runtime encode-then-decode test pattern viable. -->
```

or

```markdown
<!-- Verified 2026-MM-DD: V6 fail — x265 absent from <macOS|Ubuntu> libheif build. Mitigation: pre-compute tiny.heic fixture and use include_bytes!. See Task 5. -->
```

**If V6 fails:** Task 5's test strategy switches from runtime encoding to a checked-in `tiny.heic` byte fixture. Plan steps for that path are spelled out in Task 5.

- [ ] **Step 8: Update spec with all verified facts**

Open `docs/superpowers/specs/2026-05-21-heic-support-design.md`. For each of V1–V6, place a `<!-- Verified 2026-MM-DD: ... -->` HTML comment next to the relevant claim. Specifically:

- V1 result → near "Architecture overview" section's first paragraph.
- V2 result → near "Crate layout: no new crate" → "What the change looks like" section.
- V3 result → near "Architecture overview" paragraph that says limits live above the hook layer.
- V4 result → near "Dependencies" section, feature notes.
- V5 result → near "CI changes" section, second-to-last paragraph.
- V6 result → near "Testing approach" section, "Strategy" paragraph.

Also update the "Verification gate" table at the top — mark each row pass / fail / mitigated.

- [ ] **Step 9: Clean up scratch project, commit spec annotations**

```bash
rm -rf tmp/heic-verify tmp/heic-verify-deny.log

# Commit the spec updates:
git add docs/superpowers/specs/2026-05-21-heic-support-design.md
git commit -m "docs(spec): verify HEIC support load-bearing claims (V1-V6)

V1: register_all_decoding_hooks confirmed at libheif_rs::integration::image.
V2: <result>. V3: <result>. V4: <result>. V5: <result>. V6: <result>.

Spec section annotations updated. Mitigations triggered: <list, or 'none'>."
```

Pre-commit hook only runs fmt + clippy; this commit touches docs only, so the hook is a no-op.

---

## Task 2: Add `libheif-rs` workspace dep + crate-level deps

**Pre-condition:** V1 passed (Task 1 Step 2). V4 either passed cleanly, or its mitigation has been applied (feature list adjusted to match what `libheif-rs` actually exposes). V5 passed (or `deny.toml` updated).

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/eidetic-ingest/Cargo.toml`
- Modify: `crates/eidetic-ml/Cargo.toml`

After this task, the workspace builds with `libheif-rs` linked into ingest and ml, but no Eidetic code uses it yet. Workspace stays green.

- [ ] **Step 1: Add to root `Cargo.toml`'s `[workspace.dependencies]`**

Find the `[workspace.dependencies]` block in `Cargo.toml`. After the existing `# Image loading and resizing` group (around line 36-37 — the `image = "0.25"` line), add:

```toml
# HEIC/HEIF decoding (registers as a decoder hook on the `image` crate)
libheif-rs = { version = "2.2", default-features = false, features = ["image", "v1_17"] }
```

**If V4's mitigation triggered** (the feature list needed adjustment), use the verified feature list instead. Pull the exact line from the spec's "Dependencies" section, which Task 1 Step 8 updated.

- [ ] **Step 2: Add to `crates/eidetic-ingest/Cargo.toml`**

In `[dependencies]`, after the existing `image = { workspace = true }` line, add:

```toml
libheif-rs = { workspace = true }
```

- [ ] **Step 3: Add to `crates/eidetic-ml/Cargo.toml`**

Same edit. In `[dependencies]`, after `image = { workspace = true }`:

```toml
libheif-rs = { workspace = true }
```

- [ ] **Step 4: Build the workspace**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green. If linker errors mention `libheif`, confirm the system library is installed (`brew install libheif` on macOS, `apt install libheif-dev` on Linux). Pre-commit hook would catch this too.

If clippy complains about unused dependencies on `libheif-rs`, that's expected — Task 3 introduces the first use. The `unused_crate_dependencies` lint is not enabled in the workspace, so this should pass clippy even with the dep unused at this stage.

- [ ] **Step 5: Run `cargo deny check`**

```bash
cargo deny check
```

Expected: green. If it errors, V5's verification was incomplete; resolve the same way V5 mitigations would (allow-list + justification, or escalate).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock \
        crates/eidetic-ingest/Cargo.toml \
        crates/eidetic-ml/Cargo.toml
git commit -m "build(deps): add libheif-rs for HEIC/HEIF decoding

Workspace dep + ingest/ml crate-level deps. No code uses it yet —
the next commit wires the decoder-hook registration into the
existing image::ImageReader call sites."
```

---

## Task 3: `ensure_heic_registered` helper

**Pre-condition:** Task 2 complete. V2 outcome determines this task's path:

- **V2 pass (idempotent / replacing):** add the helper to `eidetic-ingest` and `eidetic-ml` separately, each with its own `Once`. Path **A** below.
- **V2 fail (append-only / panics):** add the helper to `eidetic-core`, single process-wide `Once`. Path **B** below.

Pick one path. Do not interleave.

### Path A (V2 pass): per-crate helper

**Files:**
- Modify: `crates/eidetic-ingest/src/lib.rs`
- Modify: `crates/eidetic-ml/src/lib.rs`

- [ ] **Step A1: Add the helper to `crates/eidetic-ingest/src/lib.rs`**

Current contents (read confirmed in spec prep):

```rust
//! File ingestion for Eidetic.

mod error;
mod hasher;
mod import;
mod meta;
mod store;
pub mod thumbnail;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use store::{commit_staged, stage_file};
```

After the last `pub use`, add:

```rust

/// Register libheif as a decoder hook on the `image` crate.
///
/// Idempotent: safe to call from every decode site; the first call
/// registers and subsequent calls are no-ops via the internal `Once`.
/// libheif-rs's `register_all_decoding_hooks` is itself
/// <idempotent|replacing> per V2 verification (2026-MM-DD), so even
/// without our `Once`, double-call is safe — the `Once` keeps the
/// atomic check on the hot path cheap.
pub fn ensure_heic_registered() {
    use std::sync::Once;
    static HEIC_INIT: Once = Once::new();
    HEIC_INIT.call_once(|| {
        libheif_rs::integration::image::register_all_decoding_hooks();
    });
}
```

- [ ] **Step A2: Add the helper to `crates/eidetic-ml/src/lib.rs`**

Current contents:

```rust
//! ML inference for Eidetic.

mod error;
mod siglip;

pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
```

After the last `pub use`, add the identical block:

```rust

/// Register libheif as a decoder hook on the `image` crate.
///
/// Idempotent: safe to call from every decode site. See identical
/// helper in eidetic-ingest/src/lib.rs for the V2 verification note.
pub fn ensure_heic_registered() {
    use std::sync::Once;
    static HEIC_INIT: Once = Once::new();
    HEIC_INIT.call_once(|| {
        libheif_rs::integration::image::register_all_decoding_hooks();
    });
}
```

The duplication is intentional — both crates use it, neither depends on the other, and centralising it in `eidetic-core` would force `eidetic-core` to pull `libheif-rs` (it currently has zero heavy deps; AGENTS.md describes it as "the lightweight foundation"). Two copies of ~6 lines is the smaller violation.

- [ ] **Step A3: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Green.

- [ ] **Step A4: Commit**

```bash
git add crates/eidetic-ingest/src/lib.rs crates/eidetic-ml/src/lib.rs
git commit -m "feat(ingest,ml): ensure_heic_registered helper

Per-crate Once-guarded wrapper around libheif-rs's
register_all_decoding_hooks. Each decode site (thumbnail.rs +
siglip.rs) will call this at the top in the next commit.

Duplicated across the two crates rather than centralised in
eidetic-core to keep eidetic-core's dep graph light (AGENTS.md
calls it 'the lightweight foundation'). libheif-rs is a heavy
C-binding crate."
```

### Path B (V2 fail): centralised helper in eidetic-core

**Files:**
- Modify: `crates/eidetic-core/Cargo.toml`
- Modify: `crates/eidetic-core/src/lib.rs`
- Modify: `crates/eidetic-ingest/Cargo.toml` (remove the libheif-rs dep added in Task 2 — eidetic-core's transitive carries it)
- Modify: `crates/eidetic-ml/Cargo.toml` (same)

- [ ] **Step B1: Add `libheif-rs` to `crates/eidetic-core/Cargo.toml`**

```toml
[dependencies]
# … existing entries …
libheif-rs = { workspace = true }
```

- [ ] **Step B2: Add the helper to `crates/eidetic-core/src/lib.rs`**

After the existing `pub use ids::...` line, add:

```rust

/// Register libheif as a decoder hook on the `image` crate.
///
/// Single process-wide Once. Required because
/// `libheif_rs::integration::image::register_all_decoding_hooks` is
/// <append-only | panics on second call> per V2 verification
/// (2026-MM-DD); a per-crate Once would still let the underlying
/// registration fire twice and corrupt the hook table.
pub fn ensure_heic_registered() {
    use std::sync::Once;
    static HEIC_INIT: Once = Once::new();
    HEIC_INIT.call_once(|| {
        libheif_rs::integration::image::register_all_decoding_hooks();
    });
}
```

- [ ] **Step B3: Remove `libheif-rs` from ingest and ml `Cargo.toml`s**

Both `crates/eidetic-ingest/Cargo.toml` and `crates/eidetic-ml/Cargo.toml`: delete the `libheif-rs = { workspace = true }` line added in Task 2. They get it transitively via `eidetic-core`.

If clippy complains about an unused crate dep, that's the signal to remove.

- [ ] **Step B4: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Green.

- [ ] **Step B5: Commit**

```bash
git add crates/eidetic-core/Cargo.toml crates/eidetic-core/src/lib.rs \
        crates/eidetic-ingest/Cargo.toml crates/eidetic-ml/Cargo.toml \
        Cargo.lock
git commit -m "feat(core): ensure_heic_registered (single process-wide Once)

V2 verification found register_all_decoding_hooks is
<append-only|panics on second call>, so a per-crate Once isn't
enough — we need a single registration point. eidetic-core is the
right home: both ingest and ml depend on it already.

Heavy C-binding dep in eidetic-core is a known cost; the
alternative (third crate carrying the dep) is worse for the
workspace map."
```

---

## Task 4: Wire `ensure_heic_registered` into the two decode sites

**Pre-condition:** Task 3 complete (either Path A or Path B).

**Files:**
- Modify: `crates/eidetic-ingest/src/thumbnail.rs`
- Modify: `crates/eidetic-ml/src/siglip.rs`

After this task, both decoders attempt libheif when `with_guessed_format` sniffs HEIC magic bytes. No tests yet — Task 5 adds them.

- [ ] **Step 1: Wire `load_image_with_limits` in `thumbnail.rs`**

The current function (`crates/eidetic-ingest/src/thumbnail.rs:108-128`) opens with:

```rust
fn load_image_with_limits(path: &Path) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
```

Insert one line at the very start of the function body (above the `let mut reader = ...` line):

```rust
fn load_image_with_limits(path: &Path) -> Result<image::DynamicImage> {
    crate::ensure_heic_registered();
    let mut reader = image::ImageReader::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
```

**If V3 failed** (Limits don't propagate through the hook): add a pre-decode dimension check between the `reader.limits(limits);` line and the `reader.decode()` call. Use libheif-rs's metadata API to read the primary-image dimensions without triggering pixel decode:

```rust
    // V3 mitigation: libheif's hook ignores image::Limits, so we check
    // dimensions explicitly using libheif's metadata-only API.
    if is_heic_path(path) {
        check_heic_dimensions(path, 16384)?;
    }
```

Plus a `check_heic_dimensions` helper at the bottom of the file. Stub:

```rust
fn check_heic_dimensions(path: &Path, max_edge: u32) -> Result<()> {
    let ctx = libheif_rs::HeifContext::read_from_file(
        path.to_str().ok_or_else(|| Error::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-utf8 path"),
        })?,
    )
    .map_err(|e| Error::ImageDecode {
        path: path.to_path_buf(),
        source: image::ImageError::Limits(image::error::LimitError::from_kind(
            image::error::LimitErrorKind::Generic(format!("libheif metadata read failed: {e}")),
        )),
    })?;
    let handle = ctx.primary_image_handle().map_err(/* same shape */)?;
    let w = handle.width();
    let h = handle.height();
    if w > max_edge || h > max_edge {
        return Err(Error::ImageDecode { /* dimension limit */ });
    }
    Ok(())
}

fn is_heic_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"))
}
```

The exact `image::ImageError::Limits(...)` shape may differ slightly between `image` crate minor versions; consult `image::error::LimitError` docs and adapt. The point is: same error variant as bomb-limit failures so the upstream pipeline branch already handles it.

If V3 passed (the common case), skip the helper and the call to it.

- [ ] **Step 2: Wire `preprocess_image` in `siglip.rs`**

The current function (`crates/eidetic-ml/src/siglip.rs:267-283`) opens with:

```rust
fn preprocess_image(path: &Path, image_size: u32) -> Result<Vec<f32>> {
    // Cap allocation + dimensions before decode so a decompression-bomb file
    // (crafted PNG/JPEG/WEBP) can't OOM-kill the embed loop. 512 MB / 16384 px
    // is well above any real photo and well below "exhaust process memory".
    let mut reader = image::ImageReader::open(path)
```

Insert one line at the start of the function body:

```rust
fn preprocess_image(path: &Path, image_size: u32) -> Result<Vec<f32>> {
    crate::ensure_heic_registered();
    // Cap allocation + dimensions before decode so a decompression-bomb file
    // (crafted PNG/JPEG/WEBP) can't OOM-kill the embed loop. 512 MB / 16384 px
    // is well above any real photo and well below "exhaust process memory".
    let mut reader = image::ImageReader::open(path)
```

Also update the bomb-limit comment to acknowledge HEIC is now in the list:

```rust
    // Cap allocation + dimensions before decode so a decompression-bomb file
    // (crafted PNG/JPEG/WEBP/HEIC) can't OOM-kill the embed loop. 512 MB / 16384 px
    // is well above any real photo and well below "exhaust process memory".
```

(The "HEIC" addition to the comment is mechanical and accurate only if V3 passed. If V3 failed, leave the comment alone and add a separate comment above the dimension-check call referencing the mitigation.)

If V3 failed, add the same `check_heic_dimensions` pattern as Step 1 — but note `eidetic-ml` would need its own copy (or move to eidetic-core under Path B). For Path A, duplicate the helper into `siglip.rs`. For Path B, expose `check_heic_dimensions` from `eidetic-core` as `pub fn`.

- [ ] **Step 3: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Green. The existing thumbnail and embed tests still pass (they use JPEG fixtures and don't touch the new code path).

- [ ] **Step 4: Smoke test the registration**

A quick non-test sanity check on Abhishek's Mac:

```bash
# Pick one HEIC from the Dalhousie import
HEIC=$(find /Volumes/Data/Dalhousie\ -type f -iname '*.heic' | head -1)
cargo run -p eidetic-cli --release -- hash "$HEIC"
```

(`eidetic hash` doesn't decode — just SHA-256s — so it always works. The real check is the next command.)

Manually walk one HEIC through the thumbnail path. There's no `eidetic thumbnail <path>` subcommand, but the existing `eidetic thumbnail` (no args) iterates over the DB. Easier: write a one-off `cargo test` invocation that loads the file through `load_image_with_limits` and asserts decode succeeds. Or just wait for Task 5's tests to cover it.

Skip if the test in Task 5 is going to land in the next commit anyway.

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ingest/src/thumbnail.rs crates/eidetic-ml/src/siglip.rs
git commit -m "feat(ingest,ml): wire ensure_heic_registered into decoders

One line at the top of load_image_with_limits and preprocess_image.
After this commit, both decoders accept HEIC inputs alongside the
JPEG/PNG/WEBP set the image crate already handled.

[If V3 mitigation: 'Also includes pre-decode dimension check via
libheif metadata API — V3 verification found image::Limits do not
propagate through the registered hook.']"
```

---

## Task 5: Unit tests

**Pre-condition:** Task 4 complete. V6 outcome determines the fixture mechanism:

- **V6 pass:** synthetic HEIC encoded at test setup via libheif-rs. Path **C**.
- **V6 fail:** pre-computed `tiny.heic` fixture, committed, used via `include_bytes!`. Path **D**.

Pick one. Tests live next to existing tests in the same crates' `#[cfg(test)] mod tests` blocks.

### Path C (V6 pass): runtime encoding helper

**Files:**
- Modify: `crates/eidetic-ingest/src/thumbnail.rs`
- Modify: `crates/eidetic-ml/src/siglip.rs`

- [ ] **Step C1: Add `make_synthetic_heic` helper to `thumbnail.rs`**

In the existing `#[cfg(test)] mod tests` block at the bottom of `thumbnail.rs`, after the existing `make_real_jpeg` helper:

```rust
    fn make_synthetic_heic(dir: &Path, w: u32, h: u32) -> PathBuf {
        use libheif_rs::*;

        // Build a synthetic RGB plane with a deterministic pattern.
        let mut img = Image::new(w, h, ColorSpace::Rgb(RgbChroma::Rgb))
            .expect("create heif image");
        img.create_plane(Channel::Interleaved, w, h, 8)
            .expect("create plane");

        // Encode at lossless quality so the round-trip preserves enough info
        // for assertions (small images + lossless = still tiny on disk).
        let mut encoder = LibHeif::new()
            .encoder_for_format(CompressionFormat::Hevc)
            .expect("hevc encoder available (V6 verification)");
        encoder.set_quality(EncoderQuality::LossLess)
            .expect("set quality");

        let mut ctx = HeifContext::new().expect("new context");
        ctx.encode_image(&img, &mut encoder, None).expect("encode");

        let bytes = ctx.write_to_bytes().expect("write to bytes");
        let path = dir.join("synthetic.heic");
        std::fs::write(&path, &bytes).expect("write heic fixture");
        path
    }
```

The exact libheif-rs API symbols may differ across the crate's versions; V1/V6 verification will have confirmed the encoder API. If symbol names don't match, consult the crate's `examples/` directory.

- [ ] **Step C2: Add the two HEIC unit tests to `thumbnail.rs`**

In the same `#[cfg(test)] mod tests` block, after the existing `generate_creates_intermediate_directories` test:

```rust
    #[test]
    fn generate_writes_thumbnails_from_heic_source() {
        crate::ensure_heic_registered();
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_synthetic_heic(tmp.path(), 64, 64);
        let library = tmp.path().join("library");
        let hash = fixture_hash();

        generate_thumbnails(&src, &hash, &library).expect("generate from HEIC");

        let small = thumbnail_path(&library, &hash, ThumbSize::Small);
        let medium = thumbnail_path(&library, &hash, ThumbSize::Medium);
        assert!(small.exists(), "small thumb missing at {small:?}");
        assert!(medium.exists(), "medium thumb missing at {medium:?}");
    }

    #[test]
    fn heic_decode_returns_dynamic_image_with_correct_dimensions() {
        // Round-trip check that the libheif hook returns a usable DynamicImage.
        // Sanity: not a crashing-empty image, has the dimensions we encoded.
        crate::ensure_heic_registered();
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_synthetic_heic(tmp.path(), 64, 32);

        let img = load_image_with_limits(&src).expect("decode synthetic HEIC");
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 32);
    }
```

- [ ] **Step C3: Add `make_synthetic_heic` helper + one test to `siglip.rs`**

In `crates/eidetic-ml/src/siglip.rs`'s `#[cfg(test)] mod tests` block, add the same helper (copy-pasted from thumbnail.rs, with the small adjustment that this file imports `image` differently):

```rust
    fn make_synthetic_heic(dir: &std::path::Path, w: u32, h: u32) -> std::path::PathBuf {
        // [same body as thumbnail.rs Step C1]
    }

    #[test]
    fn preprocess_image_accepts_heic_source() {
        crate::ensure_heic_registered();
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_synthetic_heic(tmp.path(), 64, 64);

        let pixels = preprocess_image(&src, 224).expect("preprocess HEIC");
        assert_eq!(pixels.len(), 3 * 224 * 224);
    }
```

Duplicating the helper across two crates is intentional: a) the helper is short, b) extracting it would force a test-only crate which is the same kind of mass-vs-cost tradeoff the spec rejected for the production code, c) DRY is not load-bearing for test fixtures.

- [ ] **Step C4: Run the tests**

```bash
cargo test -p eidetic-ingest -- thumbnail
cargo test -p eidetic-ml -- siglip
```

Expected: all tests pass, including the three new ones. The existing JPEG tests still pass (regression check).

- [ ] **Step C5: Workspace verify**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Green.

- [ ] **Step C6: Commit**

```bash
git add crates/eidetic-ingest/src/thumbnail.rs crates/eidetic-ml/src/siglip.rs
git commit -m "test(ingest,ml): HEIC decode round-trip tests

Three new tests via libheif-rs's encoder API:
- generate_writes_thumbnails_from_heic_source: end-to-end through
  load_image_with_limits + generate_thumbnails.
- heic_decode_returns_dynamic_image_with_correct_dimensions:
  dimensions survive the round-trip.
- preprocess_image_accepts_heic_source: SigLIP pre-tensor stage
  succeeds, output length matches 3 * 224 * 224.

make_synthetic_heic helper duplicated across the two test modules
intentionally — short fixture, not worth a test-helper crate."
```

### Path D (V6 fail): committed `tiny.heic` byte fixture

**Files:**
- Create: `crates/eidetic-ingest/tests/fixtures/tiny.heic` (a 4×4 synthetic HEIC, ~200 bytes)
- Modify: `crates/eidetic-ingest/src/thumbnail.rs` (tests reference the fixture)
- Modify: `crates/eidetic-ml/src/siglip.rs` (same)

The fixture is patent-irrelevant: 4 pixels of synthetic colour data, well below any meaningful threshold. License clean.

- [ ] **Step D1: Generate `tiny.heic` on a machine that has the x265 encoder**

```bash
mkdir -p crates/eidetic-ingest/tests/fixtures

# On Abhishek's Mac (Homebrew libheif has x265):
cat > tmp/gen-tiny.rs <<'EOF'
fn main() {
    // (Use the make_synthetic_heic body from Path C step C1, 4x4 dims.)
    // Output: ../crates/eidetic-ingest/tests/fixtures/tiny.heic
}
EOF
# Compile and run inside a scratch cargo project (or extend tmp/heic-verify
# from Task 1 if still present).
```

The resulting fixture should be under 500 bytes for a 4×4 image. Verify:

```bash
ls -la crates/eidetic-ingest/tests/fixtures/tiny.heic
file crates/eidetic-ingest/tests/fixtures/tiny.heic
# Expected: HEIF Image, HEVC codec, 4x4 pixels.
```

- [ ] **Step D2: Use `include_bytes!` in the tests**

```rust
    const TINY_HEIC: &[u8] = include_bytes!("../tests/fixtures/tiny.heic");

    fn write_tiny_heic(dir: &Path) -> PathBuf {
        let path = dir.join("tiny.heic");
        std::fs::write(&path, TINY_HEIC).expect("write fixture");
        path
    }
```

Then use `write_tiny_heic(tmp.path())` in place of `make_synthetic_heic(tmp.path(), 64, 64)` in the test bodies. Adjust assertions for the smaller dimensions (4×4 instead of 64×64).

- [ ] **Step D3: Mirror into `eidetic-ml`**

Either:
- Copy `tiny.heic` into `crates/eidetic-ml/tests/fixtures/` (small duplication).
- Or use a relative `include_bytes!` path pointing across crate boundaries (cargo allows this but it's ugly).

Pick the copy. ~200 extra bytes in the repo for symmetry across both test modules.

- [ ] **Step D4-D6: Run tests, verify, commit**

(Same as C4–C6, with adjusted commit message mentioning the static fixture rather than runtime encoding.)

---

## Task 6: CI changes

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add `libheif-dev` install step to the `clippy` job**

Open `.github/workflows/ci.yml`. The current `clippy` job (lines 26-36):

```yaml
  clippy:
    name: clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd  # v6
      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8  # stable
        with:
          toolchain: 1.92.0
          components: clippy
      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32  # v2
      - run: cargo clippy --workspace --all-targets -- -D warnings
```

Insert between the `dtolnay/rust-toolchain` step and the `Swatinem/rust-cache` step:

```yaml
      - name: Install libheif
        run: sudo apt-get update && sudo apt-get install -y libheif-dev
```

Result:

```yaml
  clippy:
    name: clippy
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd  # v6
      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8  # stable
        with:
          toolchain: 1.92.0
          components: clippy
      - name: Install libheif
        run: sudo apt-get update && sudo apt-get install -y libheif-dev
      - uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32  # v2
      - run: cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 2: Add the same step to the `test` job**

The `test` job (lines 38-64) needs the identical `Install libheif` step. Insert it between `dtolnay/rust-toolchain` and `Swatinem/rust-cache`:

```yaml
      - name: Install libheif
        run: sudo apt-get update && sudo apt-get install -y libheif-dev
```

The `fmt` and `deny` jobs don't link Rust code (just check formatting / cargo-deny config), so they don't need libheif. Leave them alone.

- [ ] **Step 3: Verify locally (no easy way — push and watch CI)**

There's no `act` / `nektos` setup in this repo to run GH Actions locally. The realistic verification is to push the branch and watch CI go green. If CI is red on a libheif-link error, the apt step landed in the wrong place; reorder.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: install libheif-dev for clippy and test jobs

eidetic-ingest and eidetic-ml link against libheif via libheif-rs.
Both clippy --all-targets (which builds dev-tests) and the test
job need the system library present. fmt and deny don't link, so
they're unchanged."
```

---

## Task 7: README + AGENTS.md updates

**Files:**
- Modify: `README.md`
- Modify: `AGENTS.md`

- [ ] **Step 1: Add "System dependencies" subsection to README**

The current `README.md` has a "Build" section starting around the existing line "## Build". Before that, after the existing "## Database" section, add:

```markdown
## System dependencies

Eidetic uses `libheif` to decode HEIC/HEIF photos (the format iPhones produce by default). Install it once:

- **macOS:** `brew install libheif`
- **Ubuntu / Debian:** `sudo apt install libheif-dev`
- **Other Linux:** install the `libheif` development package via your distribution's package manager.

If `libheif` isn't installed, Eidetic builds fine but fails at runtime with a dynamic-linker error when a HEIC file is encountered.
```

- [ ] **Step 2: Update AGENTS.md workspace map**

Open `AGENTS.md`. The workspace map table (around line 36-46) currently describes `eidetic-ingest` as:

```
| `eidetic-ingest` | File watcher, streaming hasher, content-addressable storage, thumbnail generation. Calls `PgAssetsRepo` directly. | `eidetic-core`, `eidetic-db` |
```

Update to:

```
| `eidetic-ingest` | File watcher, streaming hasher, content-addressable storage, thumbnail generation (JPEG/PNG/WEBP/HEIC via libheif). Calls `PgAssetsRepo` directly. | `eidetic-core`, `eidetic-db` |
```

And `eidetic-ml`:

```
| `eidetic-ml` | `SiglipEmbedder` (concrete, no trait) loads ONNX models via `ort` and produces L2-normalised image/text embeddings as `Vec<f32>`. Decodes JPEG/PNG/WEBP/HEIC images via the `image` crate + libheif hook. | `eidetic-core` |
```

(If Path B was taken in Task 3, update `eidetic-core`'s row to mention "Owns `ensure_heic_registered` for HEIC decoder-hook registration on the `image` crate.")

- [ ] **Step 3: Verify (no code change, just review the rendered files)**

```bash
# Optional: render README.md locally to confirm formatting.
glow README.md 2>/dev/null || cat README.md | head -100
```

- [ ] **Step 4: Commit**

```bash
git add README.md AGENTS.md
git commit -m "docs: HEIC system dep + workspace-map updates

README gains a 'System dependencies' subsection covering the
libheif install step for macOS and Linux.

AGENTS.md workspace map now mentions HEIC support on the
ingest and ml rows so future agents know the decoder surface."
```

---

## Task 8: Open PR, run backfill, capture results

**Files:** none modified (operational task)

- [ ] **Step 1: Push the branch and open the PR**

```bash
git push -u origin <branch-name>
gh pr create --title "feat: HEIC/HEIF decoding via libheif-rs" --body "$(cat <<'EOF'
## Summary
- Adds `libheif-rs` 2.2 as a workspace dep, registered as a decoder hook on the `image` crate.
- Two crates (`eidetic-ingest`, `eidetic-ml`) now decode HEIC alongside JPEG/PNG/WEBP via the existing `ImageReader` chain.
- Three new unit tests cover thumbnail generation, dimensions round-trip, and SigLIP preprocessing on HEIC sources.
- CI installs `libheif-dev`. README documents the install step.

## Why
Real-data finding: 420 of 779 photos imported from the Dalhousie trip were iPhone HEICs, all of which silently failed both thumbnailing and embedding. The landing-page grid filled with broken-image icons until PR #22 filtered to `thumbnails_generated = TRUE`, but the underlying gap (the `image` crate can't decode HEIC) stayed. This closes it.

## Verification gate outcome
Task 1 of the implementation plan ran six pre-flight checks (V1–V6) on the load-bearing claims. Results: V1 <result>, V2 <result>, V3 <result>, V4 <result>, V5 <result>, V6 <result>. See spec annotations in `docs/superpowers/specs/2026-05-21-heic-support-design.md`.

## Test plan
- [ ] `cargo test --workspace` green (CI runs this)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` green (CI + pre-commit)
- [ ] `cargo deny check` green (CI)
- [ ] After merge: run `eidetic thumbnail && eidetic embed` against the Dalhousie library on my Mac; expect ~420 successful HEIC thumbnails + embeddings.
- [ ] After backfill: `eidetic serve`, confirm the landing-page grid shows iPhone photos right-side-up.
EOF
)"
```

- [ ] **Step 2: Wait for CI to pass on the PR**

Watch the GH Actions tab. The likely failure modes:

- libheif-dev install step failed → adjust the apt command (some Ubuntu images need `apt-get update` separately).
- Link error → step landed in the wrong place in the YAML.
- A test fails on Linux but passes on Mac → likely a libheif build difference between distros (V6 issue surfaced in CI specifically).

- [ ] **Step 3: After CI green, merge and switch to main**

```bash
gh pr merge --squash --delete-branch
git checkout main
git pull --ff-only
```

- [ ] **Step 4: Run the backfill on Abhishek's Mac**

```bash
# From workspace root, with EIDETIC_DATABASE_URL set.
cargo run --release -p eidetic-cli -- thumbnail | tee /tmp/heic-thumbnail.log
cargo run --release -p eidetic-cli -- embed     | tee /tmp/heic-embed.log
```

Expected output:

```
Generating thumbnails for ~420 images…
[1/420] /Volumes/eidetic-library/aa/bb/<hash>.heic
…
Done. Generated 420.
```

If any decode failures show up in the log, capture them — each represents an HEIC variant libheif refused. File the failure cases as a follow-up issue with the offending paths.

- [ ] **Step 5: Spot-check via `eidetic serve`**

```bash
cargo run --release -p eidetic-cli -- serve
```

Open `http://127.0.0.1:8080`. Expected:

- Landing grid shows photos from the Dalhousie import including iPhone HEICs.
- Click an HEIC photo → detail page renders the thumbnail right-side-up (rotation honoured).
- `eidetic search "people sitting outside"` returns HEIC photos in results.

- [ ] **Step 6: Backfill metrics for the PR description (optional, helpful)**

Append a comment to the merged PR with the backfill numbers:

```bash
THUMBS=$(grep -c '^\[' /tmp/heic-thumbnail.log)
FAILS=$(grep -c 'generate failed' /tmp/heic-thumbnail.log)
gh pr comment <pr-number> --body "Backfill done: ${THUMBS} HEIC thumbnails generated, ${FAILS} failures. <link to follow-up issue if FAILS > 0>."
```

---

## Plan self-review

**Spec coverage.** Every section of the spec maps to at least one task:

| Spec section | Plan task |
|---|---|
| Verification gate | Task 1 |
| Goals | Task 5 (tests assert the goals), Task 8 (backfill confirms them) |
| Architecture overview / crate layout | Task 3 (Path A or B) |
| Detection / dispatch | None — happens for free via `with_guessed_format`; no code touches detection. |
| EXIF rotation | Task 5 (rotation-aware test, if added — currently the spec defers an explicit rotation test to "future work"; could add as a Task 5 step) |
| Failure-mode semantics | None new — existing pipeline handles `Err(ImageDecode)`. Verified indirectly via existing tests + the new HEIC-decode tests covering the success path. |
| Dependencies | Task 2 |
| CI changes | Task 6 |
| README updates | Task 7 |
| Testing approach | Task 5 |
| Backfill plan | Task 8 |
| Honest scope | Implicitly enforced — no task adds AVIF, sequences, etc. |

**Placeholder scan.** One intentional `todo!()` in Task 1 Step 4 (V3 verification scaffolding, in throwaway code that doesn't get committed). All production-code steps have literal code. The conditional V3-mitigation helper (`check_heic_dimensions`) has its body sketched with explicit comments about which libheif-rs symbols to use; the engineer fills in the exact API per V1/V6 verification findings.

**Type consistency.** `ensure_heic_registered()` signature is identical across both Path A and Path B (`pub fn ensure_heic_registered()`, no args, no return). Both decode sites call `crate::ensure_heic_registered();` in Path A or rely on a transitively-available `eidetic_core::ensure_heic_registered();` in Path B (the path through `crate::` actually still works in Path B because both ingest and ml depend on core; if they don't re-export, the call becomes `eidetic_core::ensure_heic_registered();`). Plan steps for Path B should make this explicit — minor inconsistency to clean up if Path B is taken.

**Verification-gate enforcement.** Each post-Task-1 task names its pre-condition explicitly and references the specific V# whose outcome shapes the task. Task 3 has Path A/B; Task 4 has an optional V3-mitigation step; Task 5 has Path C/D. No task silently assumes a verification outcome.

**Atomic commits.** Each task ends with a commit. Each commit keeps the workspace green per the project's pre-commit hook (`cargo clippy --workspace --all-targets -- -D warnings`). Task 1 commits docs only (no clippy involvement). Tasks 2–7 all touch code or config and are verified before commit. Task 8 commits nothing.

**No-co-author rule.** No commit message includes a `Co-Authored-By` line, per project convention.

**Conventional Commits.** Each commit subject starts with a `feat:`, `test:`, `build:`, `ci:`, or `docs:` prefix matching AGENTS.md's commit conventions.
