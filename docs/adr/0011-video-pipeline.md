# ADR-0011: Video pipeline = shell out to system ffmpeg, frames as moments

Date: 2026-08-21
Status: accepted

## Context

Since v0.2.0 videos import, dedupe and land in the grid as placeholder tiles — no thumbnail, no embedding, invisible to search. goals.md lists prompt-driven reel generation as the stretch goal, and reels need to know *where inside a video* a prompt matches, not just that the file matches. Whatever decodes video frames therefore shapes three features at once: thumbnails, semantic search, and reels.

The decode options:

1. **Link libav via `ffmpeg-next`/`ffmpeg-sys`.** Build-time C dependency on the libav* dev headers — strictly worse than the libheif situation ADR-0008/0009 just spent two releases containing, multiplied across every release-matrix target.
2. **Vendor ffmpeg binaries** (e.g. `ffmpeg-sidecar` downloads them at runtime). Adds a network fetch of an executable — a supply-chain and packaging liability for questionable gain.
3. **Shell out to system `ffmpeg`/`ffprobe`.** Zero build-time cost, zero link-time cost, works with whatever codecs the user's ffmpeg was built with. This is what Immich and PhotoPrism do for the same job.

## Decision

- **Shell out to `ffmpeg` and `ffprobe` found on `PATH`**, overridable via `EIDETIC_FFMPEG_PATH` / `EIDETIC_FFPROBE_PATH`. No new Cargo features: the dependency is runtime-optional, not build-optional.
- **Graceful degradation, loudly.** Without ffmpeg, videos keep importing exactly as today; thumbnail/embed passes log one warning naming the missing binary and skip video work. No command fails because ffmpeg is absent.
- **Probe at import** (`ffprobe -print_format json`): store `duration_secs`, `video_codec`, `width`, `height` on the asset (migration 004). Probe failure on a video-mimed file is a per-file warning, not an import failure.
- **Thumbnail = one frame at 10% of duration** (clamped to ≥0.5s in), piped out of ffmpeg as PNG on stdout and fed through the existing 256/1024 JPEG thumbnail path — videos and photos share the `.thumbs/` layout and the grid code.
- **Search = frames as moments.** `eidetic embed` samples up to 5 frames per video, evenly spaced across the middle 80% of the duration, embeds each with SigLIP, and stores them in a `frame_embeddings` table: `(asset_id, ts_secs, vector)`. A video's search score is the **max over its frames** (best matching moment), so one match inside a long video is enough to surface it. The matching timestamp rides along in results — that timestamp is the reel generator's cut point.
- **Reels reuse the same data.** `eidetic reel` is a consumer of frame_embeddings + duration metadata, not a new pipeline: search picks moments, ffmpeg cuts and concatenates.

## Consequences

**Good:**

- The no-native-deps build guard (ADR-0009) is untouched; release artifacts don't change.
- Codec coverage is the user's ffmpeg build's problem, which on every mainstream distro means "everything".
- One frame-sampling design serves thumbnails, search and reels; per-frame timestamps make "find the moment" queries and reel cuts the same operation.
- Max-over-frames scoring composes with the existing exact BruteForce scan — frame vectors are just more rows in the same in-process search.

**Bad / accepted:**

- A subprocess per video per operation. Fine at personal-library scale (hundreds of videos); batching exists as an escalation.
- 5 frames per video under-samples long multi-scene footage. Accepted: even coverage of the middle 80% catches the dominant scenes, and the sample count is a constant that can grow later without schema changes.
- Parsing ffprobe JSON couples us to its output format. Accepted: it has been stable for a decade and is versioned by the user's ffmpeg install.
- No hardware-accelerated decode. Irrelevant at this scale; ffmpeg flags could add it later.

## Alternatives considered

- **`ffmpeg-next` (link libav).** Rejected: reintroduces the C build-dependency class of problem ADR-0008/0009 exist to fight, on all platforms at once.
- **`ffmpeg-sidecar` (auto-download binaries).** Rejected: runtime binary download is a supply-chain liability and violates the spirit of "nothing leaves or enters the machine" beyond model downloads the user opts into.
- **GStreamer.** Rejected: heavier system dependency surface than ffmpeg for no benefit here.
- **Average frame embeddings into one vector per video.** Rejected: a single averaged vector blurs multi-scene videos and throws away the timestamp — the exact datum reels need.
- **Scene detection (`select='gt(scene,0.4)'`) instead of even sampling.** Deferred: better cut points in principle, but adds a full decode pass per video at embed time. Even sampling is predictable and cheap; revisit if reel quality demands it.

## References

- ADR-0008 (HEIC decode), ADR-0009 (distribution) — the system-dependency history this decision answers to.
- Immich ffmpeg usage: shell-out with per-codec flag sets, same architecture.
