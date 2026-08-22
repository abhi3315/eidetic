# Changelog

All notable user-facing changes between Eidetic releases.

## v0.5.0 — 2026-08-22

The "video intelligence" release — the four features scoped in [goals-v0.5.md](goals-v0.5.md), each validated against market research before a line was written. Highlights: videos now play in every browser, people are found *inside* videos, reels cut on shot boundaries, and search understands what was *said* — a capability no photo manager ships, Google and Apple included.

### Added

- **`eidetic transcode`** — browser-playback copies for videos whose codec/container browsers can't stream (HEVC iPhone footage, MKV). The policy is a codec×container compatibility matrix in one place (`eidetic-core::playback`); a safe codec in the wrong container remuxes near-instantly instead of re-encoding. Copies live under `.playback/`; originals are never touched; the web player picks `/raw`, the `/play` copy, or explains what to run. Range/206 on both.
- **Faces in videos** — `eidetic faces` scans the sampled frames of each video. Video faces pass a stricter quality gate (score ≥ 0.7, ≥ 48 px), collapse near-duplicates across frames to the sharpest instance, and may *join* existing photo-built people but never seed new person candidates or serve as matching exemplars — compression artifacts are the classic cluster poison. Person pages deep-link to the moment someone appears (`?t=` → media fragment), with a ▶ badge on video face crops.
- **Scene-aware sampling and reel cuts** — scene boundaries detected once per video (ffmpeg scene filter, persisted in `video_scenes` with a single-shot sentinel). Frame sampling takes one frame per shot (duration-scaled, up to 24) instead of five fixed offsets; `eidetic faces` shares the same timestamps; reel clips stay inside the shot containing the matched moment — no mid-shot slices, no splicing across cuts.
- **Speech search** (`--features speech`) — `eidetic transcribe` runs Whisper over video audio (silent tracks skipped by an energy gate) into an FTS5-indexed transcript store; search merges spoken-word hits into the ranking above visual matches, with the snippet and jump timestamp (`said: "…"`). Transcripts are recall fuel, never subtitles. Off by default: whisper.cpp needs cmake, and default builds stay free of native build deps.
- **CUDA verified on Blackwell** — ONNX Runtime 1.29 + cuDNN 9.25 via `ORT_DYLIB_PATH` drives the RTX 5070 Ti: ~22× less CPU time embedding, identical rankings. Recipe in the README; a cuda build without `ORT_DYLIB_PATH` now fails loudly instead of letting ort silently download a CPU-only runtime mid-command.

### Changed

- Logs go to stderr; stdout is pipeable data (`search --json | jq` works bare).
- ADR-0007's deferred model decision is settled with data: on COCO-1K, so400m beats base by only ~2 points Recall@1 (70.5 vs 68.6) at 3× the size and compute — `base` stays the default.

### Verified end-to-end on real media

- HEVC portrait video re-encoded and served with seeking; an MKV remuxed in under a second; plain MP4s keep serving originals. Found live: `infer` reports MKV as `video/webm` (same EBML magic), which is why the playback policy is a compatibility matrix, not two allowlists.
- The Serena pan video's face joined the existing Serena person (0.945 @0.5s); Sintel's two detected *animated* faces stayed unassigned without creating any person — the no-seed policy working on the first try.
- Sintel: 7 scene boundaries, frames on shot midpoints; a reel moment at 1.2s cut exactly [0.0s +2.4s] — the whole first shot, stopping at the 2.42s cut.
- A video muxed with the JFK sample: three clean Whisper segments; searching "ask not what your country can do for you" returns it first with the snippet; 7 silent videos never reached Whisper.

### Internals

- Migrations 005–008: `playback_path`, `faces.ts_secs`, `video_scenes`, `transcripts` + FTS5 + `transcript_runs`. New modules: `eidetic-core::playback`, `eidetic-ml::speech`. CI checks the speech feature builds.

## v0.4.0 — 2026-08-22

The "videos are real media now" release — and with it, every feature goals.md set out to build is in: ingestion, semantic search, face grouping, and reels. Videos stop being placeholder tiles: they thumbnail, embed, search by *moment*, play in the browser, and get cut into prompt-driven reels. The web viewer grows a People section.

### Added

- **Video pipeline** (ADR-0011) — shells out to system `ffmpeg`/`ffprobe`, found on `PATH` or via `EIDETIC_FFMPEG_PATH`/`EIDETIC_FFPROBE_PATH`. Strictly a *runtime* dependency: builds don't change, and without ffmpeg videos still import — one warning says what to install, and `eidetic thumbnail` + `eidetic embed` backfill later (both re-probe and store the metadata they find).
  - Import probes duration, codec and rotation-aware display dimensions; the container `creation_time` becomes `date_taken`; the QuickTime ISO 6709 GPS tag iPhones write geocodes offline exactly like photo EXIF.
  - A representative frame (10% in, ≥0.5s) goes through the photo thumbnail path, so videos get real tiles in the grid.
- **Semantic search over video moments** — `eidetic embed` samples up to 5 frames across the middle 80% of each video and stores each with its timestamp (`frame_embeddings`). Search ranks image and frame vectors in one pool and collapses to the best moment per asset: one matching second is enough to surface a whole video, and results carry that timestamp (`@7.0s` on default output, `ts` field, JSON). A video whose tail can't decode keeps the frames that did embed.
- **`eidetic reel "<prompt>"`** — the goals.md stretch goal. Search picks the moments; ffmpeg cuts them: ~4s clips around matched video frames (1.5s lead-in), 3s Ken Burns push-ins for photos (rendered from the upright medium thumbnail, so HEIC and rotated JPEGs come out right), all normalised to one H.264/30fps stream and joined losslessly. `--duration` (default 30s), `--size` (default 1920x1080), `--output`, `--dry-run` for the cut list. Weak matches are refused rather than padded in.
- **People in the web viewer** — `/persons` (cover face crop, name, face count), `/persons/{id}` (face-crop grid linking to each photo), and a People row on asset detail pages. Face crops are the canonical 112×112 aligned crops rebuilt from stored landmarks — no model loading — JPEG-cached under `.faces/`. Viewing only: naming and merging stay in the CLI.
- **Video playback in the browser** — detail pages render a `<video>` player, and `/assets/{id}/raw` now answers HTTP Range requests (206/416) so seeking works everywhere, Safari included. Video grid tiles get a ▶ badge.

### Changed

- `eidetic thumbnail` and `eidetic embed` treat videos as first-class pending work (previously image-only); `eidetic stats` counts them in `needs_embed`/`thumbnails_pending`.
- `VectorIndex::top_k` returns row indices instead of asset ids, so a matched frame keeps its timestamp through ranking.

### Verified on a real mixed library

- Real videos (Big Buck Bunny, jellyfish footage, Ken Burns pans incl. HEVC portrait) + 65 photos: "jellyfish floating underwater" → jellyfish.mp4 @7.0s, "cartoon rabbit in a meadow" → bunny @9.0s, "woman playing tennis" → the Serena pan @0.5s — each with a clean score gap, photos and videos ranking correctly against each other.
- A truncated mp4 imports, probes, embeds its 2 decodable frames and completes; an audio-only m4a is rejected at import.
- Reels: a 15s photo reel rendered frame-exact at 15.000s; the jellyfish reel cut exactly the matched moment and refused weak padding.
- Live server: persons grid, face crops, `<video>` seeking via Range (206 with exact byte counts), play badges, video thumbnails.

### Internals

- New: `eidetic-ingest::video` (probe/extract/sampling), `frame_embeddings` table (migration 004, with `duration_secs`/`video_codec`/`pixel_width`/`pixel_height` on assets), `eidetic-cli::reel`, `FacesRepo` person/face lookup methods, `/faces/{id}/crop` route, `Landmarks::from_template_order`.
- `/assets/{id}/raw` delegates to tower-http's `ServeFile` for correct range semantics.
- CI installs ffmpeg; a real-ffmpeg round-trip test synthesises a clip with lavfi and asserts probe + extraction + corrupt-file handling.

## v0.3.0 — 2026-08-21

The "faces and no more Docker" release. v0.2 could browse and search a library; v0.3 groups the people in it — and drops the database server entirely. Postgres + VectorChord is gone, replaced by a single SQLite file, so the whole system is now one binary plus one file, no container, no daemon.

### Added

- **Face grouping (`eidetic faces`)** — detects faces with YuNet, aligns each one onto the ArcFace 5-point template, embeds it with AuraFace (512-dim, Apache-2.0), then groups faces into people. New faces join an existing person when they sit close enough to one of that person's exemplars; leftovers are clustered with Chinese Whispers and proposed as new people once at least 3 agree. `--cluster-only` re-groups already-detected faces without rescanning. See ADR-0010.
- **`eidetic persons` and `eidetic name-person <id> <name>`** — list grouped people with face counts, give them names.
- **Durable corrections** — names, merges and splits are stored as constraints, so re-running clustering never discards them. User-assigned faces are frozen; the clusterer must never reassign them.
- **`EIDETIC_FACE_MODEL`** — `auraface` (default), `sface` (smaller/faster, 128-dim), or `buffalo_l` (InsightFace; better accuracy, but non-commercial weights you download yourself).
- **CUDA execution provider** behind a `cuda` build feature (`EIDETIC_ACCELERATOR=cuda`). Needs an ONNX Runtime ≥ 1.27 CUDA build via `ORT_DYLIB_PATH` on Blackwell (sm_120) GPUs — the bundled 1.23.x can't drive them (ADR-0006). An explicit accelerator request that can't be honored now fails loudly instead of silently running on CPU.
- **SigLIP 2 so400m variant** (`EIDETIC_MODEL=so400m`, 1152-dim) — the quality option, same variant Immich ships. Default stays `base` until `eidetic eval` measures otherwise (ADR-0007). Unknown `EIDETIC_MODEL` values are rejected instead of silently falling back.
- **Release CI** — build matrix producing binaries per platform, plus a guard that the `--no-default-features` build stays free of C system dependencies (ADR-0009).

### Changed

- **Postgres + VectorChord → embedded SQLite** (ADR-0005). No server process, no Docker, no connection string: the database is one file (`EIDETIC_DATABASE_PATH`, default `~/.cache/eidetic/eidetic.db`), created and migrated on first connect. Vector search is an exact in-process cosine scan — 100% recall, nothing to tune; usearch is the documented escalation past ~1M vectors. Integration tests went from container-per-test to ~1s total.
- **libheif is optional** behind a default-on `heic` feature (ADR-0008). `--no-default-features` builds with zero C system dependencies.

### Fixed

- **Dependency advisories cleared** — anyhow, crossbeam-epoch and spin bumped.

### Verified against LFW (Labeled Faces in the Wild)

- 65-image subset: 6 identities × 10 photos each plus 5 single-photo people, including George HW Bush as a deliberate near-match to George W Bush. Result: exactly six 100%-pure clusters (10/10 faces each after one incremental pass), all singletons correctly left unassigned, and a person's name survives `faces --cluster-only` re-runs.
- Embedding separation: same-person cosine similarity 0.67 mean vs 0.08 different-person; minimum same-person 0.34 stays above maximum different-person 0.29.
- This verification caught a real bug before release: the eye/mouth landmark order was mirrored going into alignment, which collapsed crops to ~0.24× scale and dragged same-person similarity down to 0.39. Geometry-only checks passed the whole time — an ignored LFW discrimination test (`EIDETIC_FACE_LFW_DIR`) now guards the metric that actually matters.

### Migration note

There is no automated Postgres → SQLite migration. Reset and reimport: point `EIDETIC_DATABASE_PATH` wherever you want the file (or accept the default), then rerun `eidetic import` and `eidetic embed`. The old `database_url` config is gone.

### Internals

- New crate modules: `eidetic-ml::face` (detect / align / analyzer), `eidetic-db::{faces, cluster, vector}`, `eidetic-ml::image_io`.
- New tables: `persons`, `faces`, `face_detection_runs`, `face_person_rejections`, `face_links`. Six Postgres migrations collapsed into three SQLite ones.
- Ignored end-to-end tests need real inputs: `EIDETIC_FACE_TEST_IMAGE` (geometry on one portrait) and `EIDETIC_FACE_LFW_DIR` (identity discrimination over an LFW-style directory).

## v0.2.1 — 2026-05-24

Patch release. One user-visible bugfix, one internal refactor.

### Fixed

- **JPEG / DNG portrait photos no longer render sideways.** iPhone JPEGs and DNGs ship raw landscape sensor pixels plus an EXIF `Orientation` tag (commonly value 6 = "rotate 90° clockwise"). The `image` crate doesn't auto-apply the tag, so portrait photos previously displayed on their side in the grid, detail page, and through SigLIP's preprocessor. The fix reads the EXIF orientation after decode and applies the matching transform on the `DynamicImage`. HEIC is unchanged — libheif's `irot` transform already runs during decode, so re-rotating would double-rotate. In the verified library this fixes 31 photos (20 JPEGs with orient=6/8, 11 DNGs with orient=6).

### Changed

- **`import_file` refactored to use `?`** — ten `match … return ImportOutcome::Failed(e)` arms collapse to `?` calls with `#[from]` on `Error::Db`. Same external behaviour, ~50 fewer lines.

### Migration note

If you imported your library under v0.2.0, the existing thumbnails are still oriented the old (sideways) way. To materialise the fix on already-imported rows, reset and reimport (`docker compose down -v && eidetic import …`). This is the established convention for decode-path changes at this stage.

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
