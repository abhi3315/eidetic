# Comprehensive EXIF extraction

**Status:** Design draft 2026-05-24. Verification-gated. V1, V2, V3, V5 already PASS from probes against the user's real iPhone HEIC/JPEG/DNG corpus (see annotations inline). V4 (Postgres column-add cost on 779 populated rows) is the only remaining uncertainty and has a documented mitigation.

**Working-state baseline:** `main` at `ac4fe45` (the DNG-support merge). No prerequisite PRs.

**Scope:**

1. **Long-tail JSONB.** New column `assets.exif_raw JSONB` storing every tag `kamadak-exif` can parse from the primary IFD. One key per tag; value is the human-readable display string. Survives schema evolution: new tags added in future EXIF spec revisions show up automatically once kamadak-exif learns them.
2. **Typed columns for tier-2 camera-setting fields.** Nine new nullable columns on `assets`: `lens_make`, `lens_model`, `focal_length`, `focal_length_35mm`, `aperture`, `shutter`, `iso`, `orientation`, `altitude`, `gps_direction`. These are what the detail page wants to render without parsing JSONB on every request, and they let SQL queries filter/sort on lens model or ISO without a JSON operator.
3. **Detail-page surfacing.** A new "Camera settings" section on `/assets/:id` that renders the lens / focal length / aperture / shutter / ISO block when present. Existing "Camera", "GPS", "Taken" rows stay; the new section sits between the "Camera" row and the "GPS" row.
4. **Backfill subcommand, not re-import.** New `eidetic backfill-exif` reads `storage_path` for every asset row, re-runs `extract_exif`, and `UPDATE`s the new columns. 779 files × milliseconds-per-parse is seconds of work; re-import would re-hash + re-thumbnail + re-embed 6 GB of files for no gain.

**Out of scope** — explicit list in §Honest scope. Highlights: no Apple MakerNote parsing (kamadak-exif can't decode it; verified), no reverse geocoding, no video metadata, no display-formatting polish.

---

## Verification gate

Five load-bearing claims. V1, V2, V3, V5 verified against `IMG_3493.HEIC` (iPhone 15), `IMG_2238.JPG` (iPhone 15 Pro Max), `IMG_1561.DNG` (iPhone 15 Pro Max) from `/Volumes/Data/Dalhousie /Abhishek/` via a scratch crate at `/tmp/exif-probe/`. V4 is the only one whose answer I assumed; the plan re-confirms it before the migration commit fires.

| # | Claim | If false | Verified 2026-05-24 |
|---|---|---|---|
| **V1** | `kamadak-exif 0.6` exposes every tier-2 tag I want via named `Tag` enum variants: `LensMake`, `LensModel`, `FocalLength`, `FocalLengthIn35mmFilm`, `FNumber`, `ExposureTime`, `PhotographicSensitivity`, `Orientation`, `GPSAltitude`, `GPSAltitudeRef`, `GPSImgDirection`, `GPSImgDirectionRef`. | Replace the missing names with `Tag::Unknown(_, n)` matches by numeric tag ID. Cost: trivial — the EXIF spec freezes the numbers. | **PASS.** All twelve tags surfaced by name on at least one of the three sample files; iPhone 15 HEIC carries all twelve simultaneously. Display strings from the probe: `LensMake = "Apple"`, `LensModel = "iPhone 15 back dual wide camera 5.96mm f/1.6"`, `FocalLength = 5.96 mm`, `FocalLengthIn35mmFilm = 66 mm`, `FNumber = f/1.6`, `ExposureTime = 1/25 s`, `PhotographicSensitivity = 400`, `Orientation = row 0 at top and column 0 at left` (numeric value 1), `GPSAltitude = 2032.76 meters above sea level`, `GPSAltitudeRef = above sea level`, `GPSImgDirection = 8.108 degrees in true direction`, `GPSImgDirectionRef = true direction`. |
| **V2** | iPhone HEIC + JPEG + DNG all carry these tags in **IFD 0** (`In::PRIMARY`), uniformly. No fishing in sub-IFDs or GPS-IFD. | Branch `extract_exif` to also walk `In::GPS` or hand-roll IFD descent. ~30 lines, well-scoped. | **PASS.** All twelve named tags read with `exif.get_field(Tag::X, In::PRIMARY)` across all three formats. kamadak-exif folds the GPS sub-IFD into `In::PRIMARY` queries automatically (it knows GPS-tag → GPS-IFD routing). The DNG-spec extension tags (e.g. `Tag(Tiff, 50706)` = `DNGVersion`) are also in IFD 0 — they fall into the JSONB blob via the catch-all walk, no special-casing. |
| **V3** | `kamadak-exif` does NOT parse `Apple MakerNote` contents; the tag's display value is an opaque hex blob with no sub-tags exposed. (User wanted this verified before committing to defer.) | Add an Apple-MakerNote parser. Substantial work; out of scope for this PR by user direction. | **PASS — deferred is correct.** `MakerNote` field on all three files shows as `0x4170706c6520694f53...` (hex of `"Apple iOS\0\0\x01..."`) followed by 3000+ raw bytes. No structured sub-fields. Useful Apple data (ProRAW flag, scene scores, depth-map refs) lives behind a proprietary TLV format kamadak doesn't decode. Out of scope; noted in §Honest scope. |
| **V4** | `ALTER TABLE assets ADD COLUMN <name> <type> NULL` (no `DEFAULT`) on a populated 779-row table is fast (~ms, no table rewrite). | If the migration locks the table for long enough to be user-visible, restructure as ten separate `ADD COLUMN` migrations and run them one at a time. (Still effectively free per Postgres 11+ docs, but breaks the change into reversible bites.) | **Confirmed by Postgres docs — not yet runtime-verified.** Per Postgres 11+ docs, `ADD COLUMN ... NULL` with no `DEFAULT` writes only the catalog; the new column is materialised lazily on row update. 779 rows is small enough that even a full rewrite would finish before you noticed. Plan Task 1 still runs the migration on a fresh local DB seeded with 779 dummy rows and times it as a final check. |
| **V5** | `kamadak-exif`'s existing `Reader::new().read_from_container(...)` accepts all three of JPEG, HEIC (HEIF), and DNG (TIFF) without format-specific routing. | Branch on file extension and call format-specific reader paths. Minor code cost. | **PASS.** The existing `extract_exif` already calls `read_from_container` and reads JPEG/HEIC/DNG today (production code, in use). The scratch probe ran the same call against all three sample files and parsed cleanly (59, 52, 79 fields respectively). |

Plan Task 1 re-confirms V4 against a local DB; the others get a one-line `<!-- Verified 2026-05-24: ... -->` annotation at the relevant section. No production code lands before Task 1 completes.

---

## Goals

- Every asset that already parses EXIF today (anything with a kamadak-readable container) gets the same depth of data captured on **import** going forward.
- Detail page at `/assets/:id` shows lens / focal length / aperture / shutter / ISO inline, in addition to the existing camera-make/model / GPS / date rows.
- SQL queries can filter on `iso > 1600` or `lens_model LIKE '%triple camera%'` without needing JSONB operators.
- The 779 already-imported assets get backfilled by running one command, no re-import required.
- One migration, additive only. Zero risk to existing reads.
- `cargo clippy --workspace --all-targets -- -D warnings` green on every commit.

## Non-goals

- **Apple MakerNote parsing** (V3-verified opaque). ProRAW flag, scene scores, depth-map references — all Apple-internal. The fix is a separate parser and is a much bigger PR; defer.
- **Reverse geocoding** (`GPSLatitude/Longitude → "Dharamshala, India"`). Separate PR (next one in the planned sequence; mentioned in the conversation that opened this design).
- **Video metadata.** 115 video rows currently carry no metadata. Video EXIF is a different parser (the metadata lives in MP4/MOV atoms, not TIFF/JPEG/HEIF). Separate PR.
- **Display formatting polish.** The detail page renders numeric values as Rust's default `Display` impl. A "f/1.78" / "1/120 s" pretty-printer pass can land later; this PR ships the data plumbing first.
- **JSON parsing in SQL.** Adding an index on a JSONB expression (e.g. `exif_raw->>'LensModel'`) is premature. We have typed columns for the queries we know we want; the JSONB tail is for ad-hoc inspection.
- **Tag-name canonicalisation.** Unknown tags arrive as their `Display` form, e.g. `"Tag(Tiff, 50706)"`. That's ugly but stable. Rewriting to `"DNGVersion"` would require maintaining a sidecar map; out of scope. They'll be readable as-is.
- **EXIF orientation rotation fix.** Still pending from the HEIC PR. JPEG previews shot in portrait still render sideways. Not regressed by this PR; not fixed by this PR.

---

## Architecture overview

EXIF extraction stays in `crates/eidetic-ingest/src/meta.rs::extract_exif`. The function signature changes (more fields on `ExifData`), but the call site (`crates/eidetic-ingest/src/import.rs`) only learns the new fields it has to thread into `NewAsset`.

```
   import_file(path)
        │
        ├──► meta::detect_mime(path)            (unchanged)
        │
        ├──► meta::extract_exif(path)  ──►  ExifData {
        │                                      // existing
        │                                      date_taken, latitude, longitude,
        │                                      camera_make, camera_model,
        │                                      // NEW typed
        │                                      lens_make, lens_model,
        │                                      focal_length, focal_length_35mm,
        │                                      aperture, shutter, iso,
        │                                      orientation, altitude, gps_direction,
        │                                      // NEW long tail
        │                                      raw: serde_json::Value (object)
        │                                    }
        │
        └──► PgAssetsRepo::insert_asset(NewAsset { …extra fields… })
                                              │
                                              ▼
                                     UPDATE assets SET …
```

Everything else (hashing, CAS layout, thumbnail/embed pipeline) is untouched. No new crate. The user's CLAUDE.md says "when tempted to add a new method or property, first ask 'can I remove something instead?'" — answer here: no, we genuinely need ten new typed columns + one JSONB. They map 1:1 to EXIF concepts that already exist in the source data; no derived or redundant state.

### The JSONB tail

For each `Field` in `exif.fields()` where `field.ifd_num == In::PRIMARY`:

- **Key** = `field.tag.to_string()` — e.g. `"LensMake"`, `"FNumber"`, `"GPSAltitude"`. For tags kamadak doesn't recognise (DNG-spec extensions like `0xc612`), this falls back to `"Tag(Tiff, 50706)"`. Ugly but unique and stable across kamadak versions.
- **Value** = `field.display_value().with_unit(&exif).to_string()` — the human-readable rendition kamadak prints. Examples from the probe: `"f/1.6"`, `"1/25 s"`, `"5.96 mm"`, `"Apple"`. Quoted strings keep their surrounding quotes (that's how kamadak displays ASCII fields); accepted as kamadak's choice.

What we **skip**:

- **IFD 1** (thumbnail-IFD). Carries duplicates of IFD-0 tags plus the JPEG-thumb pointer that we don't store. Adds noise. Skipping is a one-line filter.
- **MakerNote** specifically. It's already a 3 KB-of-hex string per file; storing it in JSONB blows the row size for no parseable value. Filtered out by tag match.
- **The five "old" tier-1 fields** (`Make`, `Model`, `DateTimeOriginal`, `GPSLatitude`, `GPSLongitude`) — kept in the JSONB blob too. Duplication with typed columns is fine; the JSONB is the long-tail catalog, not a "fields not already typed" subset. Keeps the blob's meaning predictable: "every tag we could parse."
- **The ten "new" tier-2 fields** — also kept in the JSONB blob, same reasoning.

Storage estimate: per the probe, an iPhone JSON would be ~50 keys × ~30 bytes average ≈ 1.5 KB per row. 779 rows × 1.5 KB ≈ 1.2 MB total JSONB load. Negligible.

### The typed columns

One match per tag, mapped to the column. Each lookup is `exif.get_field(Tag::X, In::PRIMARY)`.

| Column | Type | Source tag | Conversion |
|---|---|---|---|
| `lens_make` | TEXT NULL | `Tag::LensMake` | `read_ascii` (existing helper) |
| `lens_model` | TEXT NULL | `Tag::LensModel` | `read_ascii` |
| `focal_length` | REAL NULL | `Tag::FocalLength` | first rational → `f32` in mm |
| `focal_length_35mm` | REAL NULL | `Tag::FocalLengthIn35mmFilm` | SHORT → `f32` in mm (kamadak surfaces it as Value::Short; cast) |
| `aperture` | REAL NULL | `Tag::FNumber` | first rational → `f32` (f-stop) |
| `shutter` | TEXT NULL | `Tag::ExposureTime` | first rational → `"{num}/{denom}"` if `num` is 1 and `denom ≥ 1`; else `"{num/denom:.3}"` (decimal seconds). No unit suffix in the stored value (UI adds " s" when rendering) |
| `iso` | INTEGER NULL | `Tag::PhotographicSensitivity` | SHORT → `i32`. Fallback chain: try `Tag::PhotographicSensitivity` first, then `Tag::ISOSpeed` (newer EXIF 2.3 alt; not observed on iPhone, included for forward-compat) |
| `orientation` | SMALLINT NULL | `Tag::Orientation` | SHORT → `i16` (range 1–8 per EXIF spec) |
| `altitude` | FLOAT8 NULL | `Tag::GPSAltitude` + `Tag::GPSAltitudeRef` | rational → `f64` meters, negate if `GPSAltitudeRef == 1` (below sea level). Mirrors how the existing GPS-lat/lon parsing handles N/S/E/W. Type matches the existing `latitude`/`longitude` columns (`FLOAT8`) from `migrations/003_exif.sql`. |
| `gps_direction` | FLOAT8 NULL | `Tag::GPSImgDirection` | rational → `f64` degrees (0–360). `GPSImgDirectionRef` ("T" true / "M" magnetic) is captured in JSONB only — typed column carries the bare degrees; semantic interpretation can be done via JSONB later if it ever matters. Type matches the existing GPS columns. |

All columns are nullable: most iPhone photos won't have a `GPSImgDirection` if the user wasn't moving / compass wasn't engaged; lens info is only present on actual photos taken (not screenshots imported into the library). Nullable everywhere keeps the import path forgiving.

### `ExifData` and `NewAsset` shape

`ExifData` (in `meta.rs`) gains ten typed fields plus the JSONB blob. `NewAsset` mirrors them. Both grow but stay flat — no nested struct (`CameraSettings { ... }`) because that's added indirection for one caller. CLAUDE.md design rule: "When tempted to add a new method or property, first ask 'can I remove something instead?'" — applied here in reverse: ten fields are warranted because each one is a separate column queryable from SQL. Bundling them into a single struct would block that.

`raw` is `Option<serde_json::Value>` typed as a JSON object. `None` when the file has no parseable EXIF at all (matches existing behaviour: video files, random binary blobs, corrupt headers). `Some({})` is technically representable but the implementation should collapse "no EXIF at all" to `None` to keep "is this row's exif backfilled?" simple. The backfill subcommand uses `WHERE exif_raw IS NULL` to find pending rows; collapsing to `None` makes that filter mean "no EXIF yet OR file genuinely had none." That's intentional — re-running backfill on a row that genuinely has no EXIF is cheap (open, parse-fails, write NULL again) and idempotent.

### Detail-page section

`crates/eidetic-server/src/views.rs::detail_page` gains a new `dl` block between the existing "Camera" row and the "GPS" row. Sketch:

```rust
// Inside the dl, after the existing Camera rows, before GPS:
@if view.lens_model.is_some()
    || view.focal_length.is_some()
    || view.aperture.is_some()
    || view.shutter.is_some()
    || view.iso.is_some()
{
    dt { "Lens" }
    dd {
        @if let Some(lens) = &view.lens_model { (lens) }
    }
    @if let Some(f) = view.focal_length {
        dt { "Focal length" }
        dd {
            (format!("{f:.1} mm"))
            @if let Some(eq) = view.focal_length_35mm {
                " (" (format!("{eq:.0}")) " mm equiv.)"
            }
        }
    }
    @if let Some(a) = view.aperture {
        dt { "Aperture" } dd { (format!("f/{a:.1}")) }
    }
    @if let Some(s) = &view.shutter {
        dt { "Shutter" } dd { (s) " s" }
    }
    @if let Some(iso) = view.iso {
        dt { "ISO" } dd { (iso) }
    }
}

// Existing GPS row, augmented with altitude when present:
@if let (Some(lat), Some(lon)) = (view.latitude, view.longitude) {
    dt { "GPS" }
    dd {
        (format!("{lat:.4}, {lon:.4}"))
        @if let Some(alt) = view.altitude {
            " (" (format!("{alt:.0}")) " m)"
        }
    }
}
```

`DetailView` (the local server struct mirroring `AssetDetail`) gains the same ten typed columns. `fetch_by_id`'s SELECT list expands. Same mechanical pattern as the recent `GridTile` expansion in the DNG PR.

The `orientation`, `gps_direction`, and `exif_raw` fields are loaded into `AssetDetail` but not rendered in v1. Orientation matters for the EXIF rotation fix (separate task). `gps_direction` and the JSONB blob are stored-but-hidden — the data is there if a follow-up wants to render it.

### Why not nest into a single "Camera" block

The detail page already has a "Camera" row showing `make model`. Splitting "Camera" (make + model, top-level identity of the device) from "Lens" + the settings (per-shot variables) reads naturally — they're different categories. The existing `Camera` row stays as-is; the new fields ride below it. No restructuring of existing rendering.

---

## Schema

One migration: `migrations/005_comprehensive_exif.sql`.

```sql
-- Comprehensive EXIF capture. Adds typed columns for the camera-settings fields
-- the detail page renders directly, plus a JSONB column for the long tail of
-- every tag kamadak-exif can parse.
--
-- All columns are nullable; backfill populates them via `eidetic backfill-exif`.
-- Per Postgres 11+ docs, ADD COLUMN ... NULL with no DEFAULT is a catalog-only
-- change (no table rewrite); safe to run with the application live.

ALTER TABLE assets
    ADD COLUMN lens_make          TEXT,
    ADD COLUMN lens_model         TEXT,
    ADD COLUMN focal_length       REAL,
    ADD COLUMN focal_length_35mm  REAL,
    ADD COLUMN aperture           REAL,
    ADD COLUMN shutter            TEXT,
    ADD COLUMN iso                INTEGER,
    ADD COLUMN orientation        SMALLINT,
    ADD COLUMN altitude           FLOAT8,
    ADD COLUMN gps_direction      FLOAT8,
    ADD COLUMN exif_raw           JSONB;
```

No indexes added. No FK changes. No constraint changes. The 779 existing rows get NULL in every new column; backfill populates them on a follow-up command run.

`gen_random_uuid()`-style DEFAULTs are deliberately avoided — they'd force a table rewrite to materialise the default for every existing row, which is the slow path V4 mitigates against.

The `down` migration (if we had one — Eidetic doesn't maintain reverse migrations, but conceptually) is `ALTER TABLE assets DROP COLUMN ...` for the same eleven names.

---

## Backfill subcommand

New `eidetic backfill-exif` subcommand. Same wiring pattern as `eidetic thumbnail` and `eidetic embed`.

### Why a subcommand and not re-import

Re-importing the user's 6 GB / 779-file library means re-hashing every file (SHA-256 is cheap but I/O-bound), re-running thumbnail generation (CPU + disk for the resized files), and re-running SigLIP embedding (GPU-warm-up + onnxruntime forward passes, the slowest step by 10×). EXIF parsing is on the order of milliseconds per file. Forcing a re-import would mean redoing seconds-to-minutes of unrelated work to capture data that's already on disk in the original files we've already hashed and stored.

The subcommand reads `storage_path` (already on the row), opens that file (it's the CAS-canonical copy, identical bytes to the original we imported), reparses EXIF, and `UPDATE`s. ~2-3 minutes of work for 779 files at 200ms/file (generous estimate). No GPU, no re-hash, no thumbnail churn.

### Behaviour

```
$ eidetic backfill-exif
Backfilling EXIF for 664 image assets (115 video rows skipped)…
[ 50/664] IMG_3493.HEIC — 53 fields
[100/664] IMG_2238.JPG — 47 fields
…
[664/664] done. 654 rows updated, 10 rows had no parseable EXIF.
```

(Row counts are illustrative — exact image-vs-video split is computed from `WHERE mime_type LIKE 'image/%'` at runtime.)

Selection and update both live on `PgAssetsRepo` (matches the existing pattern at `crates/eidetic-db/src/assets.rs:134-151` where `fetch_unembedded` and `fetch_unthumbnailed` are repo methods, not inline SQL in the CLI):

- **`PgAssetsRepo::fetch_pending_exif_backfill()`** — returns `Vec<(AssetId, PathBuf, String)>` of `(id, storage_path, original_filename)`. SQL:
  ```sql
  SELECT id, storage_path, original_filename FROM assets
   WHERE exif_raw IS NULL AND mime_type LIKE 'image/%'
   ORDER BY imported_at
  ```
  The `mime_type LIKE 'image/%'` filter mirrors `fetch_unembedded` and `fetch_unthumbnailed`. Without it, the 115 video rows would be opened on every backfill run only to fail parsing — wasted I/O. (After the first run they'd get the `{}` sentinel and be skipped, but the wasted pass on first run is avoidable and the symmetry with the other repo methods is the right convention.)

- **`PgAssetsRepo::update_exif_columns(id, ExifData)`** — runs one UPDATE per row (batched is unnecessary; <1000 image rows total in practice):
  ```sql
  UPDATE assets SET
      lens_make = $2, lens_model = $3,
      focal_length = $4, focal_length_35mm = $5,
      aperture = $6, shutter = $7, iso = $8,
      orientation = $9, altitude = $10, gps_direction = $11,
      exif_raw = $12
  WHERE id = $1
  ```

The CLI handler in `eidetic-cli` calls these two repo methods. Inline SQL in the CLI handler would break the existing convention (every asset query is a method on `PgAssetsRepo`) and would also make the integration test harder to seam.

When `extract_exif` returns "no EXIF at all" (`exif_raw = None`, all typed fields `None`), the row still gets written: all NULLs **plus `exif_raw = '{}'::jsonb`**. That `{}` sentinel marks "backfill ran on this row; there's nothing here." Subsequent runs skip it because `WHERE exif_raw IS NULL` only matches actually-pending rows. The chosen sentinel is the empty JSON object (truthy from a "this row was processed" perspective; falsy from a "do we have any EXIF data?" perspective via `exif_raw = '{}'::jsonb`).

`Tag::FocalLengthIn35mmFilm` and the others — when absent, the corresponding column stays NULL. When the file has at least one tag the function recognises, `exif_raw` gets at least that tag and is non-empty.

### Going-forward import path

`crates/eidetic-ingest/src/import.rs::import_file` calls `extract_exif` once today and threads the result into `NewAsset`. The plan changes `NewAsset` to carry the ten new typed fields plus the JSONB. New imports populate everything at import time; the backfill subcommand exists only to populate the rows imported before this PR.

After backfill runs once, the subcommand becomes effectively a no-op (`WHERE exif_raw IS NULL` matches nothing). It stays in the CLI as a recovery tool in case the user ever loses the EXIF columns (e.g. a botched manual UPDATE).

### Idempotency / dry-run

Add `--dry-run` flag that runs `extract_exif` and prints what would change but doesn't UPDATE. Useful for confirming the parse outputs match expectations before scribbling over 779 rows. The flag adds two lines in the command handler.

---

## Failure-mode semantics

Three buckets:

1. **File missing on disk.** Backfill sees `storage_path` exists in the row but the file isn't there. Log a warning, skip the row, move on. Don't write any UPDATE for that row — `exif_raw` stays NULL so a later backfill picks it back up. Matches how `eidetic thumbnail` and `eidetic embed` handle this case today.

   *Note on video rows:* the selection query filters `mime_type LIKE 'image/%'`, so the 115 video rows are never enumerated by backfill in the first place. Their `exif_raw` stays NULL permanently, by design — there's nothing for kamadak to parse out of MP4/MOV containers. If video-metadata extraction is added later (separate PR per §Non-goals), that PR introduces its own column or sentinel; conflating image-EXIF NULL with video-untouched NULL is fine for now.
2. **File present but has no parseable EXIF.** kamadak returns an error from `read_from_container`. `extract_exif` already swallows this and returns `ExifData::default()` (existing behaviour at `meta.rs:39-43`). Under the new design that maps to `exif_raw = Some(json!({}))` so the row gets the `{}` sentinel and is marked "processed, nothing to see."
3. **File has EXIF, but a specific tag is malformed.** The named-tag readers (`read_ascii`, the rational-coords reader) already return `None` on bad data. New per-tag readers follow the same pattern — bad data on `Tag::FocalLength` doesn't trash the row, it just leaves `focal_length` NULL while every other field still populates. The JSONB walk also tolerates individual fields that fail to display — kamadak's `display_value` API is total (it always produces a string).

No `unwrap`, no row loss, no UPDATE that writes a partial-mess back to the DB. Standard repo convention (AGENTS.md "Errors" section).

---

## Dependencies

One workspace-level change (sqlx feature), plus three crate-level `serde_json` adds. Verified by grepping all `Cargo.toml` files in the repo on 2026-05-24.

**Workspace-level change:**

- **`sqlx` — add `"json"` feature.** Current workspace declaration at `Cargo.toml:24` reads:

  ```toml
  sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "uuid", "chrono"] }
  ```

  Binding `serde_json::Value` (or `sqlx::types::Json<serde_json::Value>`) to a `JSONB` column in sqlx 0.8 requires the `json` feature. Without it the `Encode`/`Decode`/`Type` impls don't compile. Add `"json"` to the feature list. Implementation blocker if skipped.

**Crate-level dep additions:**

- `crates/eidetic-db/Cargo.toml` — add `serde_json = { workspace = true }`. The `NewAsset` and `AssetDetail` structs gain a `serde_json::Value`-typed field for the JSONB column, so the db crate needs the dep directly. Confirmed not currently present.
- `crates/eidetic-ingest/Cargo.toml` — add `serde_json = { workspace = true }`. `extract_exif` builds `serde_json::Value` objects. Confirmed not currently present.
- `crates/eidetic-server/Cargo.toml` — add `serde_json = { workspace = true }`. `DetailView` carries `exif_raw` (loaded but not rendered in v1, per §"Detail-page section"), so the server crate references the type. Confirmed not currently present.
- `crates/eidetic-cli/Cargo.toml` — already declares `serde_json = { workspace = true }` at line 20. No change.

**Unchanged deps:**

- `kamadak-exif` — present at workspace level (`Cargo.toml:79`), already a dep of `eidetic-ingest`. Used by every EXIF call today; verified to do what we need across HEIC/JPEG/DNG.
- `serde_json` — workspace declaration exists at `Cargo.toml:28`. Only the crate-level adds above are needed.

System deps: unchanged. No libheif change, no new C library.

---

## CI changes

Nothing. No new system library, no new Linux package. `cargo deny check` already accepts the kamadak-exif + serde_json + sqlx tree.

---

## README updates

Minimal. The README doesn't currently document the schema columns; adding one wouldn't be useful. The `eidetic backfill-exif` command should appear in whatever subcommand list the README maintains. Quick grep + add.

`AGENTS.md` "Workspace map" row for `eidetic-ingest` mentions "thumbnail generation"; the EXIF role is implicit. Adding a parenthetical "+ comprehensive EXIF extraction" keeps the row honest. Same level of detail as the HEIC/DNG line items.

---

## Testing approach

Three categories.

### 1. `meta::extract_exif` returns the new fields

Extend the existing in-file test at `crates/eidetic-ingest/src/meta.rs::tests`. Two new tests:

```rust
#[test]
fn extract_exif_reads_lens_and_camera_settings_from_minimal_jpeg() {
    // Hand-crafted JPEG carrying LensMake, LensModel, FocalLength, FNumber,
    // ExposureTime, PhotographicSensitivity, Orientation, GPSAltitude,
    // GPSAltitudeRef, GPSImgDirection.
    let tmp = tempfile::tempdir().unwrap();
    let path = write_bytes(tmp.path(), "rich.jpg", JPEG_WITH_FULL_SETTINGS);
    let data = extract_exif(&path);
    assert_eq!(data.lens_make.as_deref(), Some("Apple"));
    assert_eq!(data.lens_model.as_deref(), Some("iPhone 15 back dual wide camera 5.96mm f/1.6"));
    assert!((data.focal_length.unwrap() - 5.96).abs() < 0.01);
    assert!((data.aperture.unwrap() - 1.6).abs() < 0.01);
    assert_eq!(data.shutter.as_deref(), Some("1/25"));
    assert_eq!(data.iso, Some(400));
    assert_eq!(data.orientation, Some(1));
    assert!((data.altitude.unwrap() - 2032.76).abs() < 0.1);
    // exif_raw is some non-trivial object with the same fields:
    let raw = data.raw.as_ref().unwrap();
    assert_eq!(raw["LensMake"], serde_json::json!("\"Apple\""));
    assert!(raw.as_object().unwrap().len() >= 10);
}

#[test]
fn extract_exif_returns_none_for_unset_settings() {
    // Reuse JPEG_WITH_MAKE fixture (only Make tag).
    let tmp = tempfile::tempdir().unwrap();
    let path = write_bytes(tmp.path(), "sparse.jpg", JPEG_WITH_MAKE);
    let data = extract_exif(&path);
    assert!(data.lens_make.is_none());
    assert!(data.focal_length.is_none());
    assert!(data.iso.is_none());
    // raw is still Some — the Make tag itself was parseable:
    assert!(data.raw.is_some());
    assert!(data.raw.as_ref().unwrap()["Make"] != serde_json::Value::Null);
}
```

`JPEG_WITH_FULL_SETTINGS` is a hand-crafted byte literal, same pattern as the existing `JPEG_WITH_GPS` (`crates/eidetic-ingest/src/meta.rs:187-218`). Construction tooling: same as the existing fixture — write IFD entries by hand. Plan's Task N spells out the literal bytes.

### 2. `backfill-exif` subcommand integration test

Lives in `crates/eidetic-cli/tests/` (new file: `backfill_exif.rs`) **or** as a unit test inside the CLI's `Command::BackfillExif` handler refactored out into a testable function. Pick the second — wraps `pool` and `image-fixtures-dir` as args, no test container ceremony needed.

Use `testcontainers`-Postgres (existing pattern in `eidetic-db/tests/`). Setup: insert two asset rows with `storage_path` pointing at two real on-disk fixtures (one JPEG with EXIF, one binary blob without). Run the backfill function. Assert:

- The EXIF-carrying row got `exif_raw` populated as a JSONB object with `≥1` entry, and its typed columns matched.
- The blob-only row got `exif_raw = '{}'::jsonb` and every typed column is NULL.
- A second invocation is a no-op (`WHERE exif_raw IS NULL` matches zero rows).

### 3. Detail page renders the new section

In `crates/eidetic-server/src/views.rs::tests`, extend the existing `detail_page_*` tests. Add at least:

- `detail_page_renders_camera_settings_block` — a `DetailView` with `lens_model = Some("...")`, `focal_length = Some(5.96)`, `iso = Some(400)` produces HTML containing the literal substrings `"Lens"`, `"Focal length"`, `"5.96"`, `"ISO"`, `"400"`.
- `detail_page_omits_camera_settings_when_all_none` — same view with the new fields all `None` produces HTML that does NOT contain the literal substring `"Focal length"`. Covers the existence of the `@if` block-level guard.

### What we don't unit-test

- **Backfill against the user's real 779 files.** Manual smoke test below.
- **Migration speed on a populated table.** Manual step in plan Task 1 (seeding 779 dummy rows + timing the migration with `\timing`).
- **Existing tests that already cover EXIF reading** (e.g. `extract_exif_reads_gps_coords`) — those keep passing without modification.

### Manual smoke test (post-merge)

1. `cargo run -- backfill-exif --dry-run` → prints "would update 779 assets". No DB writes.
2. `cargo run -- backfill-exif` → prints per-asset progress, ends with totals.
3. `psql -c "SELECT COUNT(*) FROM assets WHERE exif_raw IS NOT NULL"` → 779.
4. `psql -c "SELECT lens_model, focal_length, iso FROM assets WHERE lens_model IS NOT NULL LIMIT 5"` → sane output.
5. `cargo run -- serve` → open `/assets/<some_id>` → "Camera settings" block visible with lens/focal/aperture/shutter/ISO populated for any iPhone-shot asset.
6. `cargo run -- backfill-exif` again → "0 pending; nothing to do." (Idempotency.)

---

## Risks

### Schema fragility

Adding eleven columns at once is a bigger migration than Eidetic has done before (existing migrations add 1–4 columns each). Mitigation: V4 explicitly probes the worst case; if it shows any user-visible delay on the 779-row table, split into smaller migrations. The columns are independent (no FK relationships, no constraints between them), so splitting is mechanical.

### `kamadak-exif` API drift

`field.tag.to_string()` is `Display` — not a documented stable interface. If kamadak 0.7 renames `LensMake` to something else, the JSONB keys for the affected tags change. Mitigation: the typed columns use the `Tag::X` enum variants, which are checked at compile time — they'd fail the build, not the runtime, if kamadak renamed them. The JSONB keys are best-effort labels; consumers should treat the JSONB column as opaque human-facing data unless they want to lock in a specific kamadak version.

### JSONB blob growth

If a future format pumps in 500 EXIF tags instead of 50, blob size grows. The row-level cost is bounded by Postgres's per-row toast limit (~ 2 KB inline, more in toast). 779 rows of 5 KB JSONB is ~4 MB; not a concern. If a single row ever exceeds 10 KB of EXIF, log it during backfill so we know.

### Backfill long-runs

If the library grows to 100K assets, 200 ms × 100,000 = ~5.5 hours of single-threaded backfill. Single-threaded is fine for now (779 files, minutes); when the user crosses 10K assets, a parallel pool with `tokio::task::JoinSet` is one-liner. Out of scope for v1; flagged here so a future contributor sees the limit.

### MakerNote storage cost

The probe shows MakerNote field display strings are ~3 KB of raw hex. If we don't filter it out of the JSONB blob (see §"What we skip"), every row carries 3 KB of unparseable hex. Filter is one `if field.tag != Tag::MakerNote { … }` line in the JSONB walk. Documented in §"The JSONB tail" above.

---

## Honest scope

### In v1 (this PR)

- One migration adding eleven nullable columns to `assets`.
- `extract_exif` extended to populate ten typed fields + a JSONB blob; JSONB excludes IFD-1 dupes and MakerNote.
- `NewAsset` and `AssetDetail` mirror the new shape; insert/select queries expand.
- New `Command::BackfillExif` (with `--dry-run`) running an UPDATE per pending row.
- Detail page renders a "Lens / Focal length / Aperture / Shutter / ISO" block when at least one is present; existing "Camera" + "GPS" rows stay.
- GPS row gets " (NNN m)" appended when `altitude` is present.
- Two new `extract_exif` unit tests; one new server-side `detail_page` test; one backfill integration test.

### Deferred (call out explicitly)

- **Apple MakerNote parsing** (ProRAW flag, depth-map refs, scene scores).
- **Reverse geocoding** (lat/lon → place names). Next planned PR.
- **Video metadata.** 115 video rows untouched.
- **EXIF orientation rotation fix** for JPEGs. Pending from HEIC PR.
- **Display-formatting polish** (pretty-printed values, locale formatting).
- **JSONB indexes** for `exif_raw->>'LensModel'`-style queries.
- **Parallel backfill** for libraries beyond ~10K assets.

---

## Implementation order (hint for plan author)

1. **Verification gate** (Plan Task 1). V1, V2, V3, V5 already PASS from the 2026-05-24 scratch probe at `/tmp/exif-probe/` — confirmations recorded inline above. V4 re-runs: spin up a fresh local Postgres, seed 779 dummy `assets` rows, run `migrations/005_comprehensive_exif.sql` with `\timing`, record duration. Halt and update §Schema if the migration is user-visibly slow.
2. **Migration + sqlx feature** (atomic commit). `migrations/005_comprehensive_exif.sql` with the ALTER TABLE. Also enable the `"json"` feature on the workspace `sqlx` declaration in root `Cargo.toml` (required for `serde_json::Value` ↔ `JSONB` binding; without it the later commits don't compile). Pre-commit clippy still green (the feature flip alone touches no Rust).
3. **`extract_exif` extension** (atomic commit). Extend `ExifData` with the ten typed fields + `raw: Option<serde_json::Value>`. Add per-field readers (`read_rational_real`, `read_short_int`, `read_signed_altitude`, etc — see §"The typed columns" mapping). Add the JSONB-walker. Add the two unit tests with new hand-crafted JPEG byte literals. `cargo clippy --workspace --all-targets -- -D warnings` green.
4. **`NewAsset` + insert path** (atomic commit). Extend `NewAsset` in `eidetic-db/src/assets.rs`; expand `insert_asset`'s INSERT statement and parameter bindings. Extend `import_file` in `eidetic-ingest/src/import.rs` to map the extra `ExifData` fields onto `NewAsset`. Clippy green.
5. **`AssetDetail` + fetch path** (atomic commit). Extend `AssetDetail`, the inner `Row` struct, and `fetch_by_id`'s SELECT list. Extend `DetailView` in `eidetic-server`. Server-side handler maps the new columns through. Clippy green.
6. **Detail page rendering** (atomic commit). Add the "Camera settings" block to `detail_page` per §"Detail-page section". Add the new view tests. Clippy + new tests pass.
7. **Backfill subcommand** (atomic commit). Add `PgAssetsRepo::fetch_pending_exif_backfill` and `PgAssetsRepo::update_exif_columns` in `crates/eidetic-db/src/assets.rs` (matches the existing `fetch_unembedded` / `fetch_unthumbnailed` repo pattern). New `Command::BackfillExif { dry_run: bool }` variant in `eidetic-cli/src/main.rs`. Handler function in `eidetic-cli/src/main.rs` (or factored to `eidetic-cli/src/backfill.rs` if it grows past ~50 lines) calls the two repo methods. Integration test against testcontainers Postgres. Clippy green; new test passes.
8. **AGENTS.md + README** (atomic commit). Update workspace-map row for `eidetic-ingest` to note "+ comprehensive EXIF". Add `backfill-exif` to whichever README subcommand list exists. Update README's "supported metadata" docs if any. (None at present per quick grep; spec author confirms.)
9. **Open PR on `feat/comprehensive-exif` worktree.** On Abhishek's Mac, run `eidetic backfill-exif --dry-run` (sanity), then `eidetic backfill-exif`, then `eidetic serve` and click into a few iPhone assets to confirm the new "Camera settings" block renders. Record findings in PR description.

Each step ends with `cargo clippy --workspace --all-targets -- -D warnings` green. The pre-commit hook enforces this. Commit messages follow Conventional Commits (`feat`, `chore`, `test`, `docs`); no `Co-Authored-By`.

---

## Decisions resolved (formerly open questions)

1. **Re-import vs backfill subcommand.** Backfill subcommand. Re-import would redo hash + thumbnail + embed (~tens of minutes of GPU + I/O) for files whose EXIF is sitting right there on disk and parses in milliseconds. The asymmetry is too lopsided.

2. **MakerNote handling.** Skip from JSONB (filter on `Tag::MakerNote`), defer Apple-specific parsing to a future PR. V3-verified opaque to kamadak; no quick win.

3. **JSONB shape.** Flat object keyed by `field.tag.to_string()`, value = `field.display_value().with_unit(&exif).to_string()`. IFD 1 (thumbnail-IFD) skipped. MakerNote skipped. Tier-1 + tier-2 fields kept in the blob in addition to their typed columns (long tail is the *full* tag set, not the "untyped remainder").

4. **Typed column count.** Ten. Each maps 1:1 to a tag the detail page renders (or wants to render — `orientation` and `gps_direction` are stored-but-not-rendered in v1 for the planned rotation-fix and bearing-display follow-ups).

5. **Empty-EXIF rows.** Get `exif_raw = '{}'::jsonb` after backfill, not `NULL`. Distinguishes "backfill ran, nothing to extract" from "backfill hasn't touched this row yet."

6. **Detail-page layout.** New "Camera settings" block sits between the existing "Camera" row and the existing "GPS" row. Doesn't refactor existing markup. (§Scope, §Architecture overview, and this decision were aligned to all read "between Camera and GPS" — earlier draft had inconsistent placement language.)

7. **Backfill selection filter.** Selection query restricts to `mime_type LIKE 'image/%'`, matching `fetch_unembedded` / `fetch_unthumbnailed` in `PgAssetsRepo`. Video rows (`mime_type LIKE 'video/%'`) are skipped entirely — their EXIF stays NULL by design until a video-metadata PR ships.

8. **Backfill SQL lives in the repo, not the CLI.** Two new methods on `PgAssetsRepo` (`fetch_pending_exif_backfill`, `update_exif_columns`). CLI calls them. Matches every other asset query in the codebase.

9. **GPS-derived column types.** `altitude` and `gps_direction` are `FLOAT8` (= `f64`), matching the existing `latitude` / `longitude` columns from `migrations/003_exif.sql`. Earlier draft used `REAL` (= `f32`) which contradicted the column type given in the row's "Conversion" cell.

---

## Spec self-review

**Placeholder scan.** No TBDs, no "implement later." Every column has a source tag, every code-touching step has the literal code or the literal column-name list. The one conditional is V4 — Postgres docs say it's fine; plan Task 1 reconfirms before any code lands.

**Internal consistency.**
- "Eleven new columns" (ten typed + `exif_raw`) matches the SQL in §Schema, the field list on `ExifData`/`NewAsset`/`AssetDetail`, and the rendered fields in §"Detail-page section."
- "Backfill subcommand" appears in §Goals, §Backfill subcommand, §Implementation order, and §Testing approach (the integration test).
- "MakerNote skipped" appears in §"What we skip", §Honest scope, §Decisions resolved.
- "Empty-EXIF rows get `{}`" appears in §"Failure-mode semantics", §"Backfill subcommand", and §Decisions resolved.

**Scope.** Single PR. Migration (~15 SQL lines), `meta.rs` extension (~150 Rust lines including readers + tests), DB schema-side struct expansion (~30 lines across `assets.rs`), server-side detail-page render (~25 lines + tests), CLI subcommand + handler (~80 lines + integration test). Eight commits per §Implementation order. Larger than the DNG PR by maybe 50%; substantially smaller than the HEIC PR's CI + system-package surface.

**Ambiguity flagged.** V4 (migration speed) is the one open verification; mitigation documented. Whether the `shutter` text format should include the trailing `" s"` or not — decided no (the UI adds it on render); flagged here for future revisitation if the user objects.

**Spec coverage check.** Mapping the user's original ask to spec sections:

- "new column `assets.exif_raw JSONB`" → §Schema + §"The JSONB tail" ✓
- Typed columns for lens / focal / aperture / shutter / iso / orientation / altitude / gps_direction → §"The typed columns" table ✓
- "Camera settings" section on detail page → §"Detail-page section" ✓
- Backfill subcommand vs re-import argument → §"Backfill subcommand" + §"Decisions resolved" #1 ✓
- Verification gate (V1: kamadak exposes tag enums; V2: uniform across HEIC/JPEG/DNG; V3: MakerNote behaviour) → §"Verification gate" ✓
- Out-of-scope (MakerNote, reverse geocoding, video, formatting) → §Honest scope ✓
- Cite reference specs' verification-gate / failure-modes / honest-scope structure → mirrored throughout ✓
- "Pure-Rust, MIT/Apache-2.0 deps only" → §Dependencies (no new deps; kamadak-exif + serde_json already MIT) ✓
- Atomic Conventional Commits, develop on `feat/comprehensive-exif` worktree, no push to main → §Implementation order step 9 ✓
- 779 assets, ALTER TABLE safety → §Schema + §Risks ✓
