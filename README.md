# Eidetic

Self-hosted media intelligence system. A personal photo and video library with semantic search, deduplication, and (eventually) face grouping and prompt-driven reel generation.

Written in Rust. Built primarily with AI assistance.

## What works today

- `eidetic import <file|dir>` — import photos/videos into the library
  - Filters non-media files by magic bytes (not extension)
  - Deduplicates by content hash
  - Extracts EXIF: date taken, GPS, camera make/model
  - Stores files in a content-addressable layout under `EIDETIC_LIBRARY_DIR`
- `eidetic embed` — generate SigLIP 2 embeddings for all imported images (~1.4 GiB model download on first run)
- `eidetic search "dog on beach"` — find photos by natural-language description
  - `--limit N` — number of results (default 10)
  - `--fields score,date,path` — tab-separated column output
  - `--json` — full JSON array
- `eidetic stats` — library summary (asset counts, size, date range, embedding coverage)
- `eidetic hash <file>` — SHA-256 a file, no setup needed

## Quick start

```bash
# 1. Start Postgres (VectorChord variant required for vector search)
docker compose up -d

# 2. Set env vars (or copy .env.example to .env and source it)
export EIDETIC_DATABASE_URL="postgres://eidetic:eidetic@localhost:5432/eidetic"

# 3. Import your photos (migrations run automatically on first connect)
cargo run --release -p eidetic-cli -- import ~/Pictures/

# 4. Generate embeddings (downloads SigLIP 2 model ~1.4 GiB on first run)
cargo run --release -p eidetic-cli -- embed

# 5. Search
cargo run --release -p eidetic-cli -- search "golden hour at the beach"
cargo run --release -p eidetic-cli -- search "birthday cake" --limit 5 --fields score,date,path

# 6. See what's in the library
cargo run --release -p eidetic-cli -- stats
```

## Configuration

All settings have sensible defaults (`~/.cache/eidetic/`). Override with environment variables:

| Variable | Default | Description |
|---|---|---|
| `EIDETIC_DATABASE_URL` | `postgres://eidetic:eidetic@localhost:5432/eidetic` | Postgres connection string |
| `EIDETIC_LIBRARY_DIR` | `~/.cache/eidetic/library` | Content-addressable file store |
| `EIDETIC_MODELS_CACHE` | `~/.cache/eidetic/models` | SigLIP 2 ONNX model cache |
| `EIDETIC_LOG` | `info` | Log level (trace/debug/info/warn/error) |

Copy `.env.example` to `.env` and adjust as needed. There is no config file — env vars are the only configuration layer for now.

## Database

Requires Postgres with the [VectorChord](https://github.com/tensorchord/VectorChord) extension (`pgvector` compatible, built-in ANN index). The `docker-compose.yml` uses the official image.

```bash
# Start
docker compose up -d

# Stop (data persists in Docker volume)
docker compose down

# Wipe everything and start fresh
docker compose down -v
```

Migrations run automatically on every `eidetic` startup that connects to the database. They are idempotent and safe to run repeatedly.

## Build

```bash
# Install pre-commit hook (runs fmt + clippy on every commit)
./scripts/install-hooks.sh

# Build
cargo build --workspace

# Test (integration tests require Docker)
cargo test --workspace
```

## Documentation

- [`AGENTS.md`](AGENTS.md) — context for AI agents working on this codebase
- [`docs/adr/`](docs/adr/) — architecture decisions
- [`goals.md`](goals.md) — what this project is and isn't

## License

MIT OR Apache-2.0 at your option.
