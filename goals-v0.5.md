# Goals: v0.5 — video intelligence

Status: proposed (2026-08-22)

v0.4 completed everything [goals.md](goals.md) set out to build. This document scopes the next phase from the video gaps found while dogfooding v0.4, each validated against market evidence (three research passes over Google/Apple/Immich/PhotoPrism/Ente/LibrePhotos behavior, their issue trackers, and implementation prior art — August 2026).

The one-line summary of the research: **the self-hosted field is photo-first and treats videos as an afterthought. Every gap below is either table-stakes we're missing or a differentiator nobody ships.**

## Features, in build order

### 1. Transcode for browser playback — *table-stakes*

Phones default to HEVC; Firefox cannot decode it at all. Today a chunk of any iPhone library is simply unwatchable in `eidetic serve`. Every serious self-hosted media server handles this: Immich pre-transcodes on upload as a background job (policy "Required": only codecs a browser can't play; H.264 target; originals kept), PhotoPrism transcodes on demand with caching and admits in its docs that on-demand causes "unacceptable delays" on first play.

- Evidence: [Immich system settings](https://docs.immich.app/administration/system-settings/), [Immich #3087](https://github.com/immich-app/immich/issues/3087) (Sony HEVC plays in Chrome, fails in Firefox), [PhotoPrism video docs](https://docs.photoprism.app/user-guide/organize/video/).
- **Minimal version:** at import (and as a backfill command), when `video_codec` isn't browser-safe (H.264/VP9/AV1-allowlist), pre-transcode once to H.264/AAC MP4 beside the thumbnails; `/raw` keeps serving originals, the `<video>` element gets the compatible copy. Keep originals always. No quality ladder, no HLS, no hardware-encode requirement.

### 2. Faces in videos — *medium-demand, zero-supply differentiator*

Nobody self-hosted ships it: Immich and PhotoPrism detect faces only in the video *thumbnail*, Ente and LibrePhotos are photo-only, and even Apple Photos requires manually drawing a face box on a paused frame. Only Google Photos delivers it. Demand is persistent: Immich discussion [#5936](https://github.com/immich-app/immich/discussions/5936) (89 upvotes since 2023, plus closed duplicates and two failed community implementation attempts, e.g. [PR #27744](https://github.com/immich-app/immich/pull/27744)).

- Evidence: [Immich FAQ](https://docs.immich.app/FAQ/) ("may be implemented in the future"), [PhotoPrism #3483](https://github.com/photoprism/photoprism/issues/3483), [LibrePhotos #561](https://github.com/LibrePhotos/librephotos/issues/561), [Apple 108795](https://support.apple.com/en-us/108795).
- **Minimal version** (converged on by both the Immich and PhotoPrism threads): run YuNet+AuraFace over the already-sampled frames (no new extraction machinery), dedup near-identical embeddings within a video, and **quality-gate video faces so they can only *match* photo-built persons, never seed new ones** — motion blur and compression artifacts poisoning clusters is the known failure mode. Store first-appearance timestamps so a person page deep-links into the moment. Re-runnable per-video without touching photo-face state.

### 3. Scene-aware sampling and reels — *high-value differentiator*

Google Photos Memories gets 3.5B+ views/month and half a billion monthly users; nothing comparable exists self-hosted — Immich's "Memories showcase" request ([#2836](https://github.com/immich-app/immich/discussions/2836)) has 226 upvotes, open since 2023, with a third-party bolt-on tool as the only answer. We already ship prompt-driven reels; scene awareness fixes their two weaknesses at once: fixed 5-frame sampling under-samples long multi-scene footage (search misses moments), and reel cuts land at arbitrary offsets instead of shot boundaries.

- Evidence: [Google Memories](https://blog.google/products-and-platforms/products/photos/google-photos-memories-view/), [PySceneDetect](https://github.com/Breakthrough/PySceneDetect) (5.1k stars; algorithm portable), ffmpeg's built-in `select='gt(scene,X)'` filter (zero new dependencies).
- **Minimal version:** detect scene changes with ffmpeg's scene filter at embed time; sample one frame per scene (capped, duration-scaled) instead of five fixed offsets; reels cut at the nearest scene boundary around the matched moment. Prompt-driven only — auto-curated "memories" pushed at the user is explicitly not the goal (that's the part Apple gets mocked for).

### 4. Speech search (stretch) — *nobody ships it, including Google and Apple*

Searching videos by what was *said* is a genuine first: Apple's iOS 18 video search is visual-only, Google indexes no speech, and no self-hosted manager touches audio. Demand is real but niche (~40 combined upvotes on Immich [#13503](https://github.com/immich-app/immich/discussions/13503)/[#14658](https://github.com/immich-app/immich/discussions/14658), vs 319 for OCR — with the on-point user quote: "It's just as often about words said in it as the visual content"). Feasibility is the easy part: [whisper-rs](https://github.com/tazz4843/whisper-rs) is mature with CUDA support, large-v3-turbo is ~1.6 GB and runs many-times-real-time on the GPU. The honest risk is accuracy on casual Hinglish home audio (27–70% WER on code-switched speech), which dictates the design: **transcripts are recall fuel, never display text**.

- Prior art for the exact architecture: [screenpipe](https://github.com/mediar-ai/screenpipe) (Rust, Whisper + frames + SQLite FTS5, one local search).
- **Minimal version:** extract audio at embed time (skip silent clips via an energy check), Whisper once per video, timestamped segments into SQLite FTS5, FTS hits merged into the existing ranking as one more scored signal with jump-to-timestamp. No subtitle UI, no diarization, no editing.

### 5. Perceptual video dedup — *niche; build only because it's nearly free*

Demand is single-digit upvotes ([Immich #10184](https://github.com/immich-app/immich/discussions/10184): 4) and nobody ships it — the 275-upvote dedup demand is about photos. But ffmpeg's built-in MPEG-7 [`signature` filter](https://github.com/FFmpeg/FFmpeg/blob/master/libavfilter/vf_signature.c) does re-encode-robust video fingerprinting through the shell-out we already have.

- **Minimal version:** compute the signature at import, flag probable re-encodes of existing clips as a warning in import output. No comparison UI, no auto-delete. Opportunistic — build it the day it annoys us, not before.

## Anti-goals (unchanged from goals.md, plus)

- No face tracking across frames — sampled frames only; nobody in this market expects tracking.
- No auto-curated, push-style "memories" — reels stay prompt-driven.
- No subtitle rendering/editing, no speaker diarization.
- No transcode quality ladders, HLS, or hardware-encode requirements.
- Still no cloud: every model and every byte stays on the machine.
