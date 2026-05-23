# DNG preview extraction + show-original fallback

**Status:** Design draft 2026-05-23. Verification-gated (see V1–V4 below). Two concerns bundled because they share a UX surface — DNG support adds a code path, the fallback covers what happens when any decoder still can't produce a thumbnail.

**Scope:**

1. **DNG support.** Apple ProRAW DNGs (and other DNG-flavoured RAWs) fail thumbnail + embed today because the `image` crate's TIFF decoder rejects DNG's color types (`Unknown(24)` for 16-bit Linear RAW). Real-data finding from the HEIC backfill: **16 of 420 reprocessed assets were `.dng`**, all failed for the same reason.
2. **Stop hiding assets that lack a thumbnail.** Today `fetch_recent` (`crates/eidetic-db/src/assets.rs:196-208`) has `WHERE mime_type LIKE 'image/%'` — videos are invisible from the grid, and so is any image whose thumbnail generation failed. After this design, every imported asset stays visible. Images without thumbnails render the original directly via `/assets/<id>/raw`. Videos without thumbnails render a placeholder tile (extension badge + filename) since browsers can't display video files via `<img>`.

**Working-state baseline:** This spec is written against `main` at commit `4c40c9f` (the HEIC merge). No prerequisite PRs.

---

## Verification gate

Six DNGs and a handful of HEICs from Abhishek's library are available locally as ground truth. The "verify-then-implement" pattern from the HEIC spec is reused.

| # | Claim | If false | Verified 2026-05-23 |
|---|---|---|---|
| **V1** | Apple ProRAW DNGs embed a JPEG preview (one or more) discoverable via TIFF sub-IFDs with `NewSubFileType = 1`. | Pivot to `rawloader`/`dng` crate for raw-bayer decode + on-the-fly RGB conversion. Much heavier path; spec rewrite. | **PASS — simpler than expected.** Apple ProRAW puts the full-resolution JPEG in the **PRIMARY IFD** (Compression=7), not a SubIFD. Raw Bayer is in SubIFDs (tag 330) we ignore. So sub-IFD navigation isn't needed. Verified on 4 DNGs at 12 MP (4032×3024) and 48 MP (8064×6048). |
| **V2** | The `kamadak-exif` crate (already a workspace dep) can walk DNG sub-IFDs and surface the `JPEGInterchangeFormat` / `JPEGInterchangeFormatLength` tags, OR the `tiff` crate (transitive via `image`) can do the same. | Hand-roll a tiny TIFF IFD walker (~100 lines). The DNG container is well-documented; the cost is modest. | **PASS** — kamadak-exif exposes `Compression`, `ImageWidth`, `ImageLength`, `StripOffsets`, `StripByteCounts` on the primary IFD. Note: `NewSubfileType` and `JPEGInterchangeFormat[Length]` are NOT in kamadak's tag enum, but **we don't need them** — strips work fine for Apple ProRAW. |
| **V3** | At least one of the embedded previews is large enough to make a useful 1024px medium thumbnail (i.e. ≥ 1024 on its longest edge). | Decode the largest available preview anyway; if it's tiny, the medium thumbnail just upscales it. Acceptable visual cost; users see something rather than nothing. | **PASS** — full-resolution previews: 4032×3024 (12 MP) or 8064×6048 (48 MP). Way over 1024px. |
| **V4** | `kamadak-exif`'s existing `extract_exif` works on DNG (gives us camera make/model/date/GPS the same way it does for JPEG/HEIC). | Read EXIF from the largest preview's JPEG (which always re-embeds EXIF). Equivalent metadata; one more parse pass. | **PASS** — Apple/iPhone 15 Pro Max/DateTimeOriginal/GPSLatitude/GPSLongitude/Software all read cleanly. Same code path as JPEG/HEIC. |

Plan Task 1 is these four checks against the user's actual DNG files. We have them locally — no fixture problem. After verification, the spec's main decisions either stand or shift per the "If false" column.

---

## Goals

- A DNG file (Apple ProRAW or otherwise) produces working small + medium thumbnails via the existing `generate_thumbnails` pipeline.
- A DNG file produces a SigLIP embedding via the existing `preprocess_image` pipeline.
- EXIF metadata (date, GPS, camera) survives the DNG path.
- Landing grid stops hiding assets without thumbnails — every imported asset is visible, with a clear visual indicator for the "no thumbnail" case.
- Asset detail page serves the original file when no thumbnail exists, so the user can locate, inspect, or open it from the browser.

## Non-goals

- Decoding raw Bayer data. The embedded JPEG preview is what we want; the raw mosaic isn't viewable without per-camera color profile + demosaic work, none of which improves search or browsing.
- Sony ARW / Canon CR3 / Nikon NEF / Fuji RAF. Different containers, different libraries. Defer until you actually have files in those formats.
- HDR / 16-bit pipeline. The preview is 8-bit sRGB JPEG; everything downstream stays sRGB.
- "Render the original in the grid". Bandwidth-prohibitive for big files (DNGs are 20–40 MB each, videos worse). Grid stays thumbnails-only; detail page handles originals.
- Live video thumbnails (ffmpeg single-frame). Separate task — videos are 115 of 2755 assets in the library and worth addressing, but not in this scope.

---

## Architecture overview

DNG = TIFF container with multiple IFDs (Image File Directories). Apple ProRAW specifically (verified on 4 of Abhishek's DNGs) lays out:

```
IFD 0   COMPRESSION = JPEG (7), 4032x3024 or 8064x6048, ~1-7 MB of JPEG bytes
        StripOffsets + StripByteCounts point to the JPEG payload
        EXIF + GPS sub-trees attached here
  └── SubIFDs (tag 330)  →  raw Bayer / LinearRaw — what `image` crate chokes on
                             We ignore these.
```

So we don't navigate SubIFDs at all. We read the primary IFD's `Compression`, confirm it's 7 (JPEG), slice `[StripOffsets .. StripOffsets+StripByteCounts]` out of the file, and feed it to `image::load_from_memory`.

(Some non-Apple DNGs flip the layout — primary IFD is raw, preview is in a SubIFD. We handle that case if/when it shows up by adding SubIFD walking; for the user's Apple-only library, the simpler primary-IFD code path is enough.)

```
   ingest::thumbnail::load_image_with_limits()         ml::siglip::preprocess_image()
                       │                                              │
                       │ branch on file extension:                    │
                       │   if .dng → decode_dng_preview()             │
                       │   else    → existing ImageReader path        │
                       └──────────────┬───────────────────────────────┘
                                      ▼
                  ┌─────────────────────────────────────────┐
                  │  decode_dng_preview(path) → DynamicImage│
                  │  1. open file, mmap or read header      │
                  │  2. walk TIFF IFDs (kamadak-exif or     │
                  │     tiff crate)                         │
                  │  3. find sub-IFD with largest JPEG      │
                  │     preview (NewSubFileType=1 +         │
                  │     Compression=7)                      │
                  │  4. read JPEG bytes from offset/length  │
                  │  5. image::load_from_memory(...).       │
                  │     apply same image::Limits.           │
                  └─────────────────────────────────────────┘
```

Branching by extension is a small ugly thing the HEIC spec carefully avoided. For DNG it's unavoidable because the `image` crate's TIFF decoder *does* fire on DNG files (they have the TIFF magic bytes) and then *fails* on the color type. The HEIC pattern (`register_decoding_hook`) doesn't help here — `image::hooks::register_decoding_hook` is keyed by **extension**, and the `tiff` extension is already claimed by the built-in TIFF decoder with no way to override or deregister it (`image-0.25.10/src/hooks.rs`: vacant inserts, occupied returns `false`). So we branch upstream.

**Where the helper lives.** `eidetic-ingest::thumbnail::load_image_with_limits` and `eidetic-ml::siglip::preprocess_image` both need to call it. `eidetic-ml` does NOT depend on `eidetic-ingest`, so the helper cannot live in ingest. Three options:

| Option | Cost | Verdict |
|---|---|---|
| Duplicate in both crates (HEIC pattern) | ~80 lines × 2 = 160 lines duplicated | Rejected — HEIC's `ensure_heic_registered` was 5 lines, DNG is bigger. |
| Put in `eidetic-core` as `eidetic_core::dng::decode_dng_preview` | adds `kamadak-exif` + `image` workspace deps to core | **Chosen.** Both deps are pure-Rust, lightweight. AGENTS.md's "lightweight foundation" rule was about avoiding heavy C-binding deps (libheif, ort); pure-Rust EXIF + image decoding are fine. |
| New `eidetic-decode` crate | one more workspace member for ~80 lines | Rejected — same mass concern as the HEIC spec's `eidetic-media` discussion. Not enough code to justify. |

`eidetic-core/Cargo.toml` gains `image` + `kamadak-exif` deps. The HEIC helpers stay where they are; this PR doesn't refactor them. (A future cleanup PR could move both into `eidetic-core::decoders`; out of scope here.)

**Library choice (verification-gated):** prefer `kamadak-exif` since it's already a workspace dep and exposes sub-IFD walking via its `In` enum + `get_field` API. The `tiff` crate is also a transitive dep but has a less convenient API. Plan Task 1 V2 confirms one of these works; if neither does cleanly, fall back to a hand-rolled ~100-line TIFF IFD walker (well-bounded; the on-disk format is documented).

---

## Show-original fallback

The grid currently hides two sets of assets: videos (filtered by `mime_type LIKE 'image/%'`) and any image whose thumbnail generation failed (those are now embed-skipped at `import.rs:83` and render broken if surfaced anyway). We want both visible. The fallback differs by media kind because browsers can't render video bytes via `<img>`.

Three coordinated changes:

### 1. `fetch_recent` widens its filter and projection

`crates/eidetic-db/src/assets.rs:196-208` currently:

```sql
SELECT id, hash, original_filename, imported_at
FROM assets
WHERE mime_type LIKE 'image/%'
ORDER BY imported_at DESC
LIMIT $1
```

becomes:

```sql
SELECT id, hash, original_filename, mime_type, thumbnails_generated, imported_at
FROM assets
WHERE mime_type IS NOT NULL
ORDER BY imported_at DESC
LIMIT $1
```

(The `mime_type IS NOT NULL` keeps the imported-but-unknown-format rows out, matching the import-time gate at `import.rs:26-31`.)

`RecentAsset` (`assets.rs:63-69`) gains two fields:

```rust
pub struct RecentAsset {
    pub id: AssetId,
    pub hash: eidetic_core::Sha256,
    pub original_filename: String,
    pub mime_type: String,           // NEW — always Some() after the filter above
    pub thumbnails_generated: bool,  // NEW
    pub imported_at: chrono::DateTime<chrono::Utc>,
}
```

The row tuple in the sqlx `query_as` call expands accordingly.

### 2. `GridTile` and the handler mapping gain the same fields

`crates/eidetic-server/src/views.rs:8-14`:

```rust
pub(crate) struct GridTile {
    pub(crate) id: AssetId,
    pub(crate) hash: Sha256,
    pub(crate) alt: String,
    pub(crate) score: Option<f32>,
    pub(crate) mime_type: String,           // NEW
    pub(crate) thumbnails_generated: bool,  // NEW
}
```

`crates/eidetic-server/src/handlers.rs:19-30` (the `fetch_recent → GridTile` mapping) populates the new fields directly from the `RecentAsset`.

Search results (`SearchHit`) follow the same expansion since they also build `GridTile`s — confirm via grep when implementing.

### 3. `asset_grid` renders three branches per tile (real Maud syntax)

`crates/eidetic-server/src/views.rs:83-99`, currently:

```rust
img src=(format!("/thumbs/m/{}", tile.hash)) loading="lazy" alt=(tile.alt);
```

becomes:

```rust
@if tile.thumbnails_generated {
    img src=(format!("/thumbs/m/{}", tile.hash)) loading="lazy" alt=(tile.alt);
} @else if is_browser_renderable(&tile.mime_type) {
    // Image format the browser can render natively (jpeg/png/webp/...).
    // Browser loads the original; acceptable because this set is small after
    // DNG support lands.
    img src=(format!("/assets/{}/raw", tile.id))
        loading="lazy"
        alt=(tile.alt);
} @else {
    // Video, RAW, HEIC-without-thumbnail, or any other non-browser-renderable
    // type. Show a placeholder; the detail page handles inspection.
    div class="tile-placeholder" {
        span class="ext-badge" { (ext_from_filename(&tile.alt)) }
        span class="filename" { (tile.alt) }
    }
}
```

`is_browser_renderable` is a private helper in `views.rs`:

```rust
/// MIME types that all major browsers (Chrome, Firefox, Safari) reliably
/// render via `<img>`. Deliberately conservative — HEIC works on Safari
/// only, DNG/ARW/CR3 don't work anywhere. For those, show a placeholder
/// instead of a broken-image icon.
fn is_browser_renderable(mime: &str) -> bool {
    matches!(
        mime,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        | "image/avif" | "image/bmp" | "image/svg+xml"
    )
}
```

The placeholder is the same uniform dimension as a thumbnail tile so the grid stays clean. CSS-only. `ext_from_filename` is a 3-line helper that returns the upper-cased extension from the filename (defaults to `"FILE"` if none).

Mockup:

```
+------------+   +------------+   +------------+   +------------+
|            |   |            |   |[full DNG   |   |     MP4    |
|  [thumb]   |   |  [thumb]   |   |  rendered  |   |            |
|            |   |            |   | by browser]|   |  beach_vid |
+------------+   +------------+   +------------+   +------------+
   IMG_3580         IMG_3703         IMG_1234         beach_vid.mov
```

(First two: normal. Third: image without thumbnail, browser renders the original. Fourth: video, placeholder.)

### 4. Detail page: replace the "Thumbnails pending" empty-state branch

`crates/eidetic-server/src/views.rs:101-117` currently shows either the thumbnail or an empty-state `<p class="empty">Thumbnails pending…</p>` and always shows a "Download original" link below. We replace the empty-state branch:

```rust
@if view.thumbnails_generated {
    img class="preview" src=(format!("/thumbs/m/{}", view.hash)) alt=(view.original_filename);
} @else if view.mime_type.as_deref().is_some_and(is_browser_renderable) {
    img class="preview" src=(format!("/assets/{}/raw", view.id)) alt=(view.original_filename);
} @else {
    p class="empty" {
        "No preview available — this asset is "
        (view.mime_type.as_deref().unwrap_or("unknown type"))
        ". Use the download link below."
    }
}
```

`is_browser_renderable` is shared with the grid (same helper). The "Download original" link stays where it is. No size threshold needed: the detail page is one asset at a time, and the user clicked through specifically to see it.

---

## Failure-mode semantics

Same three buckets as HEIC:

1. **DNG with no embedded preview.** Should be zero for camera-produced DNGs (Apple, Adobe, all bake one in). Synthetic / pathological DNGs might not. Handled the same as any decode failure: `Err(Error::ImageDecode { path, source })`, asset row stays with `thumbnails_generated = false`, the show-original fallback takes over in the UI.
2. **DNG embedded preview is itself corrupt.** Same path — JPEG decode fails, same error variant, same UI fallback.
3. **DNG file is a malformed TIFF.** TIFF parser errors → `Err(Error::ImageDecode)`, same fallback.

No silent failures, no `unwrap`, no asset rows lost.

---

## Schema

No DB schema change. `thumbnails_generated` (bool) and `mime_type` (text) are already on the row. The grid + detail-page changes read both columns; we just expand `fetch_recent`'s SELECT list to project them.

(Side note: a `thumbnail_source = 'preview' | 'native' | NULL` column would be nice future telemetry but it's premature now. Skip.)

---

## Dependencies

No new workspace-level deps. Reuses:

- `kamadak-exif` — already a workspace dep. Walks TIFF IFDs (V2-verified).
- `image` — already a workspace dep. Decodes the extracted JPEG bytes.

**Crate-level dep additions:**

- `crates/eidetic-core/Cargo.toml` gains `image = { workspace = true }` and `kamadak-exif = { workspace = true }`. Both are pure-Rust; the "lightweight foundation" rule in AGENTS.md is about avoiding heavy C-binding deps (libheif, ort), not about all deps.

(Fallback if V2 fails: hand-rolled TIFF IFD parser in `eidetic-core::dng`; ~100 lines, no new dep, same dep additions.)

System deps unchanged. `libheif` stays for HEIC; nothing new for DNG.

---

## CI changes

Nothing. `kamadak-exif` is already linked everywhere; no new system library; no new Linux package needed.

---

## README updates

Minimal. Add `.dng` to the "supported formats" list (which the README doesn't currently call out — adding a one-liner). No new install steps.

---

## Testing approach

Three test fixtures needed:

1. **A tiny DNG with an embedded JPEG preview.** Hardest to generate synthetically — DNG encoders are scarce and complex. Three options:
   - **(a) Commit a small DNG fixture.** Generate once from a 4×4-pixel photo (e.g., shoot a photo of a flat surface with iPhone ProRAW, downsize via ExifTool while preserving the DNG container + preview structure). License-clean by Abhishek's own authorship, MIT/Apache-2.0 like the rest of the repo. ~5–20 KB committed.
   - **(b) Build one in code.** Write minimal TIFF bytes (magic + IFD with two entries) + a 4-byte embedded JPEG. Verbose, brittle, easier to mess up.
   - **(c) Skip the unit test and rely on the manual smoke test below.** Bad — leaves us without regression coverage.

   **Choosing (a).** Same pattern the HEIC spec considered for Path D. Commit `tests/fixtures/tiny.dng`. Per-test `std::fs::write` into a tempdir if the test needs a writable path.

2. **`thumbnail.rs::generate_writes_thumbnails_from_dng_source`** — run full pipeline through DNG, assert both thumbnails exist.

3. **`siglip.rs::preprocess_image_accepts_dng_source`** — assert preprocess returns the expected tensor length.

4. **`eidetic-core::dng::extract_largest_jpeg_preview`** — unit test against the committed fixture. Asserts the function returns non-empty JPEG bytes with magic `FF D8 FF`.

Plus a regression check the existing tests already cover: corrupt-DNG (random bytes with `.dng` ext) should produce `Err(Error::ImageDecode)`.

### Existing tests that need updating

- `crates/eidetic-server/src/views.rs:179-205` — `asset_grid_empty_renders_empty_message` and `asset_grid_renders_one_tile_per_input`. The `GridTile` literals need the two new fields populated. Add at least one assertion for the placeholder-tile path and one for the image-without-thumbnail path.
- `crates/eidetic-db/tests/assets.rs` (if it exists) — any `fetch_recent_*` tests need to project the new columns and either tolerate or assert on the broader filter.
- `crates/eidetic-server/src/handlers.rs` — wherever `RecentAsset → GridTile` mapping happens (around line 25-30 per the review), wire the new fields. Existing handler tests that build mock `RecentAsset`s need the new fields too.
- `crates/eidetic-server/src/views.rs::detail_page_*` tests at the bottom of the file — the new image-without-thumbnail branch needs coverage.

### Manual smoke test (post-merge)

1. `eidetic import` Abhishek's existing 16 DNGs (which now sit in some folder after the library reset).
2. `eidetic thumbnail` → expect 16/16 succeed (all have embedded previews).
3. `eidetic serve` → grid shows the DNGs with real thumbnails.
4. Pick one DNG without a thumbnail (rare; maybe one we deliberately corrupt) → detail page shows the original.
5. (Bonus, separate from DNG) confirm a video (`.mov`/`.mp4`) shows a placeholder tile and the detail page serves the original.

---

## Backfill plan

After the implementation lands and the user re-imports their library (per the reset earlier today), `eidetic thumbnail && eidetic embed` should process all DNGs cleanly along with everything else. No separate backfill step — the same commands cover all formats.

---

## Risks

### TIFF parser surface area

`kamadak-exif` is mature and handles ~all camera TIFF variants. We've used it for EXIF reads since the start. Walking IFDs for tag values is a more invasive use of the API but still within its design. If V2 reveals the API doesn't expose sub-IFD navigation cleanly, the fallback (hand-roll a tiny TIFF reader) is well-bounded.

### Preview quality

Some DNG previews are downscaled aggressively (e.g., 512px). Our medium thumbnail wants 1024px. If V3 fails — i.e., the largest embedded preview is smaller than 1024 — the medium thumbnail just upscales it. Visual quality drops but doesn't break anything. Acceptable for personal use.

### "Show original" bandwidth

The grid serves originals only for the small failure set (images without a generated thumbnail). Worst case: a handful of 40 MB images render in a single grid view — acceptable for personal use, scales to ~10s of tiles before browser memory becomes a concern. If the failure set ever grows large, revisit with a size-based fallback.

Detail page is one asset at a time; the user clicked through deliberately. No threshold there either.

### Other RAW formats showing up later

If the user adds Sony ARW / Canon CR3 / Nikon NEF files, they'll fail thumbnailing today. The show-original fallback covers them visually — they'd render as placeholder tiles in the grid, original served on detail page. Functionally fine, not searchable until we add proper decoders. Honest scope.

---

## Honest scope

### In v1 (this PR)

- DNG preview extraction (Apple ProRAW + standard DNG).
- EXIF read from DNGs (via existing kamadak-exif path; confirmed by V4).
- Grid stops filtering out non-thumbnailed assets.
- Placeholder tile rendering for assets without thumbnails.
- Detail page serves original when no thumbnail (with size-threshold safeguard).
- Two new unit tests (DNG thumbnail + DNG preprocess), one committed DNG fixture.

### Deferred

- **Video thumbnails** (first-frame via ffmpeg). 115 assets in the library. Worth doing soon, but the show-original fallback covers them in this PR — they appear in the grid as placeholders.
- **Sony / Canon / Nikon proprietary RAW.** No camera-side data yet; defer until needed.
- **`thumbnail_source` telemetry column.** Premature.
- **DNG raw-Bayer decode.** Not useful for search.
- **JPEG EXIF orientation fix.** Separate task, still pending from the HEIC PR.
- **Reverse geocoding, full EXIF JSONB.** Next planned PRs in the order discussed.

---

## Implementation order (hint for plan author)

1. **Verification gate** (Plan Task 1). Walk an actual user-supplied DNG with `kamadak-exif` and confirm V1–V4. Record findings as spec annotations.
2. Add `image` + `kamadak-exif` to `crates/eidetic-core/Cargo.toml`. Add `pub mod dng;` to `eidetic-core/src/lib.rs`. Implement `eidetic_core::dng::decode_dng_preview(path: &Path) -> Result<DynamicImage>` (and `extract_largest_jpeg_preview(path) -> Result<Vec<u8>>` as the seam — easier to unit-test than the full decode).
3. Branch on file extension in `crates/eidetic-ingest/src/thumbnail.rs::load_image_with_limits` (line 108-128) and `crates/eidetic-ml/src/siglip.rs::preprocess_image` (line 267-309). Each gains a 3-line branch: `if is_dng_path(path) { return eidetic_core::dng::decode_dng_preview(path); }` before the existing `image::ImageReader` path.
4. **Deferred to follow-up PR.** Commit the DNG fixture at `crates/eidetic-core/tests/fixtures/tiny.dng` (requires Abhishek to capture + downsize a ProRAW photo first). Add unit tests in `eidetic-core::dng` (test the seam), in `eidetic-ingest/src/thumbnail.rs#tests` (`generate_writes_thumbnails_from_dng_source`), and in `eidetic-ml/src/siglip.rs#tests` (`preprocess_image_accepts_dng_source`). Until then, regression coverage comes from the manual smoke test (re-import + thumbnail + embed against the Dalhousie DNGs).
5. Server-side changes (atomic commit):
   - `crates/eidetic-db/src/assets.rs`: expand `fetch_recent`'s SELECT list, expand `RecentAsset` struct.
   - `crates/eidetic-server/src/views.rs`: expand `GridTile`, three-branch grid rendering, detail-page empty-state replacement.
   - `crates/eidetic-server/src/handlers.rs`: wire the new fields through `RecentAsset → GridTile` mapping (and `SearchHit → GridTile` if applicable).
   - Update the affected tests in `views.rs` and `handlers.rs`.
6. CSS for `.tile-placeholder` (in whichever stylesheet the existing grid uses; grep for `class="grid"`).
7. README + AGENTS.md updates: add DNG to supported formats; note `eidetic-core` now decodes DNG previews.
8. Re-import on user's Mac, smoke-test (grid shows DNGs + videos), open PR.

Each step ends with `cargo clippy --workspace --all-targets -- -D warnings` green. Step 5 is the largest atomic commit by far; everything else is mechanical.

---

## Decisions resolved (formerly open questions)

1. **DNG fixture provenance.** Will ask Abhishek to capture one ProRAW photo of a flat surface on the iPhone; resize to ~5 KB (preserving DNG container + embedded preview structure) via ExifTool; commit as `crates/eidetic-core/tests/fixtures/tiny.dng` under the workspace MIT/Apache-2.0 license. License-clean by his authorship of a wall photo.

2. **Inline-vs-download threshold on the detail page.** Removed. Detail page is one asset; the user clicked through deliberately. No threshold.

3. **Grid behaviour.** Show everything, always. Image-without-thumbnail renders the original via `<img src="/raw">`. Video-without-thumbnail renders a placeholder. No toggle, no `mime_type LIKE 'image/%'` filter.

4. **Re-import timing.** Not blocking. Re-import happens after this PR merges (or in parallel — same backfill commands work). User will re-import whatever they want, whenever they want.

---

## Spec self-review

**Placeholder scan.** No TBDs. One conditional: V2 mitigation is "hand-roll a TIFF parser." Cost is documented (~100 lines), still small enough to fit in this PR if needed.

**Internal consistency.**
- No new crate matches the "Dependencies" section.
- DNG branching by extension matches the "Architecture overview" diagram.
- Show-original fallback in detail page matches the "Failure-mode semantics" section (UI behavior on `thumbnails_generated = false`).
- Test fixtures committed locally — matches the HEIC spec's Path D pattern.

**Scope.** Single PR. DNG decode helper (~60 lines), one branch each in thumbnail + siglip, server view changes (~50 lines), CSS (~30 lines), two tests, one fixture. Plausible in one focused PR. Larger than the HEIC PR by maybe 30%.

**Ambiguity flagged.** Open questions section above. None of them block starting Plan Task 1.
