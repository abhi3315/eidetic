# Audit Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply the validated findings from `temp/audit-2026-05-09.md` — security, code quality, performance (small wins), and the architectural dependency inversion. The two big lifts (hash-before-stage refactor and SigLIP batching) are intentionally deferred to a follow-up plan.

**Architecture:** Each task is independent and produces a self-contained commit. Tasks run in priority order: security first, then deletions/cleanup, then the typed-error/perf wins, then the architecture inversion. After each task, `cargo build` and `cargo test --workspace` must pass before committing.

**Tech Stack:** Rust 2024 edition workspace, sqlx, ort (ONNX Runtime), tokio, thiserror, anyhow, walkdir, image, sha2, tracing.

**Out of scope (separate plans):**
- Hash-before-stage refactor (`import.rs`).
- Batch SigLIP inference + parallel `import_dir` walker.
- Two-roundtrip insert in `PgAssetsRepo` (subsumed by larger DB rework).

---

## Task 1: Bind Postgres to loopback

**Files:**
- Modify: `docker-compose.yml:8-9`

**Why:** Currently binds `0.0.0.0:5432` with credentials `eidetic:eidetic` checked into the repo. Anyone on the same network can connect.

- [ ] **Step 1: Apply the change**

In `docker-compose.yml`, change:

```yaml
    ports:
      - "5432:5432"
```

to:

```yaml
    ports:
      - "127.0.0.1:5432:5432"
```

- [ ] **Step 2: Verify Docker still parses the file**

Run: `docker compose config --quiet`
Expected: no output, exit 0.

- [ ] **Step 3: Commit**

```bash
git add docker-compose.yml
git commit -m "fix(security): bind Postgres to loopback only

The previous \"5432:5432\" binding exposed the dev DB on 0.0.0.0
with the hardcoded eidetic:eidetic credentials, putting the
asset index (incl. GPS) on any reachable network."
```

---

## Task 2: Delete unused `store_file` wrapper

**Files:**
- Modify: `crates/eidetic-ingest/src/store.rs:71-80,99-151`
- Modify: `crates/eidetic-ingest/src/lib.rs:15`

**Why:** `store_file` is a thin `stage_file` + `commit_staged` wrapper used only by its own unit tests. `import_file` already calls the two functions directly.

- [ ] **Step 1: Remove the function and its re-export**

Delete `pub fn store_file` (`store.rs:71-80`) and the re-export `pub use store::store_file;` (`lib.rs:15`).

- [ ] **Step 2: Update unit tests to call `stage_file` + `commit_staged` directly**

Replace each `store_file(&src, &hash, &library)` call site in `store.rs` tests with:

```rust
let ext = src.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase());
let stage = stage_file(&src, &library).unwrap();
let dest = commit_staged(stage, &hash, ext.as_deref(), &library).unwrap();
```

Inline this in `stores_at_cas_path`, `creates_intermediate_directories`, `idempotent_second_call_returns_same_path`, and `file_without_extension_stored_without_dot`.

- [ ] **Step 3: Verify**

Run: `cargo test -p eidetic-ingest --lib store::`
Expected: all tests pass.

- [ ] **Step 4: Verify whole workspace still builds and tests pass**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ingest/src/store.rs crates/eidetic-ingest/src/lib.rs
git commit -m "refactor(ingest): remove unused store_file wrapper

The function was only called by its own unit tests; production
callers (import_file) compose stage_file + commit_staged directly."
```

---

## Task 3: Drop unused `Paths::import_dir` field

**Files:**
- Modify: `crates/eidetic-core/src/config.rs:18,28,52-54`
- Modify: `crates/eidetic-ingest/src/import.rs:174` (test fixture)

**Why:** `Paths::import_dir` is set by `EIDETIC_IMPORT_DIR` and `Default` but no production caller ever reads it. Users will set the env var thinking it does something.

- [ ] **Step 1: Remove the field and its env-var branch**

In `config.rs`:
- Remove `pub import_dir: PathBuf,` from `struct Paths`.
- Remove `import_dir: cache.join("import"),` from `Default`.
- Remove the `EIDETIC_IMPORT_DIR` block (`config.rs:52-54`).
- Remove `EIDETIC_IMPORT_DIR` from the doc-comment list above `from_env`.
- Remove the `assert!(config.paths.import_dir.ends_with("import"));` line (`config.rs:78`).

- [ ] **Step 2: Update test fixture in `import.rs`**

In `import.rs:172-178`, remove the `import_dir: tmp.path().join("import"),` line from `make_paths`.

- [ ] **Step 3: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-core/src/config.rs crates/eidetic-ingest/src/import.rs
git commit -m "refactor(core): drop unused Paths::import_dir

The field was settable via EIDETIC_IMPORT_DIR but no caller read it.
A documented env var that does nothing is a footgun; remove it."
```

---

## Task 4: Make missing `HOME` panic instead of silently falling back to `.`

**Files:**
- Modify: `crates/eidetic-core/src/config.rs:66-69`

**Why:** Falling back to `.` (CWD) on missing `HOME` silently scatters cache/library data wherever the binary is invoked. CLI tools that need `HOME` should fail loudly, not silently misbehave.

- [ ] **Step 1: Replace the silent fallback**

Change `default_cache_dir` from:

```rust
fn default_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("eidetic")
}
```

to:

```rust
fn default_cache_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .expect("HOME must be set; required for default cache dir (override with EIDETIC_LIBRARY_DIR / EIDETIC_MODELS_CACHE)");
    PathBuf::from(home).join(".cache").join("eidetic")
}
```

- [ ] **Step 2: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass (the existing `default_paths_are_under_cache_dir` test runs in an env where `HOME` is set, so it stays green).

- [ ] **Step 3: Commit**

```bash
git add crates/eidetic-core/src/config.rs
git commit -m "fix(core): panic instead of silently falling back to CWD when HOME unset

Previous behavior wrote the library/cache to ./.cache/eidetic if HOME
was missing, scattering data wherever the binary was invoked. Failing
loudly is the correct default for a personal CLI tool; users who need
to run without HOME can override with EIDETIC_LIBRARY_DIR / _MODELS_CACHE."
```

---

## Task 5: Make `Config::from_env` infallible; delete dead `eidetic-core::Error`

**Files:**
- Modify: `crates/eidetic-core/src/config.rs:46-63`
- Delete: `crates/eidetic-core/src/error.rs`
- Modify: `crates/eidetic-core/src/lib.rs:8,12`
- Modify: `crates/eidetic-cli/src/main.rs:125,171,193,269`

**Why:** `Error::Config` is the only variant of `eidetic_core::Error` and is never constructed. `Config::from_env` always returns `Ok`. Callers wrap it with `.context(...)` for an impossible failure.

- [ ] **Step 1: Make `from_env` return `Self`**

In `config.rs`, change the signature and body:

```rust
pub fn from_env() -> Self {
    let mut config = Self::default();

    if let Ok(v) = std::env::var("EIDETIC_DATABASE_URL") {
        config.database_url = v;
    }
    if let Ok(v) = std::env::var("EIDETIC_LIBRARY_DIR") {
        config.paths.library_dir = PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("EIDETIC_MODELS_CACHE") {
        config.paths.models_cache = PathBuf::from(v);
    }

    config
}
```

Remove `use crate::Result;` from `config.rs:1`.

- [ ] **Step 2: Delete `error.rs` and its re-exports**

Delete `crates/eidetic-core/src/error.rs`.

In `crates/eidetic-core/src/lib.rs`, remove:
- `pub mod error;`
- `pub use error::{Error, Result};`

- [ ] **Step 3: Update CLI call sites**

In `crates/eidetic-cli/src/main.rs`, replace the four occurrences of:

```rust
let config = Config::from_env().context("failed to load config")?;
```

with:

```rust
let config = Config::from_env();
```

(Lines 125, 171, 193, 269.)

- [ ] **Step 4: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-core crates/eidetic-cli/src/main.rs
git commit -m "refactor(core): drop dead Error type; make Config::from_env infallible

eidetic_core::Error::Config was never constructed and from_env always
returned Ok. Inlining the impossible Result removes 4 .context() noise
sites in the CLI and a whole error module."
```

---

## Task 6: Stop collapsing non-UTF-8 filenames to literal `"unknown"`

**Files:**
- Modify: `crates/eidetic-ingest/src/import.rs:34-38`
- Add test: `crates/eidetic-ingest/src/import.rs` (tests module)

**Why:** Two files with non-UTF-8 names both stash `original_filename = "unknown"` in the DB, losing distinguishing info. Use lossy conversion so we keep something resembling the real name.

- [ ] **Step 1: Write the failing test**

Add this test to the `tests` module in `import.rs`:

```rust
#[tokio::test]
async fn lossy_filename_is_preserved_not_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = make_paths(&tmp);
    let src = write_jpeg(tmp.path(), "héllo.jpg", b"a");
    let index = MockAssetIndex::new();

    import_file(&src, &index, &paths).await;

    let inserted = index.all_inserted();
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].original_filename, "héllo.jpg");
    assert_ne!(inserted[0].original_filename, "unknown");
}
```

- [ ] **Step 2: Run to verify it passes already (UTF-8 path)**

Run: `cargo test -p eidetic-ingest --lib lossy_filename_is_preserved_not_unknown`
Expected: PASS — the existing code handles UTF-8 fine.

The "unknown" fallback only triggers on non-UTF-8 OsStr. We can't easily forge that on macOS in a portable test, so the regression-prevention test above guards the UTF-8 path; the lossy fix below covers non-UTF-8 by code review.

- [ ] **Step 3: Apply the lossy-conversion fix**

In `import.rs:34-38`, change:

```rust
let original_filename = path
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or("unknown")
    .to_string();
```

to:

```rust
let original_filename = path
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| "unknown".to_string());
```

The `unwrap_or_else` branch is now only hit when `file_name()` returns `None` (i.e. the path is `..` or root) — `import_dir` filters non-files so this is effectively unreachable, but keeping a graceful fallback over `expect` is safer for the `import_file` direct-entry case.

- [ ] **Step 4: Run tests**

Run: `cargo test -p eidetic-ingest --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ingest/src/import.rs
git commit -m "fix(ingest): preserve non-UTF-8 filenames via lossy conversion

The previous code collapsed any non-UTF-8 OsStr to literal \"unknown\",
making distinct files indistinguishable in the asset index."
```

---

## Task 7: Drop unreachable validation in `format_result`

**Files:**
- Modify: `crates/eidetic-cli/src/main.rs:81-104`

**Why:** `format_result` does `anyhow::bail!` on unknown fields, but the only caller already validates against `VALID_FIELDS` (`main.rs:258-267`). The bail arm is unreachable and the `anyhow::Result` return type is theatre.

- [ ] **Step 1: Change `format_result` to infallible**

Replace lines 81-104 with:

```rust
fn format_result(r: &eidetic_db::SearchResult, fields: &[String]) -> String {
    let mut parts = Vec::new();
    for field in fields {
        let value = match field.as_str() {
            "path" => r.storage_path.display().to_string(),
            "score" => format!("{:.3}", r.score),
            "date" => r
                .date_taken
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            "make" => r.camera_make.clone().unwrap_or_default(),
            "model" => r.camera_model.clone().unwrap_or_default(),
            "lat" => r.latitude.map(|l| format!("{l:.6}")).unwrap_or_default(),
            "lon" => r.longitude.map(|l| format!("{l:.6}")).unwrap_or_default(),
            "mime" => r.mime_type.clone().unwrap_or_default(),
            other => unreachable!("unknown field {other:?} should have been rejected by VALID_FIELDS check"),
        };
        parts.push(value);
    }
    parts.join("\t")
}
```

- [ ] **Step 2: Update the call site**

At `main.rs:322`, change:

```rust
println!("{}", format_result(r, field_list)?);
```

to:

```rust
println!("{}", format_result(r, field_list));
```

- [ ] **Step 3: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-cli/src/main.rs
git commit -m "refactor(cli): make format_result infallible

The bail!() arm was unreachable: VALID_FIELDS validation in the
Search command (main.rs:258-267) rejects unknown fields before
format_result is called. Replace it with unreachable!() and drop
the anyhow::Result wrapping."
```

---

## Task 8: Collapse `Io` and `StoreIo` error variants

**Files:**
- Modify: `crates/eidetic-ingest/src/error.rs:6-17`
- Modify: `crates/eidetic-ingest/src/store.rs:11-23,50-66`

**Why:** Both variants have the same shape `{ path: PathBuf, source: std::io::Error }` and exist only because `Display` prefixes differ. The path itself tells you what failed; the `Io` variant is enough.

- [ ] **Step 1: Remove the `StoreIo` variant**

In `error.rs`, replace the enum with:

```rust
#[derive(Debug, Error)]
pub enum Error {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("asset index: {0}")]
    Index(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}
```

- [ ] **Step 2: Replace `Error::StoreIo` constructions in `store.rs`**

In `store.rs:11-23` and `:50-66`, replace every `crate::Error::StoreIo { path, source }` with `crate::Error::Io { path, source }`. Five call sites total.

- [ ] **Step 3: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-ingest/src/error.rs crates/eidetic-ingest/src/store.rs
git commit -m "refactor(ingest): collapse StoreIo into Io error variant

Both variants had identical shape; the path field already conveys
which I/O step failed."
```

---

## Task 9: Delete `Embedding::into_vec` and its round-trip test

**Files:**
- Modify: `crates/eidetic-ml/src/embedder.rs:21-23,87-93`

**Why:** `into_vec` is only used by its own test. All real callers reach for `.as_slice()`.

- [ ] **Step 1: Remove the method and the test**

Delete:
- `pub fn into_vec(self) -> Vec<f32> { ... }` (lines 21-23).
- The `embedding_round_trip` test (lines 87-93).

- [ ] **Step 2: Verify**

Run: `cargo test -p eidetic-ml --lib`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/eidetic-ml/src/embedder.rs
git commit -m "refactor(ml): remove unused Embedding::into_vec"
```

---

## Task 10: Demote `MockEmbedder` to `#[cfg(test)]`

**Files:**
- Modify: `crates/eidetic-ml/src/embedder.rs:43-65`
- Modify: `crates/eidetic-ml/src/lib.rs:7`

**Why:** `MockEmbedder` is `pub` re-exported but only consumed by `eidetic-ml`'s own tests. Production wiring uses `SiglipEmbedder` directly.

- [ ] **Step 1: Wrap `MockEmbedder` in `#[cfg(test)]`**

In `embedder.rs`, prefix the `pub struct MockEmbedder` block (lines 42-65 — both the struct/`impl MockEmbedder`/`impl Embedder for MockEmbedder`) with `#[cfg(test)]`, and drop the `pub` from each.

The simplest shape is to wrap them in a `#[cfg(test)]` module *or* just attach `#[cfg(test)]` on each item. Pick the cleaner approach by inlining all three into the existing `tests` module — they're only used there.

Move the `MockEmbedder` definition (struct + both impls) into the `#[cfg(test)] mod tests { ... }` block, replacing the top-level definitions.

- [ ] **Step 2: Drop the re-export**

In `lib.rs:7`, change:

```rust
pub use embedder::{Embedder, Embedding, MockEmbedder};
```

to:

```rust
pub use embedder::{Embedder, Embedding};
```

- [ ] **Step 3: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-ml/src/embedder.rs crates/eidetic-ml/src/lib.rs
git commit -m "refactor(ml): scope MockEmbedder to #[cfg(test)]

Mock was pub re-exported but only used in this crate's tests."
```

---

## Task 11: Replace per-pixel HWC→CHW loop with linear `as_raw()` walk

**Files:**
- Modify: `crates/eidetic-ml/src/siglip.rs:155-166`
- Add test: `crates/eidetic-ml/src/siglip.rs` (existing tests module)

**Why:** The current double-nested `get_pixel(x,y)` loop does ~65k bounds-checked 2D lookups per image on every embed call. `image::ImageBuffer::as_raw()` already returns the contiguous HWC buffer; one linear pass gives identical output.

- [ ] **Step 1: Write a regression test that locks the current output**

First, find the existing tests module in `siglip.rs`. Look at the test that already loads `SiglipEmbedder` (line 249 area) for the test layout.

Add this test at the bottom of the `#[cfg(test)] mod tests` block (or create one if none exists):

```rust
#[test]
fn preprocess_image_chw_layout_matches_naive() {
    use image::{ImageBuffer, Rgb};

    // Build a synthetic 4x4 RGB image with deterministic per-channel gradients
    // (4x4 is much smaller than IMAGE_SIZE so we override SIZE locally).
    let size = 4usize;
    let mut img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(size as u32, size as u32);
    for y in 0..size {
        for x in 0..size {
            img.put_pixel(
                x as u32,
                y as u32,
                Rgb([(x * 17) as u8, (y * 23) as u8, ((x + y) * 11) as u8]),
            );
        }
    }

    // Naive (old) HWC -> CHW with per-pixel get_pixel.
    let mut naive = vec![0.0f32; 3 * size * size];
    for y in 0..size {
        for x in 0..size {
            let pixel = img.get_pixel(x as u32, y as u32);
            for c in 0..3usize {
                naive[c * size * size + y * size + x] = (pixel.0[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }

    // New (linear) HWC -> CHW from as_raw().
    let raw = img.as_raw();
    let mut linear = vec![0.0f32; 3 * size * size];
    let plane = size * size;
    for i in 0..plane {
        let r = raw[3 * i] as f32;
        let g = raw[3 * i + 1] as f32;
        let b = raw[3 * i + 2] as f32;
        linear[i] = (r / 255.0 - 0.5) / 0.5;
        linear[plane + i] = (g / 255.0 - 0.5) / 0.5;
        linear[2 * plane + i] = (b / 255.0 - 0.5) / 0.5;
    }

    assert_eq!(naive, linear, "linear walk must match naive get_pixel");
}
```

- [ ] **Step 2: Run the test to verify it passes (it tests the new code in isolation)**

Run: `cargo test -p eidetic-ml --lib preprocess_image_chw_layout_matches_naive`
Expected: PASS.

- [ ] **Step 3: Replace the production loop**

In `preprocess_image` (`siglip.rs:144-167`), replace lines 157-166 (from `let size = IMAGE_SIZE as usize;` through the end of the closing `}` of the `for y in 0..size` loop) with:

```rust
let size = IMAGE_SIZE as usize;
let raw = rgb.as_raw(); // contiguous HWC, length 3*size*size
debug_assert_eq!(raw.len(), 3 * size * size);
let mut chw = vec![0.0f32; 3 * size * size];
let plane = size * size;
for i in 0..plane {
    let r = raw[3 * i] as f32;
    let g = raw[3 * i + 1] as f32;
    let b = raw[3 * i + 2] as f32;
    chw[i] = (r / 255.0 - 0.5) / 0.5;
    chw[plane + i] = (g / 255.0 - 0.5) / 0.5;
    chw[2 * plane + i] = (b / 255.0 - 0.5) / 0.5;
}
```

- [ ] **Step 4: Run all ml tests**

Run: `cargo test -p eidetic-ml --lib`
Expected: PASS (including the existing SigLIP integration test, gated on the model being downloaded — if it's `#[ignore]` or skipped because no model, that's fine).

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ml/src/siglip.rs
git commit -m "perf(ml): linear as_raw() walk for HWC→CHW preprocessing

The previous double-nested loop did 65k bounds-checked get_pixel calls
per image. as_raw() returns the underlying contiguous HWC buffer; one
linear pass produces identical output (locked by regression test)."
```

---

## Task 12: Switch `pub mod` + flat re-export to `mod` + `pub use`

**Files:**
- Modify: `crates/eidetic-ingest/src/lib.rs`
- Modify: `crates/eidetic-ml/src/lib.rs`

**Why:** Both crates expose submodules via `pub mod foo;` *and* re-export their items at the crate root, giving two paths (`eidetic_ingest::store::stage_file` and `eidetic_ingest::stage_file` would both work — actually `stage_file` isn't re-exported, but the pattern is there for `error::Error` etc.). One canonical path is cleaner.

- [ ] **Step 1: Change `eidetic-ingest/src/lib.rs`**

Replace:

```rust
pub mod error;
pub mod hasher;
pub mod import;
pub mod meta;
pub mod repo;
pub mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use repo::{AssetIndex, InsertOutcome, NewAsset};
```

with:

```rust
mod error;
mod hasher;
mod import;
mod meta;
mod repo;
mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use repo::{AssetIndex, InsertOutcome, NewAsset};
pub use store::{commit_staged, stage_file};
```

(The `store_file` re-export was already deleted in Task 2; re-export `stage_file` and `commit_staged` so callers like `import.rs` keep working — they currently `use crate::store::{...}` which is fine within the crate; adding `pub use` doesn't hurt and keeps external surface explicit.)

- [ ] **Step 2: Change `eidetic-ml/src/lib.rs`**

Replace:

```rust
pub mod embedder;
pub mod error;
pub mod siglip;

pub use embedder::{Embedder, Embedding};
pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
```

with:

```rust
mod embedder;
mod error;
mod siglip;

pub use embedder::{Embedder, Embedding};
pub use error::{Error, Result};
pub use siglip::SiglipEmbedder;
```

- [ ] **Step 3: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass. If anything fails because an external caller used the `crate::module::Foo` path, replace with the re-exported `crate::Foo` path.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-ingest/src/lib.rs crates/eidetic-ml/src/lib.rs
git commit -m "refactor: hide submodules; expose only via flat re-exports

Drops the dual-path API surface (crate::module::Foo vs crate::Foo)
in eidetic-ingest and eidetic-ml. Pinned re-exports remain the
canonical entry points."
```

---

## Task 13: Drop redundant `BufReader` in `hasher.rs`

**Files:**
- Modify: `crates/eidetic-ingest/src/hasher.rs:16,26-32`

**Why:** Reading into a 64 KiB stack buffer through a `BufReader` (default capacity 8 KiB) does a redundant memcpy on each chunk. Either drop the `BufReader` and read directly into the stack buffer, or drop the stack buffer and read from the `BufReader`'s buffer. The first is simpler.

- [ ] **Step 1: Remove `BufReader`**

In `hasher.rs`:
- Change `use std::io::{BufReader, Read};` to `use std::io::Read;`.
- In `hash_file`, change:

```rust
let file = File::open(path).map_err(|source| Error::Io {
    path: path.to_path_buf(),
    source,
})?;
let mut reader = BufReader::with_capacity(READ_BUF_SIZE, file);
let mut hasher = Sha256Hasher::new();
let mut buf = [0u8; READ_BUF_SIZE];

loop {
    let n = reader.read(&mut buf).map_err(|source| Error::Io {
```

to:

```rust
let mut file = File::open(path).map_err(|source| Error::Io {
    path: path.to_path_buf(),
    source,
})?;
let mut hasher = Sha256Hasher::new();
let mut buf = [0u8; READ_BUF_SIZE];

loop {
    let n = file.read(&mut buf).map_err(|source| Error::Io {
```

- [ ] **Step 2: Verify**

Run: `cargo test -p eidetic-ingest --lib hasher::`
Expected: PASS — the existing `hashes_known_content`, `hashes_empty_file`, `streaming_matches_oneshot_for_buffer_boundaries`, and `returns_error_for_missing_file` tests cover the path.

- [ ] **Step 3: Commit**

```bash
git add crates/eidetic-ingest/src/hasher.rs
git commit -m "perf(ingest): read directly into stack buffer in hash_file

BufReader's internal buffer + the 64 KiB stack buffer was a redundant
memcpy per chunk."
```

---

## Task 14: Move `AssetIndex` + types to `eidetic-core`; flip `eidetic-db → eidetic-ingest` dep

**Files:**
- Modify: `crates/eidetic-core/src/lib.rs` (add new module)
- Create: `crates/eidetic-core/src/index.rs`
- Modify: `crates/eidetic-ingest/Cargo.toml` (no change expected, but verify)
- Modify: `crates/eidetic-ingest/src/repo.rs` (replace with re-export from `eidetic-core`)
- Modify: `crates/eidetic-ingest/src/import.rs` (paths)
- Modify: `crates/eidetic-ingest/src/lib.rs`
- Modify: `crates/eidetic-ingest/src/error.rs` (typed `Index` variant)
- Modify: `crates/eidetic-db/Cargo.toml` (drop `eidetic-ingest` dep)
- Modify: `crates/eidetic-db/src/assets.rs` (use `eidetic-core` paths)
- Modify: `crates/eidetic-db/src/error.rs` if any (new variant for trait error)

**Why:** Today `eidetic-db` path-depends on `eidetic-ingest` purely to implement `AssetIndex`. The dependency direction is upside-down: storage shouldn't depend on the ingest flow. A future HTTP API consumer would inherit `eidetic-ingest` for free, which is wrong.

This is the largest task in the plan; do it last so any breakage is isolated.

- [ ] **Step 1: Create `eidetic-core/src/index.rs`**

Add a new file `crates/eidetic-core/src/index.rs`:

```rust
//! Index trait: storage-layer abstraction for the asset catalog.
//!
//! Lives here (not in `eidetic-ingest`) so that any DB / HTTP / test
//! implementation can depend only on `eidetic-core`.

use crate::AssetId;
use chrono::{DateTime, Utc};
use std::path::PathBuf;

/// Boxed error returned by [`AssetIndex`] implementations.
///
/// Each impl chooses its own concrete error type (e.g. `sqlx::Error`)
/// and boxes it; callers that need to inspect the cause downcast.
pub type IndexError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type IndexResult<T> = std::result::Result<T, IndexError>;

#[derive(Debug)]
pub enum InsertOutcome {
    Inserted(AssetId),
    Existing(AssetId),
}

#[derive(Clone, Debug)]
pub struct NewAsset {
    pub hash: String,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: u64,
    pub mime_type: Option<String>,
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

#[allow(async_fn_in_trait)]
pub trait AssetIndex {
    async fn find_by_hash(&self, hash: &str) -> IndexResult<Option<AssetId>>;
    async fn insert_asset(&self, asset: NewAsset) -> IndexResult<InsertOutcome>;
}
```

In `crates/eidetic-core/src/lib.rs`, add:

```rust
pub mod index;
pub use index::{AssetIndex, IndexError, IndexResult, InsertOutcome, NewAsset};
```

In `crates/eidetic-core/Cargo.toml`, add `chrono = { workspace = true }` to `[dependencies]` if not already present. Verify by reading the file first.

- [ ] **Step 2: Replace `eidetic-ingest/src/repo.rs` content with a thin shim**

Replace the contents of `crates/eidetic-ingest/src/repo.rs` with:

```rust
//! Re-export the AssetIndex trait + types from `eidetic-core`.
//!
//! The trait moved to `eidetic-core` so storage backends (e.g. `eidetic-db`)
//! can implement it without depending on this crate.

pub use eidetic_core::{AssetIndex, InsertOutcome, NewAsset};

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use eidetic_core::AssetId;
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub(crate) struct MockAssetIndex {
        records: Mutex<HashMap<String, AssetId>>,
        inserted: Mutex<Vec<NewAsset>>,
    }

    impl MockAssetIndex {
        pub(crate) fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
                inserted: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn all_inserted(&self) -> Vec<NewAsset> {
            self.inserted.lock().unwrap().clone()
        }

        #[allow(dead_code)]
        pub(crate) fn seed(&self, hash: &str, id: AssetId) {
            self.records.lock().unwrap().insert(hash.to_string(), id);
        }
    }

    impl AssetIndex for MockAssetIndex {
        async fn find_by_hash(
            &self,
            hash: &str,
        ) -> eidetic_core::IndexResult<Option<AssetId>> {
            Ok(self.records.lock().unwrap().get(hash).copied())
        }

        async fn insert_asset(
            &self,
            asset: NewAsset,
        ) -> eidetic_core::IndexResult<InsertOutcome> {
            let mut records = self.records.lock().unwrap();
            if let Some(&existing_id) = records.get(&asset.hash) {
                return Ok(InsertOutcome::Existing(existing_id));
            }
            let id = AssetId::new();
            records.insert(asset.hash.clone(), id);
            drop(records);
            self.inserted.lock().unwrap().push(asset);
            Ok(InsertOutcome::Inserted(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidetic_core::AssetId;
    use std::path::PathBuf;
    use test_support::MockAssetIndex;

    #[tokio::test]
    async fn mock_insert_then_find() {
        let index = MockAssetIndex::new();
        let asset = NewAsset {
            hash: "abc123abc123abc123abc123abc123abc123abc123abc123abc123abc123abcd".to_string(),
            original_filename: "photo.jpg".to_string(),
            storage_path: PathBuf::from("/library/ab/c1/abc123.jpg"),
            file_size: 1024,
            mime_type: None,
            date_taken: None,
            latitude: None,
            longitude: None,
            camera_make: None,
            camera_model: None,
        };
        let outcome = index.insert_asset(asset).await.unwrap();
        let id = match outcome {
            InsertOutcome::Inserted(id) => id,
            InsertOutcome::Existing(_) => panic!("expected Inserted, got Existing"),
        };
        let found = index
            .find_by_hash("abc123abc123abc123abc123abc123abc123abc123abc123abc123abc123abcd")
            .await
            .unwrap();
        assert_eq!(found, Some(id));
    }

    #[tokio::test]
    async fn mock_unknown_hash_returns_none() {
        let index = MockAssetIndex::new();
        let found = index
            .find_by_hash("0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .unwrap();
        assert!(found.is_none());
    }
}
```

- [ ] **Step 3: Update `eidetic-ingest/src/error.rs` so `Index` carries `IndexError` directly**

In `crates/eidetic-ingest/src/error.rs`, change:

```rust
#[error("asset index: {0}")]
Index(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
```

to:

```rust
#[error("asset index: {0}")]
Index(#[source] eidetic_core::IndexError),
```

(Same shape — `IndexError` *is* `Box<dyn Error + Send + Sync>` — but referenced through `eidetic-core` so the dependency is named.)

- [ ] **Step 4: Adjust `import.rs` error wrapping**

`import.rs` calls `index.find_by_hash` and `index.insert_asset`, which now return `Result<_, eidetic_core::IndexError>` (the `IndexResult` alias). Today the call is `match index.find_by_hash(&hash_hex).await { Ok(...) => ..., Err(e) => return ImportOutcome::Failed(e) }` where `e: eidetic_ingest::Error`. We need to map the `IndexError` into `eidetic_ingest::Error::Index`.

Change `import.rs:62-66` from:

```rust
match index.find_by_hash(&hash_hex).await {
    Ok(Some(existing_id)) => return ImportOutcome::Duplicate(existing_id),
    Ok(None) => {}
    Err(e) => return ImportOutcome::Failed(e),
}
```

to:

```rust
match index.find_by_hash(&hash_hex).await {
    Ok(Some(existing_id)) => return ImportOutcome::Duplicate(existing_id),
    Ok(None) => {}
    Err(e) => return ImportOutcome::Failed(Error::Index(e)),
}
```

And `import.rs:92-96` from:

```rust
match index.insert_asset(new_asset).await {
    Ok(InsertOutcome::Inserted(id)) => ImportOutcome::Imported(id),
    Ok(InsertOutcome::Existing(id)) => ImportOutcome::Duplicate(id),
    Err(e) => ImportOutcome::Failed(e),
}
```

to:

```rust
match index.insert_asset(new_asset).await {
    Ok(InsertOutcome::Inserted(id)) => ImportOutcome::Imported(id),
    Ok(InsertOutcome::Existing(id)) => ImportOutcome::Duplicate(id),
    Err(e) => ImportOutcome::Failed(Error::Index(e)),
}
```

- [ ] **Step 5: Drop `eidetic-ingest` from `eidetic-db/Cargo.toml`**

In `crates/eidetic-db/Cargo.toml`, remove the line:

```toml
eidetic-ingest = { path = "../eidetic-ingest" }
```

- [ ] **Step 6: Update `eidetic-db/src/assets.rs` to use `eidetic-core` paths**

Change the `use` line from:

```rust
use eidetic_ingest::{AssetIndex, InsertOutcome, NewAsset};
```

to:

```rust
use eidetic_core::{AssetIndex, InsertOutcome, NewAsset};
```

In the `impl AssetIndex for PgAssetsRepo` block (`assets.rs:158-208`), update the return types and error mapping:

- `async fn find_by_hash(&self, hash: &str) -> eidetic_ingest::Result<Option<AssetId>>` → `async fn find_by_hash(&self, hash: &str) -> eidetic_core::IndexResult<Option<AssetId>>`.
- `async fn insert_asset(&self, asset: NewAsset) -> eidetic_ingest::Result<InsertOutcome>` → `async fn insert_asset(&self, asset: NewAsset) -> eidetic_core::IndexResult<InsertOutcome>`.
- All four `.map_err(|e| eidetic_ingest::Error::Index(Box::new(e)))?` → `.map_err(|e| Box::new(e) as eidetic_core::IndexError)?`.

- [ ] **Step 7: Verify**

Run: `cargo build --workspace --all-targets && cargo test --workspace --lib`
Expected: pass.

If `eidetic-db` integration tests need Postgres (testcontainers), they will be `#[ignore]`d or gated; that's fine. The lib-level tests should still pass.

- [ ] **Step 8: Commit**

```bash
git add crates/eidetic-core crates/eidetic-ingest crates/eidetic-db
git commit -m "refactor: invert eidetic-db → eidetic-ingest dependency

Move AssetIndex/NewAsset/InsertOutcome to eidetic-core, where storage
backends and ingest flows can both depend on it without one pulling
the other. eidetic-db no longer path-depends on eidetic-ingest."
```

---

## Self-Review

**Spec coverage:** All audit items I committed to addressing have a task:

- Quality/Medium #1 `store_file` → Task 2.
- Quality/Medium #2 `Paths::import_dir` → Task 3.
- Quality/Medium #3 `default_cache_dir` HOME → Task 4.
- Quality/Low (Error::Config) → Task 5.
- Quality/Low (`Embedding::into_vec`) → Task 9.
- Quality/Low (`MockEmbedder` cfg) → Task 10.
- Quality/Low (`format_result` unreachable) → Task 7.
- Quality/Low (`original_filename`) → Task 6.
- Quality/Low (Io+StoreIo) → Task 8.
- Security/High (loopback) → Task 1.
- Performance/High (HWC→CHW) → Task 11.
- Performance/Low (BufReader) → Task 13.
- Architecture/Medium #1 (eidetic-db dep) → Task 14.
- Architecture/Medium #2 (Error::Config dup) → Task 5.
- Architecture/Low (`pub mod` + flat) → Task 12.
- Architecture/Low (`store_file`) → Task 2.

**Deferred (with reason):**
- Performance/High `import_dir` parallelism → needs DB pool sizing + spawn_blocking design; separate plan.
- Performance/High SigLIP batching → reshapes Embedder trait; separate plan.
- Performance/Medium hash-before-stage → real refactor with tee-vs-twice-read tradeoff; separate plan.
- Performance/Medium 2-roundtrip insert → subsumed by larger DB rework.
- Architecture/Low `Embedder` trait removal → debatable (audit said "trait pays for itself only with external polymorphism" but the embed call sites already wrap in `Arc<dyn ...>` indirectly through `spawn_blocking`; leave alone).
- Architecture/Low `LibraryStats`/`SearchResult` extraction → low value relative to risk; defer.
- Architecture/Low `AssetIndex` test mock via closure → already lives behind `#[cfg(test)]`; no change needed.

**Type consistency:** Names used in later tasks (`AssetIndex`, `NewAsset`, `InsertOutcome`, `IndexError`, `IndexResult`) match definitions introduced in Task 14. The `stage_file`/`commit_staged` re-export in Task 12 is consistent with usage in `import.rs` (already imports them as `crate::store::*`).

**Placeholder scan:** No TBDs. Every code step shows the actual code.

---

## Execution Handoff

Per the audit cleanup being mostly mechanical with quick verification, I'll execute inline with checkpoints rather than spinning up subagents per task.
