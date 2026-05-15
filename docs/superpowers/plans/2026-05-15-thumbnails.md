# Thumbnails Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two-size JPEG thumbnails (256px + 1024px longest edge) generated for every image during `eidetic import`, with an `eidetic thumbnail` backfill command. Failure-mode parity with embeddings: bad inputs warn and leave the row unthumbnailed for retry.

**Architecture:** New `eidetic-ingest::thumbnail` module handles decode → resize → JPEG-encode → write. `import_file` calls it after `commit_staged` for image assets; failures warn and set `thumbnails_generated = false`. New `assets.thumbnails_generated BOOLEAN` column plus repo methods `fetch_unthumbnailed` and `mark_thumbnailed` power the backfill subcommand. Path scheme: `library_dir/.thumbs/<s|m>/<hash[..2]>/<hash[2..4]>/<hash>.jpg`.

**Tech Stack:** Rust 2024 workspace, `image` 0.25 (decode + resize + JPEG encode — all default features, zero new deps), `sqlx`, `tokio` (`spawn_blocking` per asset), `thiserror`.

**Spec deviation:** The design doc said WebP quality 85. The `image` crate's built-in WebP encoder is lossless-only as of 0.25, which would produce files larger than the JPEG it replaces. Switching to JPEG quality 85 keeps the "zero new dependencies" constraint and matches the classic personal-thumbnail format. File extension `.jpg`, MIME type implied by extension, the rest of the design is unchanged.

---

## File map

**Create:**
- `migrations/004_thumbnails.sql` — `ALTER TABLE assets ADD COLUMN thumbnails_generated`.
- `crates/eidetic-ingest/src/thumbnail.rs` — `ThumbSize`, `generate_thumbnails`, `thumbnail_path`, decompression-bomb-limited image loader, JPEG encoder. Unit tests in-file.
- `crates/eidetic-db/tests/thumbnail_integration.rs` — testcontainer-backed integration tests for `fetch_unthumbnailed` / `mark_thumbnailed` and the end-to-end backfill flow.

**Modify:**
- `crates/eidetic-db/src/assets.rs` — `NewAsset` gains `thumbnails_generated`; `insert_asset` SQL writes the column; `LibraryStats` gains `thumbnails_pending`; `fetch_stats` SQL adds one count expression; new methods `fetch_unthumbnailed` and `mark_thumbnailed`.
- `crates/eidetic-ingest/src/lib.rs` — `pub mod thumbnail;`.
- `crates/eidetic-ingest/src/import.rs` — call `generate_thumbnails` in `import_file` for image MIME types; pass `thumbnails_generated` to `NewAsset`.
- `crates/eidetic-ingest/Cargo.toml` — add `image = { workspace = true }`.
- `crates/eidetic-db/tests/assets.rs` — every `NewAsset { ... }` literal gets `thumbnails_generated: false`.
- `crates/eidetic-db/tests/import_integration.rs` — same.
- `crates/eidetic-cli/src/main.rs` — `Command::Thumbnail` variant + arm body; `Stats` output adds one line.
- `AGENTS.md` — small note in the `eidetic-ingest` workspace-map row.

---

## Task 1: Migration + `NewAsset` field + `insert_asset` SQL

**Goal of this task:** Add the DB column and the type-level field. Keep workspace green by simultaneously updating every `NewAsset { ... }` literal with `thumbnails_generated: false`. Migration is forward-only and `cargo build` doesn't run migrations, so the SQL is harmless until a Postgres connects.

**Files:**
- Create: `migrations/004_thumbnails.sql`
- Modify: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-ingest/src/import.rs:83`
- Modify: `crates/eidetic-db/tests/assets.rs` (8 `NewAsset` literals)
- Modify: `crates/eidetic-db/tests/import_integration.rs` (3 `NewAsset` literals)

- [ ] **Step 1: Create branch off main**

```bash
git checkout main
git pull --ff-only
git checkout -b feat/thumbnails  # if not already created from the spec commit
```

If the branch already exists from the spec commit, just `git checkout feat/thumbnails` and skip ahead.

- [ ] **Step 2: Write `migrations/004_thumbnails.sql`**

Create the file with:

```sql
-- Thumbnails: per-asset generation tracking.
--
-- `thumbnails_generated = TRUE` only when both the 256px and 1024px
-- JPEG thumbnails wrote successfully to the on-disk CAS-shaped path
-- under `<library_dir>/.thumbs/{s,m}/`. False is the default and
-- the retry state — `eidetic thumbnail` finds these rows.

ALTER TABLE assets
    ADD COLUMN thumbnails_generated BOOLEAN NOT NULL DEFAULT FALSE;
```

- [ ] **Step 3: Add the field to `NewAsset` in `crates/eidetic-db/src/assets.rs`**

Find the `NewAsset` struct (around line 11). Append `thumbnails_generated` after `camera_model`:

```rust
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
    pub thumbnails_generated: bool,
}
```

- [ ] **Step 4: Update `insert_asset` SQL in `crates/eidetic-db/src/assets.rs`**

Find the `insert_asset` method (it lives inside `impl PgAssetsRepo`). The current INSERT statement reads:

```rust
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type, \
              date_taken, latitude, longitude, camera_make, camera_model) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (hash) DO NOTHING \
             RETURNING id",
        )
        .bind(new_id.as_uuid())
        .bind(&asset.hash)
        .bind(&asset.original_filename)
        .bind(asset.storage_path.to_string_lossy().as_ref())
        .bind(asset.file_size as i64)
        .bind(asset.mime_type.as_deref())
        .bind(asset.date_taken)
        .bind(asset.latitude)
        .bind(asset.longitude)
        .bind(asset.camera_make.as_deref())
        .bind(asset.camera_model.as_deref())
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::Error::Query)?;
```

Add the `thumbnails_generated` column and corresponding `$12` bind:

```rust
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "INSERT INTO assets \
             (id, hash, original_filename, storage_path, file_size, mime_type, \
              date_taken, latitude, longitude, camera_make, camera_model, \
              thumbnails_generated) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (hash) DO NOTHING \
             RETURNING id",
        )
        .bind(new_id.as_uuid())
        .bind(&asset.hash)
        .bind(&asset.original_filename)
        .bind(asset.storage_path.to_string_lossy().as_ref())
        .bind(asset.file_size as i64)
        .bind(asset.mime_type.as_deref())
        .bind(asset.date_taken)
        .bind(asset.latitude)
        .bind(asset.longitude)
        .bind(asset.camera_make.as_deref())
        .bind(asset.camera_model.as_deref())
        .bind(asset.thumbnails_generated)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::Error::Query)?;
```

The `ON CONFLICT` fallback path is unchanged — duplicates by hash keep their existing row's thumbnail state.

- [ ] **Step 5: Update `crates/eidetic-ingest/src/import.rs` to pass the new field**

Find the `NewAsset { ... }` literal at line 83 in `import_file`. The current value:

```rust
    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path,
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
    };
```

Add `thumbnails_generated: false` at the end. This is a placeholder for now; Task 3 replaces the literal `false` with a computed value once `generate_thumbnails` is wired in:

```rust
    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path,
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
        thumbnails_generated: false,
    };
```

- [ ] **Step 6: Update every `NewAsset` literal in tests**

Eleven test files / locations need `thumbnails_generated: false` appended:

- `crates/eidetic-db/tests/assets.rs:34` — `let asset = NewAsset { … }`
- `crates/eidetic-db/tests/assets.rs:89` — same
- `crates/eidetic-db/tests/assets.rs:139` — `let make_asset = || NewAsset { … }` (closure body)
- `crates/eidetic-db/tests/assets.rs:179` — `let asset_a = NewAsset { … }`
- `crates/eidetic-db/tests/assets.rs:191` — `let asset_b = NewAsset { … }`
- `crates/eidetic-db/tests/assets.rs:232` — `let asset = NewAsset { … }`
- `crates/eidetic-db/tests/assets.rs:281` — `let asset_a = NewAsset { … }`
- `crates/eidetic-db/tests/assets.rs:293` — `let asset_b = NewAsset { … }`
- `crates/eidetic-db/tests/assets.rs:346` — `let make_asset = |hash: &str, name: &str| NewAsset { … }` (closure)
- `crates/eidetic-db/tests/import_integration.rs:109` — seed insert inside `known_hash_returns_duplicate`
- `crates/eidetic-db/tests/import_integration.rs:192` — seed inside `dir_counts_duplicates_separately`
- `crates/eidetic-db/tests/import_integration.rs:276` — seed inside `duplicate_import_does_not_touch_library_dir`

For each: append `thumbnails_generated: false,` as the last field before the closing brace. Example for `tests/assets.rs:34`:

```rust
    let asset = NewAsset {
        hash: hash.to_string(),
        original_filename: "test.jpg".to_string(),
        storage_path: PathBuf::from("/library/aa/aa/aaaa1111.jpg"),
        file_size: 2048,
        mime_type: None,
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    };
```

Apply the same pattern at every site. Don't try to be clever (e.g. `Default::default()` for spread) — straightforward literal addition keeps the diff readable.

- [ ] **Step 7: Verify the workspace compiles**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green. If a `NewAsset` literal was missed, the compile error tells you exactly which file and line.

- [ ] **Step 8: Commit**

```bash
git add migrations/004_thumbnails.sql \
        crates/eidetic-db/src/assets.rs \
        crates/eidetic-db/tests/assets.rs \
        crates/eidetic-db/tests/import_integration.rs \
        crates/eidetic-ingest/src/import.rs
git commit -m "feat(db): add thumbnails_generated column

Migration 004 adds a BOOLEAN NOT NULL DEFAULT FALSE column to
assets. NewAsset gains the matching field; insert_asset writes
it. All existing NewAsset construction sites updated to set
thumbnails_generated: false — this is a placeholder until the
thumbnail-generation step wires into import_file."
```

---

## Task 2: New `thumbnail` module + unit tests

**Goal:** Land the pure thumbnail-generation function. Workspace stays green because the module is exported but has no caller in prod yet (Task 3 wires it in). Tests in-file cover the path-derivation, both-sizes-produced contract, and the failure path.

**Files:**
- Create: `crates/eidetic-ingest/src/thumbnail.rs`
- Modify: `crates/eidetic-ingest/src/lib.rs` (add `pub mod thumbnail;`)
- Modify: `crates/eidetic-ingest/Cargo.toml` (add `image = { workspace = true }`)

- [ ] **Step 1: Add `image` to `crates/eidetic-ingest/Cargo.toml`**

Current `[dependencies]` block (verify by reading the file first; the order should remain alphabetical-ish after internal-path deps):

```toml
[dependencies]
eidetic-core = { path = "../eidetic-core" }
eidetic-db = { path = "../eidetic-db" }
sha2 = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true }
walkdir = { workspace = true }
chrono = { workspace = true }
infer = { workspace = true }
kamadak-exif = { workspace = true }
tempfile = { workspace = true }
```

Add `image = { workspace = true }` (alphabetical position: after `chrono`, before `infer`):

```toml
[dependencies]
eidetic-core = { path = "../eidetic-core" }
eidetic-db = { path = "../eidetic-db" }
sha2 = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true }
walkdir = { workspace = true }
chrono = { workspace = true }
image = { workspace = true }
infer = { workspace = true }
kamadak-exif = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 2: Create `crates/eidetic-ingest/src/thumbnail.rs`**

Full file content:

```rust
//! Thumbnail generation for images.
//!
//! Two JPEG sizes (256/1024 longest edge), stored under
//! `<library_dir>/.thumbs/{s,m}/<hash[..2]>/<hash[2..4]>/<hash>.jpg`.

use crate::{Error, Result};
use eidetic_core::Sha256;
use image::ImageFormat;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use std::path::{Path, PathBuf};

/// JPEG quality used for all sizes.
///
/// 85 is the classic photo-thumbnail value: visually indistinguishable
/// from quality 95 at human distances, ~30% smaller. The personal-use
/// trade-off favours bytes saved.
const JPEG_QUALITY: u8 = 85;

/// Thumbnail size variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbSize {
    /// 256px longest edge — grid previews, search result strips.
    Small,
    /// 1024px longest edge — single-photo card view.
    Medium,
}

impl ThumbSize {
    /// Directory segment under `<library_dir>/.thumbs/`. Stable for the
    /// lifetime of the data — adding a new size adds a new letter, never
    /// renames an existing one.
    pub fn dir_segment(self) -> &'static str {
        match self {
            ThumbSize::Small => "s",
            ThumbSize::Medium => "m",
        }
    }

    /// Longest edge in pixels. Aspect ratio is preserved during resize.
    pub fn longest_edge(self) -> u32 {
        match self {
            ThumbSize::Small => 256,
            ThumbSize::Medium => 1024,
        }
    }
}

/// On-disk path for a thumbnail. Derived purely from `hash` and `size`;
/// no DB lookup needed to locate the file.
pub fn thumbnail_path(library_dir: &Path, hash: &Sha256, size: ThumbSize) -> PathBuf {
    let hex = hash.to_string();
    library_dir
        .join(".thumbs")
        .join(size.dir_segment())
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(format!("{hex}.jpg"))
}

/// Generate both 256px and 1024px JPEG thumbnails for `src`.
///
/// Synchronous; CPU-bound. Call inside `tokio::task::spawn_blocking`.
///
/// Returns `Ok(())` only when both thumbnails wrote successfully. On
/// failure of either size, returns `Err`; partial files (if any) are
/// left on disk for a future run to overwrite.
pub fn generate_thumbnails(src: &Path, hash: &Sha256, library_dir: &Path) -> Result<()> {
    let img = load_image_with_limits(src)?;

    for size in [ThumbSize::Small, ThumbSize::Medium] {
        let dest = thumbnail_path(library_dir, hash, size);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let edge = size.longest_edge();
        let resized = img.resize(edge, edge, FilterType::Lanczos3);

        let rgb = resized.to_rgb8();

        let mut bytes: Vec<u8> = Vec::new();
        let encoder = JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY);
        rgb.write_with_encoder(encoder)
            .map_err(|e| Error::Io {
                path: dest.clone(),
                source: std::io::Error::other(format!("jpeg encode: {e}")),
            })?;

        std::fs::write(&dest, &bytes).map_err(|source| Error::Io {
            path: dest,
            source,
        })?;
    }

    Ok(())
}

/// Open and decode an image with the same decompression-bomb limits the
/// ML preprocessing uses. Identical guards: 512 MB allocation cap, 16384
/// max width/height.
fn load_image_with_limits(path: &Path) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    reader = reader.with_guessed_format().map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);

    // Suppress the unused-import warning if `ImageFormat` ends up unused
    // after future refactors — currently kept for the doc trail of "this
    // reader is format-agnostic, MIME has already been detected upstream".
    let _ = ImageFormat::Jpeg;

    reader.decode().map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(format!("image decode: {e}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidetic_core::Sha256;
    use image::{ImageBuffer, Rgb};
    use std::path::PathBuf;

    fn fixture_hash() -> Sha256 {
        Sha256::from_hex("aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899")
            .expect("test hash is valid 64-char hex")
    }

    fn make_real_jpeg(dir: &Path, w: u32, h: u32) -> PathBuf {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let path = dir.join("src.jpg");
        img.save_with_format(&path, ImageFormat::Jpeg)
            .expect("write fixture jpeg");
        path
    }

    #[test]
    fn thumb_size_dir_segment_is_stable() {
        assert_eq!(ThumbSize::Small.dir_segment(), "s");
        assert_eq!(ThumbSize::Medium.dir_segment(), "m");
    }

    #[test]
    fn thumb_size_longest_edge() {
        assert_eq!(ThumbSize::Small.longest_edge(), 256);
        assert_eq!(ThumbSize::Medium.longest_edge(), 1024);
    }

    #[test]
    fn thumbnail_path_derivation() {
        let hash = fixture_hash();
        let lib = PathBuf::from("/library");
        let small = thumbnail_path(&lib, &hash, ThumbSize::Small);
        let medium = thumbnail_path(&lib, &hash, ThumbSize::Medium);

        assert_eq!(
            small,
            PathBuf::from(
                "/library/.thumbs/s/aa/bb/aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899.jpg"
            )
        );
        assert_eq!(
            medium,
            PathBuf::from(
                "/library/.thumbs/m/aa/bb/aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899.jpg"
            )
        );
    }

    #[test]
    fn generate_writes_both_sizes() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_real_jpeg(tmp.path(), 800, 600);
        let library = tmp.path().join("library");
        let hash = fixture_hash();

        generate_thumbnails(&src, &hash, &library).expect("generate");

        let small = thumbnail_path(&library, &hash, ThumbSize::Small);
        let medium = thumbnail_path(&library, &hash, ThumbSize::Medium);
        assert!(small.exists(), "small thumbnail missing at {small:?}");
        assert!(medium.exists(), "medium thumbnail missing at {medium:?}");

        // Re-read them through image to confirm they are valid JPEGs.
        let s_img = image::ImageReader::open(&small).unwrap().decode().unwrap();
        let m_img = image::ImageReader::open(&medium).unwrap().decode().unwrap();
        // Aspect ratio preserved (4:3 source → 4:3 thumbs).
        // 800x600 source → at edge 256 the wider edge wins:
        //   max(w, h) becomes 256, so 256x192.
        // At edge 1024 the source is smaller than 1024 → image stays 800x600.
        assert_eq!(s_img.width(), 256);
        assert_eq!(s_img.height(), 192);
        assert_eq!(m_img.width(), 800);
        assert_eq!(m_img.height(), 600);
    }

    #[test]
    fn generate_fails_on_corrupt_input() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        // 4-byte JPEG-magic stub — passes infer() upstream but image::decode rejects it.
        let src = tmp.path().join("corrupt.jpg");
        std::fs::write(&src, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        let library = tmp.path().join("library");
        let hash = fixture_hash();

        let result = generate_thumbnails(&src, &hash, &library);
        assert!(
            result.is_err(),
            "expected Err on corrupt input, got {result:?}"
        );
    }

    #[test]
    fn generate_creates_intermediate_directories() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = make_real_jpeg(tmp.path(), 100, 100);
        let nested_library = tmp.path().join("deeply").join("nested").join("library");
        let hash = fixture_hash();

        generate_thumbnails(&src, &hash, &nested_library).expect("generate");
        assert!(thumbnail_path(&nested_library, &hash, ThumbSize::Small).exists());
    }
}
```

- [ ] **Step 3: Wire the module into `crates/eidetic-ingest/src/lib.rs`**

Current contents:

```rust
//! File ingestion for Eidetic.

mod error;
mod hasher;
mod import;
mod meta;
mod store;

pub use error::{Error, Result};
pub use hasher::hash_file;
pub use import::{ImportOutcome, ImportSummary, import_dir, import_file};
pub use meta::ExifData;
pub use store::{commit_staged, stage_file};
```

Add `pub mod thumbnail;` (public so the CLI's backfill command can call `thumbnail::generate_thumbnails` directly without re-export). Final:

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

- [ ] **Step 4: Run the tests**

```bash
cargo test -p eidetic-ingest thumbnail
```

Expected: 5 tests pass (`thumb_size_dir_segment_is_stable`, `thumb_size_longest_edge`, `thumbnail_path_derivation`, `generate_writes_both_sizes`, `generate_fails_on_corrupt_input`, `generate_creates_intermediate_directories` — six total). All pass.

Then full workspace:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude eidetic-db
```

Expected: all green. (`eidetic-db` integration tests need Docker; CI runs those.)

- [ ] **Step 5: Commit**

```bash
git add crates/eidetic-ingest/Cargo.toml \
        crates/eidetic-ingest/src/thumbnail.rs \
        crates/eidetic-ingest/src/lib.rs
git commit -m "feat(ingest): thumbnail generation module

New eidetic-ingest::thumbnail module exposes generate_thumbnails
(both sizes, JPEG q85, Lanczos3, aspect-preserving) and
thumbnail_path (derived from hash + size, no DB needed). Six unit
tests cover path derivation, both-sizes-produced, corrupt-input
failure, and intermediate-directory creation. Module is unused in
prod until Task 3 wires it into import_file."
```

The `Cargo.lock` will also pick up `image` as a new normal-dep of `eidetic-ingest` (it was previously only a transitive dep via `eidetic-ml`). Add it to the commit:

```bash
git add Cargo.lock
git commit --amend --no-edit
```

(or include it in the original `git add` line if you prefer one commit step.)

---

## Task 3: Wire `generate_thumbnails` into `import_file`

**Goal:** Replace the placeholder `thumbnails_generated: false` from Task 1 with an actual call to `generate_thumbnails` for image MIME types. Failures warn and leave the column `false` (the asset row still lands).

**Files:**
- Modify: `crates/eidetic-ingest/src/import.rs`

- [ ] **Step 1: Update `import_file` to compute `thumbnails_generated`**

In `import_file`, immediately after the `commit_staged` block produces `storage_path` and before the `let new_asset = NewAsset { ... }` literal, insert the generation block.

The current block of code (around lines 78–94) reads:

```rust
    let storage_path = match commit_staged(stage, &hash, ext.as_deref(), &config.library_dir) {
        Ok(p) => p,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path,
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
        thumbnails_generated: false,
    };
```

Modify to compute `thumbnails_generated` from a `generate_thumbnails` call (image MIME types only — videos stay `false`):

```rust
    let storage_path = match commit_staged(stage, &hash, ext.as_deref(), &config.library_dir) {
        Ok(p) => p,
        Err(e) => return ImportOutcome::Failed(e),
    };

    let thumbnails_generated = if mime_type.starts_with("image/") {
        match crate::thumbnail::generate_thumbnails(&storage_path, &hash, &config.library_dir) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "thumbnail generation failed; asset stored without thumbnails"
                );
                false
            }
        }
    } else {
        false
    };

    let new_asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename,
        storage_path: storage_path.clone(),
        file_size,
        mime_type: Some(mime_type),
        date_taken: exif.date_taken,
        latitude: exif.latitude,
        longitude: exif.longitude,
        camera_make: exif.camera_make,
        camera_model: exif.camera_model,
        thumbnails_generated,
    };
```

Note `storage_path: storage_path.clone()` — `generate_thumbnails` takes the path by reference but we still need to move `storage_path` into the `NewAsset` afterwards. The clone is cheap (`PathBuf::clone`).

**Why this is synchronous instead of `spawn_blocking`:** `import_file` itself is `async fn` but already blocks on `hash_file`, `stage_file`, `commit_staged`, and `extract_exif` — all sync, all CPU/IO-bound. The decision to dispatch sync I/O onto a blocking pool is made at the *call boundary* (the CLI's `import` command wraps `import_dir` in a Tokio task; thumbnails get the same treatment for free). Adding a `spawn_blocking` here would only deepen the nesting without changing concurrency.

- [ ] **Step 2: Verify the workspace builds**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green.

- [ ] **Step 3: Run the ingest crate tests**

```bash
cargo test -p eidetic-ingest
```

Expected: pass. `import_file`'s body now calls `generate_thumbnails` for the synthetic stub-JPEG fixtures used elsewhere — but the existing in-file tests for `import_file` all live in `crates/eidetic-db/tests/import_integration.rs` (testcontainer-backed). The local `eidetic-ingest` test suite covers `hasher`, `meta`, `store`, and `thumbnail` — none of those exercise `import_file` directly, so they all still pass.

The testcontainer integration tests will exercise the new path; they need Docker and are covered in Task 7.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-ingest/src/import.rs
git commit -m "feat(ingest): generate thumbnails during import

For image MIME types, call generate_thumbnails after commit_staged
and set NewAsset.thumbnails_generated based on the result. Failure
warns and leaves the row's flag as false — the eidetic thumbnail
backfill command (next task series) retries them.

Videos always get thumbnails_generated = false; they're filtered
out of the backfill query by mime_type LIKE 'image/%'."
```

---

## Task 4: `Stats` query addition + CLI output line

**Goal:** Surface the thumbnail-pending count alongside the existing stats.

**Files:**
- Modify: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-cli/src/main.rs`

- [ ] **Step 1: Extend `LibraryStats` with `thumbnails_pending`**

In `crates/eidetic-db/src/assets.rs`, find the `LibraryStats` struct (around line 38):

```rust
pub struct LibraryStats {
    pub total: i64,
    pub images: i64,
    pub videos: i64,
    pub embedded: i64,
    pub needs_embed: i64,
    pub total_bytes: i64,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
}
```

Add a `thumbnails_pending` field after `needs_embed`:

```rust
pub struct LibraryStats {
    pub total: i64,
    pub images: i64,
    pub videos: i64,
    pub embedded: i64,
    pub needs_embed: i64,
    pub thumbnails_pending: i64,
    pub total_bytes: i64,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
}
```

- [ ] **Step 2: Update the `fetch_stats` SQL**

The current SQL (around lines 79–93):

```rust
        let row: StatsRow = sqlx::query_as(
            "SELECT \
               COUNT(*)                                                   AS total, \
               COUNT(*) FILTER (WHERE mime_type LIKE 'image/%')           AS images, \
               COUNT(*) FILTER (WHERE mime_type LIKE 'video/%')           AS videos, \
               COUNT(*) FILTER (WHERE embedding IS NOT NULL)              AS embedded, \
               COUNT(*) FILTER (WHERE embedding IS NULL \
                                  AND mime_type LIKE 'image/%')           AS needs_embed, \
               COALESCE(SUM(file_size), 0)::bigint                        AS total_bytes, \
               MIN(date_taken)                                            AS earliest, \
               MAX(date_taken)                                            AS latest \
             FROM assets",
        )
```

Add one count expression for thumbnails_pending (images whose thumbnails haven't been generated):

```rust
        let row: StatsRow = sqlx::query_as(
            "SELECT \
               COUNT(*)                                                   AS total, \
               COUNT(*) FILTER (WHERE mime_type LIKE 'image/%')           AS images, \
               COUNT(*) FILTER (WHERE mime_type LIKE 'video/%')           AS videos, \
               COUNT(*) FILTER (WHERE embedding IS NOT NULL)              AS embedded, \
               COUNT(*) FILTER (WHERE embedding IS NULL \
                                  AND mime_type LIKE 'image/%')           AS needs_embed, \
               COUNT(*) FILTER (WHERE thumbnails_generated = FALSE \
                                  AND mime_type LIKE 'image/%')           AS thumbnails_pending, \
               COALESCE(SUM(file_size), 0)::bigint                        AS total_bytes, \
               MIN(date_taken)                                            AS earliest, \
               MAX(date_taken)                                            AS latest \
             FROM assets",
        )
```

- [ ] **Step 3: Update the `StatsRow` struct (the local sqlx::FromRow inside `fetch_stats`)**

The current inline struct:

```rust
        #[derive(sqlx::FromRow)]
        struct StatsRow {
            total: i64,
            images: i64,
            videos: i64,
            embedded: i64,
            needs_embed: i64,
            total_bytes: i64,
            earliest: Option<DateTime<Utc>>,
            latest: Option<DateTime<Utc>>,
        }
```

Add `thumbnails_pending: i64,` between `needs_embed` and `total_bytes`:

```rust
        #[derive(sqlx::FromRow)]
        struct StatsRow {
            total: i64,
            images: i64,
            videos: i64,
            embedded: i64,
            needs_embed: i64,
            thumbnails_pending: i64,
            total_bytes: i64,
            earliest: Option<DateTime<Utc>>,
            latest: Option<DateTime<Utc>>,
        }
```

And in the final `Ok(LibraryStats { ... })` literal, add `thumbnails_pending: row.thumbnails_pending,` in the same position:

```rust
        Ok(LibraryStats {
            total: row.total,
            images: row.images,
            videos: row.videos,
            embedded: row.embedded,
            needs_embed: row.needs_embed,
            thumbnails_pending: row.thumbnails_pending,
            total_bytes: row.total_bytes,
            earliest: row.earliest,
            latest: row.latest,
        })
```

- [ ] **Step 4: Update `Stats` command output in `crates/eidetic-cli/src/main.rs`**

Find the `Command::Stats` arm. The current output block (around lines 198–209):

```rust
            println!("Assets     {:>8}", s.total);
            println!("  images   {:>8}", s.images);
            println!("  videos   {:>8}", s.videos);
            println!("Embedded   {:>8}", s.embedded);
            println!("Not yet    {:>8}", s.needs_embed);
            println!("Size       {:>8}", format_bytes(s.total_bytes as u64));
            if let Some(earliest) = s.earliest {
                println!("Earliest   {}", earliest.format("%Y-%m-%d"));
            }
            if let Some(latest) = s.latest {
                println!("Latest     {}", latest.format("%Y-%m-%d"));
            }
```

Add a `Thumbs pending` line right after the embed-related lines, before `Size`:

```rust
            println!("Assets     {:>8}", s.total);
            println!("  images   {:>8}", s.images);
            println!("  videos   {:>8}", s.videos);
            println!("Embedded   {:>8}", s.embedded);
            println!("Not yet    {:>8}", s.needs_embed);
            println!("Thumbs pending {:>4}", s.thumbnails_pending);
            println!("Size       {:>8}", format_bytes(s.total_bytes as u64));
            if let Some(earliest) = s.earliest {
                println!("Earliest   {}", earliest.format("%Y-%m-%d"));
            }
            if let Some(latest) = s.latest {
                println!("Latest     {}", latest.format("%Y-%m-%d"));
            }
```

(`{:>4}` instead of `{:>8}` because "Thumbs pending" is longer than the other labels; total column width stays around the same.)

- [ ] **Step 5: Verify**

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
```

Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/eidetic-db/src/assets.rs crates/eidetic-cli/src/main.rs
git commit -m "feat(stats): surface thumbnail-pending count

fetch_stats picks up one more COUNT FILTER for images with
thumbnails_generated = FALSE. The CLI Stats command prints a new
'Thumbs pending' line between 'Not yet' (embedding) and 'Size'."
```

---

## Task 5: `fetch_unthumbnailed` + `mark_thumbnailed` repo methods

**Goal:** Add the two `PgAssetsRepo` methods that power the `eidetic thumbnail` subcommand. Test them via the existing testcontainer pattern.

**Files:**
- Modify: `crates/eidetic-db/src/assets.rs`
- Modify: `crates/eidetic-db/tests/assets.rs`

- [ ] **Step 1: Add `fetch_unthumbnailed` and `mark_thumbnailed` to `PgAssetsRepo`**

In `crates/eidetic-db/src/assets.rs`, inside the existing `impl PgAssetsRepo { ... }` block, after `store_embedding` and before the inherent `find_by_hash` / `insert_asset` methods (or wherever fits the existing ordering — after `store_embedding` is fine), add:

```rust
    /// Image assets that don't yet have thumbnails generated.
    ///
    /// Returns `(id, hash, storage_path)` so callers can locate the
    /// source bytes (via `storage_path`) and derive the destination
    /// thumbnail paths (via `hash`) without further DB roundtrips.
    ///
    /// Ordered by `id` for stable resumption across runs.
    pub async fn fetch_unthumbnailed(&self) -> crate::Result<Vec<(AssetId, eidetic_core::Sha256, PathBuf)>> {
        let rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
            "SELECT id, hash, storage_path FROM assets \
             WHERE thumbnails_generated = FALSE \
               AND mime_type LIKE 'image/%' \
             ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::Error::Query)?;

        Ok(rows
            .into_iter()
            .map(|(uuid, hash_hex, path)| {
                let hash = eidetic_core::Sha256::from_hex(&hash_hex)
                    .expect("hash column is CHAR(64) of lowercase hex written by our own code");
                (AssetId::from(uuid), hash, PathBuf::from(path))
            })
            .collect())
    }

    /// Flip `thumbnails_generated` to TRUE for a single row.
    pub async fn mark_thumbnailed(&self, id: AssetId) -> crate::Result<()> {
        sqlx::query("UPDATE assets SET thumbnails_generated = TRUE WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(crate::Error::Query)?;
        Ok(())
    }
```

The `expect` in `Sha256::from_hex` is acceptable because:
1. The column is `CHAR(64) UNIQUE NOT NULL` per migration 001.
2. Our only writer (`insert_asset`) takes `&asset.hash: &String` directly from `hash.to_string()` in `import_file`, which always produces 64 lowercase hex chars.
3. There is no admin / manual SQL path that bypasses this guarantee.

If a future code path inserts assets without going through `insert_asset`, this assumption needs revisiting.

- [ ] **Step 2: Add integration tests in `crates/eidetic-db/tests/assets.rs`**

Append two tests after the existing ones (end of file). The tests use the same `start_db` helper already present at the top of the file:

```rust
#[tokio::test]
async fn fetch_unthumbnailed_returns_images_with_flag_false() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let image_pending = NewAsset {
        hash: "11110000111100001111000011110000111100001111000011110000ffffffff".to_string(),
        original_filename: "pending.jpg".to_string(),
        storage_path: PathBuf::from("/lib/11/11/pending.jpg"),
        file_size: 1,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    };
    let image_done = NewAsset {
        hash: "22220000222200002222000022220000222200002222000022220000ffffffff".to_string(),
        original_filename: "done.jpg".to_string(),
        storage_path: PathBuf::from("/lib/22/22/done.jpg"),
        file_size: 1,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: true,
    };
    let video_pending = NewAsset {
        hash: "33330000333300003333000033330000333300003333000033330000ffffffff".to_string(),
        original_filename: "video.mp4".to_string(),
        storage_path: PathBuf::from("/lib/33/33/video.mp4"),
        file_size: 1,
        mime_type: Some("video/mp4".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    };

    repo.insert_asset(image_pending).await.expect("insert pending image");
    repo.insert_asset(image_done).await.expect("insert done image");
    repo.insert_asset(video_pending).await.expect("insert video");

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert_eq!(
        pending.len(),
        1,
        "only the unthumbnailed image should come back; got {pending:?}"
    );
    let (_, _, path) = &pending[0];
    assert!(path.to_str().unwrap().contains("pending.jpg"));
}

#[tokio::test]
async fn mark_thumbnailed_flips_flag_to_true() {
    let (_container, url) = start_db().await;
    let config = Config {
        database_url: url,
        ..Default::default()
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);

    let asset = NewAsset {
        hash: "44440000444400004444000044440000444400004444000044440000ffffffff".to_string(),
        original_filename: "mark_me.jpg".to_string(),
        storage_path: PathBuf::from("/lib/44/44/mark.jpg"),
        file_size: 1,
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    };
    let outcome = repo.insert_asset(asset).await.expect("insert");
    let id = match outcome {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    let before = repo.fetch_unthumbnailed().await.expect("fetch before");
    assert_eq!(before.len(), 1);

    repo.mark_thumbnailed(id).await.expect("mark");

    let after = repo.fetch_unthumbnailed().await.expect("fetch after");
    assert_eq!(after.len(), 0, "row should no longer be pending after mark");
}
```

- [ ] **Step 3: Build the tests**

```bash
cargo build -p eidetic-db --tests
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green. The tests themselves need Docker to run (testcontainers); CI will execute them.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-db/src/assets.rs crates/eidetic-db/tests/assets.rs
git commit -m "feat(db): fetch_unthumbnailed + mark_thumbnailed

Two new PgAssetsRepo methods. fetch_unthumbnailed returns
(id, hash, storage_path) for images where thumbnails_generated
is FALSE, ordered by id for stable resumption. mark_thumbnailed
flips the flag for one row.

Two new testcontainer-backed tests cover the basic mark/unmark
flow and the video-filter behaviour (videos must never appear in
the pending list)."
```

---

## Task 6: CLI `eidetic thumbnail` subcommand

**Goal:** Wire the repo methods + thumbnail module into a user-facing command.

**Files:**
- Modify: `crates/eidetic-cli/src/main.rs`

- [ ] **Step 1: Add the `Thumbnail` variant to the `Command` enum**

Find the `enum Command { ... }` definition in `crates/eidetic-cli/src/main.rs`. Add a new variant after the existing ones (alphabetical order would put it between `Stats` and `Search`; pragmatic order is "fits the existing pattern" — just add it next to `Embed` to mirror that command):

For example, if the current enum reads like:

```rust
#[derive(Subcommand)]
enum Command {
    Import { path: PathBuf },
    Stats,
    Eval { coco_csv: PathBuf, coco_images: PathBuf, limit: Option<usize> },
    Embed,
    Search { ... },
}
```

Add `Thumbnail,` next to `Embed`:

```rust
#[derive(Subcommand)]
enum Command {
    Import { path: PathBuf },
    Stats,
    Eval { coco_csv: PathBuf, coco_images: PathBuf, limit: Option<usize> },
    Embed,
    /// Generate missing thumbnails for previously-imported images.
    Thumbnail,
    Search { ... },
}
```

(Read the file to confirm the exact enum shape — clap derive enums can have attribute macros and doc comments above each variant. Follow the existing style.)

- [ ] **Step 2: Add the `Command::Thumbnail` match arm body**

In the big `match command { ... }` at the bottom of `main`, add an arm. Place it next to `Command::Embed` for consistency. Body:

```rust
        Command::Thumbnail => {
            let config = Config::from_env();
            let pool = eidetic_db::connect(&config)
                .await
                .context("failed to connect to database")?;
            let repo = eidetic_db::PgAssetsRepo::new(pool);

            let pending = repo
                .fetch_unthumbnailed()
                .await
                .context("failed to fetch unthumbnailed assets")?;

            if pending.is_empty() {
                println!("Nothing to do.");
                return Ok(());
            }

            let total = pending.len();
            println!("Generating thumbnails for {total} images…");

            let mut generated = 0u32;
            let mut failed = 0u32;

            for (i, (id, hash, storage_path)) in pending.into_iter().enumerate() {
                let lib = config.paths.library_dir.clone();
                let storage_clone = storage_path.clone();
                let hash_clone = hash.clone();

                let result = tokio::task::spawn_blocking(move || {
                    eidetic_ingest::thumbnail::generate_thumbnails(
                        &storage_clone,
                        &hash_clone,
                        &lib,
                    )
                })
                .await
                .context("thumbnail thread panicked")?;

                match result {
                    Ok(()) => match repo.mark_thumbnailed(id).await {
                        Ok(()) => {
                            println!("[{}/{}] {}", i + 1, total, storage_path.display());
                            generated += 1;
                        }
                        Err(e) => {
                            // Keep going — rows whose mark failed stay false
                            // and are retried on the next run, same pattern
                            // as eidetic embed.
                            eprintln!(
                                "  [{}/{}] mark failed for {}: {e}",
                                i + 1,
                                total,
                                storage_path.display()
                            );
                            failed += 1;
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "  [{}/{}] generate failed for {}: {e}",
                            i + 1,
                            total,
                            storage_path.display()
                        );
                        failed += 1;
                    }
                }
            }

            if failed > 0 {
                println!("Done. Generated {generated}, failed {failed}.");
                std::process::exit(1);
            } else {
                println!("Done. Generated {generated}.");
            }
        }
```

The `hash.clone()` and `storage_path.clone()` are required because the `move` closure consumes them and the surrounding loop still needs `storage_path` for the `println!`. `Sha256` is `Copy` (it wraps `[u8; 32]`); actually, check `crates/eidetic-core/src/ids.rs` — if `Sha256` derives `Copy`, drop the `.clone()`. If it doesn't, leave the clone (cheap, 32-byte memcpy).

Read `crates/eidetic-core/src/ids.rs` to confirm. If `Sha256` does derive `Copy` (likely — it's a small wrapper), you can simplify:

```rust
            for (i, (id, hash, storage_path)) in pending.into_iter().enumerate() {
                let lib = config.paths.library_dir.clone();
                let storage_clone = storage_path.clone();

                let result = tokio::task::spawn_blocking(move || {
                    eidetic_ingest::thumbnail::generate_thumbnails(
                        &storage_clone,
                        &hash,
                        &lib,
                    )
                })
                ...
```

(The `hash` is `Copy`'d into the closure, the original is consumed by the move but the `Copy` semantics make this fine.)

If `Sha256` does NOT derive `Copy`, leave the `.clone()`. Both compile.

- [ ] **Step 3: Verify**

```bash
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green.

Manual smoke (optional, no DB needed): `cargo run -p eidetic-cli -- thumbnail --help`. Confirms the subcommand registered with clap.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-cli/src/main.rs
git commit -m "feat(cli): eidetic thumbnail subcommand

Backfill thumbnails for images where thumbnails_generated = FALSE.
Mirrors eidetic embed: per-asset spawn_blocking, progress line,
exits non-zero on any per-row failure so cron / shell pipelines
can branch on it. Distinct from embed: no model load, no
mpsc-worker pattern (thumbnail generation is cheap; the per-call
spawn_blocking overhead is negligible relative to image decode)."
```

---

## Task 7: End-to-end integration tests for thumbnail flow

**Goal:** Cover the `import_file → generate_thumbnails → DB → fetch_unthumbnailed → mark_thumbnailed` round-trip through testcontainers.

**Files:**
- Create: `crates/eidetic-db/tests/thumbnail_integration.rs`

- [ ] **Step 1: Write the integration test file**

Create `crates/eidetic-db/tests/thumbnail_integration.rs` with this content:

```rust
//! End-to-end integration: thumbnail generation through import_file +
//! the backfill flow (fetch_unthumbnailed → generate → mark_thumbnailed).

use eidetic_core::{Config, Paths};
use eidetic_db::{InsertOutcome, NewAsset, PgAssetsRepo};
use eidetic_ingest::{ImportOutcome, hash_file, import_file, thumbnail};
use image::{ImageBuffer, ImageFormat, Rgb};
use std::path::{Path, PathBuf};
use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::AsyncRunner};

async fn start_db() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("tensorchord/vchord-postgres", "pg17-v0.4.3")
        .with_wait_for(WaitFor::message_on_stderr("ready to accept connections"))
        .with_env_var("POSTGRES_USER", "eidetic")
        .with_env_var("POSTGRES_PASSWORD", "eidetic")
        .with_env_var("POSTGRES_DB", "eidetic")
        .start()
        .await
        .expect("failed to start postgres container");

    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://eidetic:eidetic@127.0.0.1:{port}/eidetic");
    (container, url)
}

async fn fixture() -> (
    PgAssetsRepo,
    Paths,
    tempfile::TempDir,
    testcontainers::ContainerAsync<GenericImage>,
) {
    let (container, url) = start_db().await;
    let tmp = tempfile::tempdir().expect("tmpdir");
    let config = Config {
        database_url: url,
        paths: Paths {
            library_dir: tmp.path().join("library"),
            models_cache: tmp.path().join("models"),
        },
    };
    let pool = eidetic_db::connect(&config).await.expect("connect");
    let repo = PgAssetsRepo::new(pool);
    (repo, config.paths, tmp, container)
}

/// Write a real JPEG (not a 4-byte stub) so `image::decode` succeeds.
fn write_real_jpeg(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });
    img.save_with_format(&path, ImageFormat::Jpeg).unwrap();
    path
}

#[tokio::test]
async fn imported_image_has_thumbnails_generated_true() {
    let (repo, paths, tmp, _container) = fixture().await;
    let src = write_real_jpeg(tmp.path(), "photo.jpg", 800, 600);

    let outcome = import_file(&src, &repo, &paths).await;
    let _id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported, got {other:?}"),
    };

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert!(
        pending.is_empty(),
        "newly imported image should have thumbnails_generated = true; pending={pending:?}"
    );

    // Both thumbnail files should be on disk.
    let hash = hash_file(&src).unwrap();
    let small = thumbnail::thumbnail_path(&paths.library_dir, &hash, thumbnail::ThumbSize::Small);
    let medium = thumbnail::thumbnail_path(&paths.library_dir, &hash, thumbnail::ThumbSize::Medium);
    assert!(small.exists(), "small thumb missing at {small:?}");
    assert!(medium.exists(), "medium thumb missing at {medium:?}");
}

#[tokio::test]
async fn imported_video_stays_thumbnails_generated_false() {
    let (repo, paths, tmp, _container) = fixture().await;

    // Stub video: 12 bytes of an MP4-ish header that `infer` will accept.
    // `infer` matches MP4 on a few canonical byte patterns; use the
    // simplest valid prefix: 00 00 00 20 66 74 79 70 69 73 6F 6D
    // = "....ftypisom" — `infer` returns video/mp4 for this.
    let src = tmp.path().join("clip.mp4");
    std::fs::write(
        &src,
        [
            0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70, 0x69, 0x73, 0x6F, 0x6D,
        ],
    )
    .expect("write stub mp4");

    let outcome = import_file(&src, &repo, &paths).await;
    let _id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported (video should still ingest), got {other:?}"),
    };

    // Video should NOT appear in the unthumbnailed list (filter is
    // `mime_type LIKE 'image/%'`).
    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert!(
        pending.is_empty(),
        "video should not appear in unthumbnailed list; got {pending:?}"
    );
}

#[tokio::test]
async fn corrupt_image_lands_but_thumbnails_stay_pending() {
    let (repo, paths, tmp, _container) = fixture().await;

    // 4-byte JPEG-magic stub — passes `infer` (which only checks the
    // first few bytes) but `image::decode` rejects it during thumbnail
    // generation. The asset row should land; the row should appear in
    // fetch_unthumbnailed for retry.
    let src = tmp.path().join("corrupt.jpg");
    std::fs::write(&src, [0xFF, 0xD8, 0xFF, 0xE0]).expect("write stub");

    let outcome = import_file(&src, &repo, &paths).await;
    let id = match outcome {
        ImportOutcome::Imported(id) => id,
        other => panic!("expected Imported even on corrupt image, got {other:?}"),
    };

    let pending = repo.fetch_unthumbnailed().await.expect("fetch");
    assert_eq!(pending.len(), 1, "corrupt image should be retry-pending");
    let (pending_id, _hash, _path) = &pending[0];
    assert_eq!(*pending_id, id, "the pending row should be the one we imported");
}

#[tokio::test]
async fn backfill_flow_generates_and_marks_pending_image() {
    let (repo, paths, tmp, _container) = fixture().await;

    // Seed the DB with a row whose thumbnails_generated = FALSE,
    // simulating an asset imported before this feature existed (or
    // whose first attempt failed).
    let src = write_real_jpeg(tmp.path(), "src.jpg", 400, 300);
    let hash = hash_file(&src).unwrap();
    let hash_hex = hash.to_string();
    let asset = NewAsset {
        hash: hash_hex.clone(),
        original_filename: "src.jpg".to_string(),
        storage_path: src.clone(),
        file_size: std::fs::metadata(&src).unwrap().len(),
        mime_type: Some("image/jpeg".to_string()),
        date_taken: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        thumbnails_generated: false,
    };
    let id = match repo.insert_asset(asset).await.expect("insert") {
        InsertOutcome::Inserted(id) => id,
        InsertOutcome::Existing(_) => panic!("expected Inserted"),
    };

    // Simulate the backfill: fetch, generate, mark.
    let pending = repo.fetch_unthumbnailed().await.expect("fetch before");
    assert_eq!(pending.len(), 1);
    let (pending_id, pending_hash, pending_path) = pending.into_iter().next().unwrap();
    assert_eq!(pending_id, id);
    assert_eq!(pending_hash.to_string(), hash_hex);

    thumbnail::generate_thumbnails(&pending_path, &pending_hash, &paths.library_dir)
        .expect("generate");
    repo.mark_thumbnailed(id).await.expect("mark");

    // After: row no longer pending, files on disk.
    let after = repo.fetch_unthumbnailed().await.expect("fetch after");
    assert!(after.is_empty());

    let small = thumbnail::thumbnail_path(
        &paths.library_dir,
        &pending_hash,
        thumbnail::ThumbSize::Small,
    );
    let medium = thumbnail::thumbnail_path(
        &paths.library_dir,
        &pending_hash,
        thumbnail::ThumbSize::Medium,
    );
    assert!(small.exists());
    assert!(medium.exists());
}
```

- [ ] **Step 2: Build the test crate**

```bash
cargo build -p eidetic-db --tests
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green.

- [ ] **Step 3: Run the tests if Docker is available locally**

```bash
cargo test -p eidetic-db --test thumbnail_integration
```

Expected (if Docker present): four tests pass. If Docker isn't available, skip — CI runs them.

- [ ] **Step 4: Commit**

```bash
git add crates/eidetic-db/tests/thumbnail_integration.rs
git commit -m "test(db): integration tests for thumbnail flow

Four testcontainer-backed tests:
- imported_image_has_thumbnails_generated_true: happy path
- imported_video_stays_thumbnails_generated_false: video filter
- corrupt_image_lands_but_thumbnails_stay_pending: failure mode
- backfill_flow_generates_and_marks_pending_image: backfill cycle"
```

---

## Task 8: AGENTS.md tweak + final verify + PR

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Update the `eidetic-ingest` row in `AGENTS.md`**

The current row reads:

```
| `eidetic-ingest` | File watcher, streaming hasher, content-addressable storage. Calls `PgAssetsRepo` directly. | `eidetic-core`, `eidetic-db` |
```

Change to:

```
| `eidetic-ingest` | File watcher, streaming hasher, content-addressable storage, thumbnail generation. Calls `PgAssetsRepo` directly. | `eidetic-core`, `eidetic-db` |
```

(Just adds ", thumbnail generation" to the Role column.)

- [ ] **Step 2: Final verification**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude eidetic-db
```

All three should be green. (`eidetic-db` tests need Docker; CI handles them.)

If `cargo-deny` is installed locally:

```bash
cargo deny check
```

Expected: no new advisories / license issues. `image` was already a workspace dependency, so cargo-deny shouldn't object.

- [ ] **Step 3: Commit the AGENTS.md update**

```bash
git add AGENTS.md
git commit -m "docs(agents): note thumbnail generation in eidetic-ingest role"
```

- [ ] **Step 4: Push and open PR**

```bash
git push -u origin feat/thumbnails
gh pr create --title "feat: thumbnails (256/1024 JPEG, generated on import + backfill command)" --body "$(cat <<'EOF'
## Summary

- New \`eidetic-ingest::thumbnail\` module generates two JPEG thumbnails per image (256px and 1024px longest edge, quality 85, Lanczos3, aspect-preserving). Stored at \`<library_dir>/.thumbs/<s|m>/<hash[..2]>/<hash[2..4]>/<hash>.jpg\` — CAS-shaped, derivable from \`(hash, size)\` alone.
- New \`assets.thumbnails_generated BOOLEAN\` column (migration 004). \`import_file\` writes \`TRUE\` when both sizes produced, \`FALSE\` (with a warning) on any failure.
- New \`eidetic thumbnail\` subcommand backfills missing thumbnails for images where \`thumbnails_generated = FALSE\`. Per-asset \`spawn_blocking\`; exits non-zero on any per-row failure (same convention as \`eidetic embed\`).
- \`eidetic stats\` shows a new "Thumbs pending" line.
- Zero new dependencies — \`image\` was already in the workspace.

Closes Phase 1. Plan at \`docs/superpowers/plans/2026-05-15-thumbnails.md\`; design doc at \`docs/superpowers/specs/2026-05-15-thumbnails-design.md\`.

## Spec deviation worth flagging

The design said WebP quality 85. The \`image\` crate's built-in WebP encoder is lossless-only as of 0.25, which would produce larger files than the JPEG it replaces. Switched to JPEG quality 85 — same visual quality budget, smaller files, no new dependencies. File extension is \`.jpg\`.

## Behavioural notes

- Videos are imported normally (already were) but never get thumbnails. The \`mime_type LIKE 'image/%'\` filter on \`fetch_unthumbnailed\` keeps them out of the backfill query, so they show \`thumbnails_generated = FALSE\` without ever appearing in the "Thumbs pending" count.
- A corrupt image (passes \`infer\` MIME detection but \`image::decode\` rejects it) ingests its row successfully; the thumbnail generation step warns and leaves \`thumbnails_generated = false\`. The backfill command's next run will retry.
- \`eidetic thumbnail\` does not currently distinguish "tried and failed" from "never tried" — both are \`FALSE\`. The retry loop is idempotent and cheap; running it repeatedly on a corrupt image will fail repeatedly, which the operator sees in the per-row \`generate failed\` line.

## Test plan

- [x] \`cargo fmt --all -- --check\`
- [x] \`cargo clippy --workspace --all-targets -- -D warnings\`
- [x] \`cargo test --workspace --exclude eidetic-db\` (Docker tests gated on CI)
- [ ] Manual smoke after merge: \`eidetic import <small folder>\`, then check \`<library_dir>/.thumbs/{s,m}/\` for two files per image.
- [ ] Manual smoke after merge: \`eidetic stats\` shows "Thumbs pending  0" on a fully-thumbnailed library.

## Commits

- \`docs(specs): add thumbnails design\` — design doc (already on branch from brainstorm)
- \`feat(db): add thumbnails_generated column\` — migration 004 + NewAsset field
- \`feat(ingest): thumbnail generation module\` — pure generator + 6 unit tests
- \`feat(ingest): generate thumbnails during import\` — wire into import_file
- \`feat(stats): surface thumbnail-pending count\` — fetch_stats + CLI Stats output
- \`feat(db): fetch_unthumbnailed + mark_thumbnailed\` — repo methods + tests
- \`feat(cli): eidetic thumbnail subcommand\` — backfill command
- \`test(db): integration tests for thumbnail flow\` — end-to-end coverage
- \`docs(agents): note thumbnail generation in eidetic-ingest role\`
EOF
)"
```

Return the PR URL.

---

## Self-Review

**Spec coverage (each requirement in the design doc):**

| Spec requirement | Plan task |
|---|---|
| Two sizes (256, 1024) JPEG at quality 85, Lanczos3 | Task 2 (`generate_thumbnails`, `JPEG_QUALITY`, `FilterType::Lanczos3`) |
| CAS-shaped path under `.thumbs/<s|m>/...` | Task 2 (`thumbnail_path`) |
| `thumbnails_generated BOOLEAN NOT NULL DEFAULT FALSE` | Task 1 Step 2 |
| Generation during `import_file` after `commit_staged` | Task 3 Step 1 |
| Failures warn, leave row false | Task 3 Step 1 (the `tracing::warn!` + `false` branches) |
| Image MIME types only (videos skipped) | Task 3 Step 1 (`if mime_type.starts_with("image/")`) |
| `fetch_unthumbnailed` returns `(id, hash, storage_path)` | Task 5 Step 1 |
| `mark_thumbnailed` flips one row | Task 5 Step 1 |
| `eidetic thumbnail` CLI mirrors `eidetic embed` shape | Task 6 |
| Stats shows pending count | Task 4 |
| Failure mode parity with embeddings | Task 3 Step 1 + Task 6 Step 2 (continue-on-error, non-zero exit on any row fail) |
| Zero new dependencies | Task 2 Step 1 (image was already in workspace) |
| Migration is additive, forward-only | Task 1 Step 2 |
| Module is `eidetic-ingest::thumbnail` (not a new crate) | Task 2 Step 2 |
| Unit tests in-file for the thumbnail module | Task 2 Step 2 (six tests) |
| Integration tests for backfill flow | Task 7 (four tests) |
| AGENTS.md mention | Task 8 Step 1 |

**Placeholder scan:**
- One open `TODO`-shape note in Task 6 Step 2: "Read `crates/eidetic-core/src/ids.rs` to confirm `Sha256: Copy`." This is a verification step, not a placeholder; both branches (Copy or not) compile, and the plan provides both code variants. Acceptable.
- The `Stats` output column-width tweak (`{:>4}` vs `{:>8}`) is concrete; the implementer can adjust if they prefer different spacing.
- No "TBD," no "implement later," no "handle edge cases."

**Type consistency check:**
- `ThumbSize::dir_segment() -> &'static str` used consistently in Task 2 (definition + tests) and Task 5 (no, doesn't reference it — the repo method derives path via `thumbnail_path` which lives in the thumbnail module, so the dir-segment lookup is encapsulated). ✓
- `thumbnail_path(&Path, &Sha256, ThumbSize) -> PathBuf` signature stable in Task 2 (definition + tests) and Task 7 (integration tests use it). ✓
- `generate_thumbnails(src: &Path, hash: &Sha256, library_dir: &Path) -> Result<()>` signature stable in Task 2 (definition), Task 3 (call site), Task 6 (call site), Task 7 (call site). ✓
- `fetch_unthumbnailed() -> Result<Vec<(AssetId, Sha256, PathBuf)>>` signature consistent in Task 5 (definition) and Task 6 (consumer). ✓
- `NewAsset.thumbnails_generated: bool` consistent in Task 1 (struct), all literal-update sites, Task 3 (computed value), Tasks 5 and 7 (test seeds). ✓

**Known risk documented:**
The `Sha256::from_hex(&hash).expect("hash column is CHAR(64)...")` in `fetch_unthumbnailed` will panic if a row ever has a non-hex hash. This can only happen if a future code path inserts assets without going through `insert_asset`. The plan calls this assumption out in Task 5 Step 1.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-15-thumbnails.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, two-stage review between tasks. Same flow used for PR #15 and PR #16; eight focused tasks fit this model well.

**2. Inline Execution** — execute tasks in this session via `superpowers:executing-plans`.

Which approach?
