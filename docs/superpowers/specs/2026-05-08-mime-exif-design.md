# MIME detection + EXIF extraction

**Date:** 2026-05-08
**Status:** Approved

## What this covers

Phase 1 follow-up: detect MIME type from file content so non-media files can be filtered during import, and extract EXIF metadata (date taken, GPS coordinates, camera make/model) from image files. Populates the `mime_type` column that Phase 1 left as `NULL`, and adds five new EXIF columns to `assets`.

Explicitly out of scope: video metadata extraction, thumbnail generation, file watcher.

---

## Approach

Single-pass inline enrichment inside `import_file`. MIME detection is the first step in the pipeline (before hashing) so non-media files are rejected cheaply. EXIF extraction happens after the CAS copy, before the DB insert, so the row is always fully populated when it lands in the database.

No new crate. Everything lives in `eidetic-ingest` — only one caller exists (`import_file`), which is below the two-caller threshold in AGENTS.md. New logic goes in `src/meta.rs`.

---

## New dependencies

| Crate | Purpose |
|---|---|
| `infer` | Magic-byte MIME detection — pure Rust, no system deps |
| `kamadak-exif` | EXIF parsing for JPEG / TIFF / HEIF — pure Rust, named in Phase 1 spec |

`chrono` is already in workspace deps. Both new crates added to `[workspace.dependencies]` in root `Cargo.toml` and to `[dependencies]` in `crates/eidetic-ingest/Cargo.toml`.

---

## Pipeline change

Current order in `import_file`:
```
size → hash → dedup → CAS copy → DB insert
```

New order:
```
MIME detect → filter → size → hash → dedup → CAS copy → EXIF extract → DB insert
```

Step-by-step:

1. `detect_mime(path)` — reads ≤512 bytes
   - `Err` → `ImportOutcome::Failed`
   - `None` (unrecognized magic bytes) → `ImportOutcome::Skipped`
   - `Some(mime)` where `!mime.starts_with("image/") && !mime.starts_with("video/")` → `ImportOutcome::Skipped`
2. `std::fs::metadata(path)` → `file_size` (unchanged)
3. `hash_file(path)` (unchanged)
4. `index.find_by_hash(&hash)` (unchanged)
5. `store_file(path, &hash, &library_dir)` (unchanged)
6. If `mime.starts_with("image/")`: `extract_exif(path)` → `ExifData`; else `ExifData::default()` (all `None` for video)
7. `index.insert_asset(NewAsset { …, mime_type: Some(mime), …exif })` (unchanged call site, expanded struct)

---

## New module: `eidetic-ingest/src/meta.rs`

```rust
pub struct ExifData {
    pub date_taken: Option<DateTime<Utc>>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

impl Default for ExifData { /* all None */ }

/// Read magic bytes to identify MIME type.
/// Returns None if the format is unrecognized (not a failure).
pub fn detect_mime(path: &Path) -> Result<Option<String>>

/// Extract EXIF fields from an image file.
/// Never fails — parse errors are logged at debug level and produce None fields.
pub fn extract_exif(path: &Path) -> ExifData
```

### `detect_mime`

Opens the file, reads up to 512 bytes, passes to `infer::get`. Returns `Some("image/jpeg")` etc. Returns `None` if `infer` does not recognize the magic bytes. The only `Err` case is an IO failure opening or reading the file.

### `extract_exif`

Opens the file, runs `kamadak_exif::Reader::new()`. Fields extracted:

- **`date_taken`**: `DateTimeOriginal` tag → parsed into `DateTime<Utc>`. EXIF datetimes carry no UTC offset; the raw value is treated as UTC. This is a known imprecision — the camera clock is not necessarily in UTC — acceptable for a personal library.
- **`latitude`** / **`longitude`**: `GPSLatitude` + `GPSLatitudeRef` and `GPSLongitude` + `GPSLongitudeRef`. DMS rational triplets converted to decimal degrees. South and West values are negated.
- **`camera_make`**: `Make` tag, trailing whitespace trimmed (some cameras pad these strings).
- **`camera_model`**: `Model` tag, same trimming.

Any tag that is missing or fails to parse produces `None` for that field, logged at `tracing::debug`. A file with corrupt EXIF still imports successfully with all EXIF fields `None`.

---

## Data model changes

### `NewAsset` in `eidetic-ingest/src/repo.rs`

```rust
pub struct NewAsset {
    pub hash: String,
    pub original_filename: String,
    pub storage_path: PathBuf,
    pub file_size: u64,
    pub mime_type: Option<String>,         // now populated
    pub date_taken: Option<DateTime<Utc>>, // new
    pub latitude: Option<f64>,             // new
    pub longitude: Option<f64>,            // new
    pub camera_make: Option<String>,       // new
    pub camera_model: Option<String>,      // new
}
```

### `ImportOutcome` in `eidetic-ingest/src/import.rs`

```rust
pub enum ImportOutcome {
    Imported(AssetId),
    Duplicate(AssetId),
    Skipped,    // not a media file (MIME not image/* or video/*)
    Failed(Error),
}
```

### `ImportSummary` in `eidetic-ingest/src/import.rs`

```rust
pub struct ImportSummary {
    pub imported: u32,
    pub duplicates: u32,
    pub skipped: u32,   // new
    pub failed: Vec<(PathBuf, Error)>,
}
```

---

## Database changes

### Migration `migrations/003_exif.sql`

```sql
ALTER TABLE assets
    ADD COLUMN date_taken   TIMESTAMPTZ,
    ADD COLUMN latitude     FLOAT8,
    ADD COLUMN longitude    FLOAT8,
    ADD COLUMN camera_make  TEXT,
    ADD COLUMN camera_model TEXT;

CREATE INDEX assets_date_taken_idx
    ON assets (date_taken DESC)
    WHERE date_taken IS NOT NULL;
```

Additive only — no existing data is touched. The partial index covers only rows with a date, keeping it lean.

Geo queries on lat/lng do not need an index for a personal library at current scale; add one when needed.

### `PgAssetsRepo::insert_asset` in `eidetic-db/src/assets.rs`

The INSERT statement gains five new bound parameters for the EXIF columns. `find_by_hash` is unchanged.

---

## CLI output change

`eidetic-cli/src/main.rs` — directory import summary gains a Skipped line:

```
Imported     42 files
Duplicates    3 files
Skipped       8 files
Failed        1 files
  /path/to/bad.jpg — …
```

Single-file import: `Skipped` prints a one-line message and exits 0 (not an error).

---

## Testing

### `eidetic-ingest` unit tests

**`meta.rs` — `detect_mime`:**
- JPEG magic bytes (`FF D8 FF`) → `Some("image/jpeg")`
- PNG magic bytes (`89 50 4E 47`) → `Some("image/png")`
- MP4 magic bytes → `Some("video/mp4")`
- Random / unrecognized bytes → `None`
- Non-existent path → `Err`

**`meta.rs` — `extract_exif`:**
- File with no EXIF → all fields `None`, no panic
- Minimal JPEG fixture with known EXIF → all five fields populated with expected values
- Corrupt bytes → all fields `None`, no panic

EXIF test fixture: a minimal valid JPEG with hand-crafted EXIF (a few hundred bytes) committed to `crates/eidetic-ingest/tests/fixtures/`. No full-size photo needed.

**`import.rs` — `import_file` with `MockAssetIndex`:**
- Non-media file (random bytes, no recognizable magic) → `Skipped`
- JPEG file → `Imported`, `mime_type` is `Some("image/jpeg")`
- MP4 file → `Imported`, EXIF fields all `None`

**`import.rs` — `import_dir`:**
- Directory with media + non-media files → `skipped` counter matches non-media count

### `eidetic-db` integration tests (testcontainers)

- `insert_asset` updated to supply all new `NewAsset` fields
- Existing round-trip test (`insert` then `find_by_hash`) updated — no new DB test needed for EXIF columns since `find_by_hash` only returns the `AssetId`

---

## Deferred

| Item | When |
|------|------|
| Video metadata extraction (creation date, duration) | Later phase; needs different library / ffprobe |
| Geo search index on lat/lng | When geo queries exist |
| Timezone-aware EXIF datetime | When user configures camera timezone |
| Thumbnail generation | Phase 1 follow-up (separate spec) |
| File watcher | Phase 1 follow-up (separate spec) |
