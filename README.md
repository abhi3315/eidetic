# Eidetic

Self-hosted media intelligence system. A personal photo and video library with semantic search, deduplication, and (eventually) face grouping and prompt-driven reel generation.

Written in Rust. Built primarily with AI assistance.

## Status

Pre-alpha. Ingestion pipeline is working — import, dedup, MIME filtering, and EXIF extraction. Semantic search (embedding model + query CLI) is not yet built.

## What works today

- `eidetic hash <file>` — SHA-256 a file, no setup needed
- `eidetic import <file|dir>` — import photos/videos into the library
  - Filters non-media files by magic bytes (not extension)
  - Deduplicates by content hash
  - Extracts EXIF: date taken, GPS, camera make/model
  - Stores files in a content-addressable layout under `EIDETIC_LIBRARY_DIR`

## Quick start

```bash
# One-time: install pre-commit hook (runs fmt + clippy on commit)
./scripts/install-hooks.sh

# Build everything
cargo build --workspace

# Hash a file (no DB needed)
cargo run -p eidetic-cli -- hash <path>

# Import photos (requires Postgres — see below)
cargo run -p eidetic-cli -- import ~/Pictures/vacation/
```

## Postgres setup

Requires pgvector + VectorChord. Easiest with Docker:

```bash
docker run -d \
  --name eidetic-pg \
  -e POSTGRES_USER=eidetic \
  -e POSTGRES_PASSWORD=eidetic \
  -e POSTGRES_DB=eidetic \
  -p 5432:5432 \
  tensorchord/vchord-postgres:pg17-v0.4.3

export EIDETIC_DATABASE_URL="postgres://eidetic:eidetic@localhost:5432/eidetic"
export EIDETIC_LIBRARY_DIR="$HOME/.cache/eidetic/library"
```

Migrations run automatically on first connect.

## Documentation

- [`AGENTS.md`](AGENTS.md) — context for AI agents working on this codebase
- [`docs/adr/`](docs/adr/) — architecture decisions
- [`goals.md`](goals.md) — what this project is and isn't
- [`../STACK_AUDIT.md`](../STACK_AUDIT.md) — verified stack choices (2026-04-26)
- [`../IMPLEMENTATION_KICKOFF.md`](../IMPLEMENTATION_KICKOFF.md) — how we're building this

## License

MIT OR Apache-2.0 at your option.
