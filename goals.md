# Goals

## What this project is

A self-hosted media intelligence system. Local Google Photos-style management for a personal photo and video library, with:

- Automatic ingestion and deduplication — done
- Semantic search ("dog on beach", "Kashmir trip 2025") — done, photos and video moments
- Face grouping — done (v0.3)
- Prompt-driven reel generation — done (v0.4)

Everything above shipped in v0.4.0. The next phase is scoped in [goals-v0.5.md](goals-v0.5.md): video intelligence (playback transcoding, faces in videos, scene-aware sampling/reels, speech search).

## Who it's for

One person: me. My personal photo library, on my own hardware.

## What it's not

- **Not a SaaS.** No multi-tenant features, ever.
- **Not a competitor to Immich or PhotoPrism.** Different purpose: learning Rust deeply on a real systems project, plus owning a media engine end-to-end.
- **Not a UI project.** CLI and (eventual) WebDAV are the only interfaces.

## Anti-goals

- Generic web UI
- Multi-user authentication
- Real-time sync across devices
- Aesthetic scoring (LAION) — blur detection is enough
- Anything that requires the cloud
