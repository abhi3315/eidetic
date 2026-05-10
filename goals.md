# Goals

## What this project is

A self-hosted media intelligence system. Local Google Photos-style management for a personal photo and video library, with:

- Automatic ingestion and deduplication
- Semantic search ("dog on beach", "Kashmir trip 2025")
- Face grouping (later phase)
- Prompt-driven reel generation (stretch)

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
