# Thumbnails Design

**Status:** Design approved 2026-05-15. Ready for implementation plan.

**Scope:** Phase 1 closure — adds image thumbnail generation to Eidetic. Backfills the gap between the current ingest pipeline (stores originals, extracts EXIF, computes embeddings) and the future personal web UI / WebDAV viewing surface.

---

## Goals

- Generate two thumbnail sizes per image asset: **256px** (grid / search preview) and **1024px** (single-photo card view).
- Generate **during `eidetic import`** so a fresh library is fully thumbnailed without a separate manual step.
- Provide an `eidetic thumbnail` CLI command to backfill thumbnails for assets where generation previously failed or was skipped.
- Failure-mode parity with embeddings: a corrupt image fails its thumbnail without failing its row; the backfill command catches it later.
- Zero new heavy dependencies. Use the `image` crate already in the workspace; no `ffmpeg`, no `fast_image_resize`, no AVIF encoder.

## Non-goals

- Video thumbnails. Video thumbnails need `ffmpeg` (heavy native dep) and the personal-use video count is low. Videos remain in CAS without thumbnails; the `eidetic thumbnail` command filters them out via `mime_type LIKE 'image/%'`.
- Multiple "responsive" sizes beyond 256/1024. The original Phase 1 spec called for 200/800/1600; 1600 is wasteful when the original is already on disk, and 200 vs 256 doesn't matter perceptually.
- An HTTP server to serve thumbnails. That's a separate Phase concern; this spec only writes thumbnails to disk and tracks generation state in the DB.
- AVIF encoding. WebP at quality 85 is good enough for personal use and encodes 10× faster than libavif.
- Parallel thumbnail generation. Thumbnails are cheap relative to embeddings; a single blocking task per asset is adequate at Phase 1 throughput.

---

## Architecture overview

```
              ┌──────────────────┐
              │  eidetic import  │
              └─────────┬────────┘
                        │
              ┌─────────▼────────┐
              │  import_file     │       (eidetic-ingest)
              │  ─ hash          │
              │  ─ stage         │
              │  ─ commit_staged │
              │  ─ exif          │
              │  ─ thumbnail ◄───┼─────── new module
              │  ─ insert_asset  │
              └──────────────────┘
```

```
              ┌──────────────────────┐
              │  eidetic thumbnail   │   (eidetic-cli, new subcommand)
              └─────────┬────────────┘
                        │
                        │ fetch_unthumbnailed()
                        ▼
              ┌──────────────────────┐
              │  PgAssetsRepo        │   (eidetic-db)
              │   ─ mark_thumbnailed │
              └──────────────────────┘
                        │
                        ▼
                 [thumbnail crate calls]
                        │
                        ▼
              ┌──────────────────────┐
              │  generate_thumbs()   │   (eidetic-ingest::thumbnail)
              └──────────────────────┘
```

---

## File layout

Thumbnails live under the library dir, in a CAS-shaped subtree keyed by size:

```
library_dir/
├── .thumbs/
│   ├── s/             ← small (256px longest edge)
│   │   └── ab/
│   │       └── cd/
│   │           └── abcdef….webp
│   └── m/             ← medium (1024px longest edge)
│       └── ab/
│           └── cd/
│               └── abcdef….webp
├── ab/                ← original CAS, untouched
│   └── cd/
│       └── abcdef….jpg
└── …
```

Path derivation is `library_dir.join(".thumbs").join(<size>).join(&hash[..2]).join(&hash[2..4]).join(format!("{hash}.webp"))` — derived from `(hash, size)`, no DB lookup needed to find a thumbnail file.

Size codes are single letters (`s`, `m`) so the directory shape stays compact and a future `l` tier slots in without breaking the existing layout.

The leading `.` on `.thumbs/` keeps it out of casual `ls` listings of the library dir while remaining visible to anything operating on the library programmatically.

---

## Data model changes

One migration adds a single column:

```sql
-- migrations/003_thumbnails.sql
ALTER TABLE assets
    ADD COLUMN thumbnails_generated BOOLEAN NOT NULL DEFAULT FALSE;
```

No `thumbnails` table. Paths are derived from `(hash, size)`; storing them would be redundant and create a consistency burden.

The column tracks "have we attempted-and-succeeded for both sizes?" Set to `TRUE` only when the small AND medium WebPs both wrote successfully. If either fails the row stays `FALSE`, and the next `eidetic thumbnail` run retries.

For non-image assets (videos), the column stays `FALSE` forever — the `eidetic thumbnail` query filters on `mime_type LIKE 'image/%'` so videos are correctly skipped without any special-case logic.

---

## Module layout

### New module: `crates/eidetic-ingest/src/thumbnail.rs`

Public surface:

```rust
/// Thumbnail sizes generated for every image asset.
///
/// Stable identifier per size — used as the directory segment under
/// `<library_dir>/.thumbs/<size>/`.
pub enum ThumbSize {
    Small,   // 256px longest edge
    Medium,  // 1024px longest edge
}

impl ThumbSize {
    pub fn dir_segment(self) -> &'static str { /* "s" or "m" */ }
    pub fn longest_edge(self) -> u32 { /* 256 or 1024 */ }
}

/// Generate both thumbnail sizes for a source image, writing into
/// `<library_dir>/.thumbs/{s,m}/<hash[0..2]>/<hash[2..4]>/<hash>.webp`.
///
/// Synchronous. CPU-bound. Call inside `tokio::task::spawn_blocking`.
///
/// Returns `Ok(())` only when BOTH sizes wrote successfully. If either
/// fails, the function returns `Err(...)` and the partial output state
/// is left as-is (the next run will overwrite).
pub fn generate_thumbnails(
    src: &Path,
    hash: &Sha256,
    library_dir: &Path,
) -> Result<()>;

/// Derive the on-disk path for a thumbnail.
pub fn thumbnail_path(library_dir: &Path, hash: &Sha256, size: ThumbSize) -> PathBuf;
```

Internal helpers handle: load image with `image::ImageReader` (re-using the decompression-bomb limits already in `eidetic-ml::siglip::preprocess_image`), resize with Lanczos3 to longest-edge `n` preserving aspect ratio, encode to WebP at quality 85, write atomically via temp-file + rename.

The decompression-bomb limits get factored into a small helper here, since `eidetic-ingest` and `eidetic-ml` both need them:

```rust
// In eidetic-ingest::thumbnail (or a shared helper if it gets reused often)
fn open_with_limits(path: &Path) -> Result<image::ImageReader<...>> { ... }
```

If duplication ends up at two call sites (here + SigLIP's `preprocess_image`), leaving the small copy in each place is fine for v0. If a third caller wants it, hoist it up.

### Wiring in `crates/eidetic-ingest/src/import.rs`

`import_file` gets one new step, after `commit_staged` and `extract_exif` succeed but before `insert_asset`:

```rust
let thumbnails_generated = if mime_type.starts_with("image/") {
    match tokio::task::spawn_blocking({
        let dest = storage_path.clone();
        let hash = hash.clone();
        let lib = config.library_dir.clone();
        move || crate::thumbnail::generate_thumbnails(&dest, &hash, &lib)
    })
    .await
    {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "thumbnail generation failed; asset stored without thumbnails"
            );
            false
        }
        Err(_panic) => {
            tracing::warn!(
                path = %path.display(),
                "thumbnail task panicked; asset stored without thumbnails"
            );
            false
        }
    }
} else {
    false
};
```

The flag is then passed into `NewAsset` (new field, see below) so `insert_asset` writes the column correctly on the first insert.

### Wiring in `crates/eidetic-db/src/assets.rs`

`NewAsset` gains one field:

```rust
pub struct NewAsset {
    // … existing fields …
    pub thumbnails_generated: bool,
}
```

`insert_asset`'s SQL adds the new column to the column list and the VALUES list. The conflict-fallback `SELECT` for `InsertOutcome::Existing` doesn't need to read or update the column — duplicates by hash already have their own row's thumbnail state.

Two new repo methods:

```rust
impl PgAssetsRepo {
    /// Rows with `thumbnails_generated = false AND mime_type LIKE 'image/%'`,
    /// ordered by id for stable resumption.
    pub async fn fetch_unthumbnailed(&self) -> Result<Vec<(AssetId, Sha256, PathBuf)>>;

    /// Mark a single row's thumbnails as generated.
    pub async fn mark_thumbnailed(&self, id: AssetId) -> Result<()>;
}
```

`fetch_unthumbnailed` returns the hash so the CLI knows where to write thumbnails (the thumb path is derived from hash, not the original storage_path), and the storage_path so the source image can be read.

### New CLI subcommand: `Command::Thumbnail`

In `crates/eidetic-cli/src/main.rs`:

```rust
Command::Thumbnail => {
    let config = Config::from_env();
    let pool = eidetic_db::connect(&config).await?;
    let repo = eidetic_db::PgAssetsRepo::new(pool);

    let pending = repo.fetch_unthumbnailed().await?;
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
        let result = tokio::task::spawn_blocking(move || {
            eidetic_ingest::thumbnail::generate_thumbnails(&storage_path, &hash, &lib)
        })
        .await
        .context("thumbnail thread panicked")?;

        match result {
            Ok(()) => {
                if let Err(e) = repo.mark_thumbnailed(id).await {
                    // Keep going — assets without the flag set are retried
                    // on the next run, same pattern as eidetic embed.
                    eprintln!("  [{}/{}] mark failed: {e}", i + 1, total);
                    failed += 1;
                } else {
                    println!("[{}/{}] {}", i + 1, total, id);
                    generated += 1;
                }
            }
            Err(e) => {
                eprintln!("  [{}/{}] failed: {e}", i + 1, total);
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

Simpler than `eidetic embed` because thumbnail generation doesn't need a long-lived ONNX session held on a worker thread. Each thumbnail is its own `spawn_blocking`.

### `eidetic stats` extension

Add one line to the existing `Stats` output:

```
Thumbnails  {:>8}
  pending   {:>8}
```

where `pending` is unthumbnailed images (the `fetch_stats` query gets one more count expression). Small enough to ship in the same PR.

---

## Failure modes

| Failure | Behavior |
|---|---|
| `image::ImageReader::open` fails | Warn, set `thumbnails_generated = false`, continue. |
| `decode()` rejects the file (corrupt header, format unsupported by `image`) | Same — warn, leave row unthumbnailed. |
| Decompression-bomb limit exceeded | Same — warn. The asset is already in CAS, but no thumbnail. |
| WebP encode fails | Same — warn, no thumbnail. |
| Filesystem write fails (disk full, permissions) | Same — warn. Distinguishable in logs via the I/O error context. |
| Small writes but medium fails | Function returns `Err`. Both partials may exist on disk. Re-running `eidetic thumbnail` overwrites both. |
| `mark_thumbnailed` fails after successful generation | Warn, count as failed. Next run will redo the (already-on-disk) thumbnails — wasteful but correct. |

Bad inputs never bring down ingest. The asset row lands; the thumbnail just gets retried later.

---

## Testing approach

Reuse the established pattern:

- **Unit tests in `crates/eidetic-ingest/src/thumbnail.rs`** for the pure functions: `thumbnail_path` correctness, `dir_segment` / `longest_edge`. Use the same synthetic JPEG-stub trick already in `import.rs` tests to validate that `generate_thumbnails` produces files at the expected paths and that they decode back as valid images. Probably 4–6 small tests.

- **Integration tests in `crates/eidetic-db/tests/import_integration.rs`** for the end-to-end path:
  - `imported_image_has_thumbnails_generated_true` — `import_file`, then SELECT `thumbnails_generated` to confirm it's `TRUE` and both thumb files exist on disk.
  - `imported_video_has_thumbnails_generated_false` — same, but with a non-image stub; confirm the column stays `FALSE`.
  - `import_dir_generates_thumbnails_for_each_image` — directory of three images, then verify counts via `fetch_unthumbnailed()` (should be empty).

- **Integration tests for the backfill command** in `crates/eidetic-db/tests/thumbnail_integration.rs` (new file):
  - Seed an asset with `thumbnails_generated = false` (raw INSERT, bypassing `import_file`); run `fetch_unthumbnailed`; expect one result.
  - Run `generate_thumbnails` against a real source image, then `mark_thumbnailed`; assert the flag is `TRUE` and `fetch_unthumbnailed` is now empty.

No new test-only dependencies. `image::ImageReader::open` already handles the stub-JPEG `[0xFF, 0xD8, 0xFF, 0xE0]` magic well enough to fail at decode (it isn't a real JPEG body), which is exactly what we want to assert on the failure path — write a 4-byte JPEG-magic stub, attempt to thumbnail it, expect failure, expect column stays `FALSE`.

---

## Dependencies

The `image` crate is already in `workspace.dependencies` and is already a dep of `eidetic-ingest` (for `Limits`) and `eidetic-ml` (for preprocessing). WebP encoding is built into the `image` crate's default features. **Zero new dependencies.**

---

## CLI surface (final)

After this work:

```
eidetic import <path>      # ingest, embed-pending, thumbnail-pending
eidetic embed              # backfill embeddings for embedding IS NULL rows
eidetic thumbnail          # backfill thumbnails for thumbnails_generated = false rows
eidetic search <query>     # semantic search
eidetic stats              # now shows thumbnail-pending count
```

`eidetic import` does NOT trigger embedding or thumbnail backfill for previously-imported rows — it only handles new files. The two backfill commands are the way to fill in missing data on rows that landed before this feature existed (or whose generation failed transiently).

---

## Migration safety

Migration `003_thumbnails.sql` adds one nullable-default column. Existing rows get `FALSE` automatically. No data loss. No backfill required to apply the migration — running `eidetic thumbnail` after migration generates thumbnails for the existing library.

The migration is forward-only (no `DOWN.sql`); reverting requires dropping the column manually if ever needed. Matches the existing migration discipline in `migrations/`.

---

## Out of scope (deferred follow-ups)

- Video thumbnails (need `ffmpeg`, deferred to a future spec).
- Adaptive sizing or `srcset`-aware multiple variants.
- Pre-generation of thumbnails on a separate worker pool for performance.
- HTTP / WebDAV exposure of thumbnails (separate Phase).
- Garbage collection of orphaned thumbnails (assets deleted with `ON DELETE CASCADE` is currently the migration's responsibility; thumbnail files become orphans on disk). A `eidetic gc` command is a future-spec problem.

---

## Implementation order (hint for the plan author)

1. Migration `003_thumbnails.sql` + `NewAsset` field + `insert_asset` SQL update.
2. New `eidetic-ingest::thumbnail` module + its unit tests.
3. Wire into `import_file`; update CLI `Stats` query.
4. Add `fetch_unthumbnailed` + `mark_thumbnailed` repo methods + their tests.
5. Add `Command::Thumbnail` CLI subcommand.
6. Integration tests in `eidetic-db/tests/`.
7. Update AGENTS.md if anything load-bearing changed.
8. Verify (fmt, clippy, test, cargo-deny) and open PR.

Each step ends with `cargo clippy --workspace --all-targets -- -D warnings` green — the workspace pre-commit hook does not accept intermediate broken states. The whole feature will land as one PR with a small number of atomic commits.
