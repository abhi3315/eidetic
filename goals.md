# Goals

## What this project is

A self-hosted media intelligence system. Local Google Photos-style management for a personal photo and video library, with:

- Automatic ingestion and deduplication — done
- Semantic search ("dog on beach", "Kashmir trip 2025") — done, photos and video moments
- Face grouping — done (v0.3)
- Prompt-driven reel generation — done (v0.4)

Everything above shipped in v0.4.0. Since then: video intelligence — playback transcoding, faces in videos, scene-aware sampling/reels, speech search ([goals-v0.5.md](goals-v0.5.md), shipped v0.5–v0.6), structured search filters and the web viewer's People section ([goals-v0.7.md](goals-v0.7.md), shipped v0.7), and the agentic edit engine — reel project files, edit ops, transitions, variations, OTIO export, and the `eidetic mcp` server ([goals-v0.8.md](goals-v0.8.md), shipped v0.8). The current phase is [goals-v0.9.md](goals-v0.9.md): eyes, taste, and recipes.

## Who it's for

One person: me. My personal photo library, on my own hardware.

## What it's not

- **Not a SaaS.** No multi-tenant features, ever.
- **Not a competitor to Immich or PhotoPrism.** Different purpose: learning Rust deeply on a real systems project, plus owning a media engine end-to-end.
- **Not a UI project.** The interfaces are the CLI, a deliberately minimal localhost web viewer (`eidetic serve`, v0.7), read-only WebDAV (v0.6), and the MCP server for agents (v0.8). None of them is a product surface to invest in for its own sake — the CLI and MCP are where capability lands first.

## Anti-goals

- A general-purpose web UI (the minimal viewer exists to *see* the library; editing, curation and administration stay in the CLI/MCP)
- Multi-user authentication
- Real-time sync across devices
- Aesthetic scoring (LAION) — blur detection is enough
- Anything that requires the cloud
