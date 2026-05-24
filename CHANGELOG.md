# Changelog

All notable user-facing changes between Eidetic releases.

## v0.2.0 — 2026-05-24

The "self-hosted photo intelligence" release. v0.1 imported and embedded JPEGs/PNGs in a CLI; v0.2 adds a browseable web UI, full iPhone format support, and offline place tagging — all still single-user, localhost-only, and zero external API calls.

### Added

- **HTTP server (`eidetic serve`)** — Axum app on `127.0.0.1:8080` with a thumbnail grid (`/`), full-text + semantic search (`/search`), per-asset detail page (`/assets/<id>`), and raw-file download. Localhost-bound, no auth — single-user by design.
- **HEIC / HEIF decoding** via libheif. iPhone HEICs now thumbnail, embed, and render natively. Requires `brew install libheif` (macOS) or `apt install libheif-dev libheif-plugin-x265` (Ubuntu).
- **DNG / Apple ProRAW decoding** via embedded JPEG preview extraction (no raw-bayer decode needed). Apple ProRAW DNGs go through the same thumbnail + embedding pipeline as JPEGs.
- **Comprehensive EXIF extraction** — lens make/model, focal length (real + 35mm equivalent), aperture, shutter, ISO, orientation, altitude, GPS direction. Plus a `exif_raw` JSONB column with every parseable tag for the long tail.
- **Offline reverse geocoding** — GPS coordinates resolve to `country_code`, `country_name`, `admin1` (state/province), and `place` (nearest city/town) via a bundled GeoNames `cities500` dataset (auto-downloads on first import; CC-BY 4.0). No network calls per asset; coordinates never leave the machine.
- **Automatic thumbnail generation** at import time — 256px and 1024px JPEGs in a content-addressable layout under `.thumbs/`.
- **Show every imported asset in the grid** — assets without thumbnails render as placeholder tiles (extension badge + filename) so videos and unrenderable formats stay visible. Browser-renderable images under 25 MiB render inline via `<img src="/raw">`.
- **Camera settings on the detail page** — lens, aperture, shutter, ISO, focal length, altitude block under each photo.
- **Place line on the detail page** — `Dalhousie, Himachal Pradesh, India (12 m from city centroid)` for GPS-having photos.

### Changed

- **Hash before staging** — duplicate imports short-circuit without copying file bytes into the staging area. Large-library re-imports are noticeably faster.
- **Inlined the `Embedder` trait** — `SiglipEmbedder` is now used concretely; the single-implementation trait + `Mutex<Session>` ceremony was over-engineered.
- **Deleted the `AssetIndex` trait** — same reasoning. `PgAssetsRepo` is used directly.
- **CoreML guidance clearer** — setting `EIDETIC_ACCELERATOR=coreml` now logs a warning explaining that CoreML measures slower than CPU for SigLIP 2 on Apple Silicon (265 ms/img vs 148 ms/img). Wiring kept in case future ort/coremltools releases fix the underlying compile bugs.

### Verified end-to-end on a real 779-asset library

- Import: 779 imported, 8 duplicates skipped, 1 non-media skipped, 0 failures.
- 664 image rows: 100% thumbnailed inline, 100% embedded, **553 / 553 GPS rows reverse-geocoded** (290 to "Dalhousie / Himachal Pradesh / India", 205 to "Dharamsala", 50 to "Chamba", plus a few travel-day entries).
- 115 videos: imported, rendered as placeholder tiles in the grid (video frame thumbnails are a future feature).

### Internals

- New crates: `eidetic-server` (Axum app).
- New modules: `eidetic-core::dng`, `eidetic-core::geocoder`.
- Migrations 002–006 add embeddings, EXIF columns, thumbnails flag, comprehensive EXIF + JSONB, and place columns.
- CI installs `libheif-dev` + `libheif-plugin-x265` + `libheif-plugin-libde265` on Ubuntu.

### Deferred (planned for later releases)

- Video frame thumbnails (needs ffmpeg).
- JPEG EXIF orientation handling (HEIC already rotates via libheif's `irot`; JPEG-with-portrait-orientation still renders sideways).
- Sony ARW / Canon CR3 / Nikon NEF decoders.
- Browse-by-place UI (the columns are populated; the filter UI isn't built yet).
- Map view in the web UI.

## v0.1.0 — 2026-05-10

Initial release. CLI-only: `eidetic import`, `embed`, `search`, `stats`, `hash`. SigLIP 2 embeddings via ONNX Runtime. PostgreSQL + VectorChord for vector search.
