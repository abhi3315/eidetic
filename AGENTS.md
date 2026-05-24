# AGENTS.md

Context for AI coding agents working on Eidetic. Read this before making changes.

This file works for any agent that supports the `AGENTS.md` convention (Claude Code, Cursor, Copilot, Codex, OpenCode, Aider). No tool-specific overrides yet — when there are, they go in a separate `CLAUDE.md` next to this file.

---

## What this project is

Eidetic is a self-hosted personal media intelligence system. It ingests photos and videos, deduplicates them, extracts EXIF + visual embeddings, and (eventually) groups faces and generates prompt-driven reels. CLI-first; written in Rust; built primarily with AI assistance.

This is one person's photo library on one person's hardware. Not a SaaS, not multi-user. See `goals.md` for product scope and anti-goals.

---

## Stack (current)

- **Language:** Rust 1.92, edition 2024
- **Async runtime:** Tokio
- **Web framework:** Axum (added when `eidetic-server` exists)
- **Database:** PostgreSQL + pgvector + VectorChord (see ADR-0002)
- **ML inference:** ONNX Runtime via the `ort` crate (see ADR-0003 once written)
- **Image embedding:** SigLIP 2 base, 768-dim (see ADR-0004 once written)
- **Face detection / recognition:** SCRFD + ArcFace via `ort`
- **Vector storage:** pgvector + VectorChord index
- **CLI:** clap (derive)
- **Errors:** `thiserror` in libraries, `anyhow` in binaries
- **Logging:** `tracing` + `tracing-subscriber`

ADRs in `docs/adr/` are the source of truth for *why* each choice was made.

---

## Workspace map

Crates under `crates/`:

| Crate | Role | Depends on |
|---|---|---|
| `eidetic-core` | Shared types: `AssetId`, `Sha256`, `Config`, `Paths`. Zero deps on tokio/sqlx/ort. Hosts `dng::extract_largest_jpeg_preview` (DNG decoding needed by both ingest and ml, pure-Rust via kamadak-exif) and `geocoder::Geocoder` (offline nearest-city lookup over a GeoNames cities500 dataset cached at `~/.cache/eidetic/geonames/`). | (nothing internal) |
| `eidetic-db` | sqlx pool, migration runner. `PgAssetsRepo` owns asset CRUD + search. Defines `NewAsset` / `InsertOutcome`. | `eidetic-core` |
| `eidetic-ingest` | File watcher, streaming hasher, content-addressable storage, thumbnail generation (JPEG/PNG/WEBP + HEIC via libheif hook + DNG via embedded preview extraction), comprehensive EXIF extraction (typed camera-settings columns + JSONB long tail). Calls `PgAssetsRepo` directly. Owns `ensure_heic_registered` — any new decode site in this crate must call it. | `eidetic-core`, `eidetic-db` |
| `eidetic-ml` | `SiglipEmbedder` (concrete, no trait) loads ONNX models via `ort` and produces L2-normalised image/text embeddings as `Vec<f32>`. Decodes JPEG/PNG/WEBP/HEIC/DNG via the `image` crate (HEIC through the libheif hook; DNG via `eidetic_core::dng::extract_largest_jpeg_preview`). | `eidetic-core` |
| `eidetic-server` | Axum HTTP server. `serve()` owns the embedder worker and the route table (`/`, `/search`, `/assets/:id`, `/assets/:id/raw`, `/thumbs/:size/:hash`). Localhost-bound, no auth. | `eidetic-core`, `eidetic-db`, `eidetic-ml` |
| `eidetic-cli` | Binary. Wires up dependencies and exposes subcommands. | `eidetic-core`, `eidetic-db`, `eidetic-ingest`, `eidetic-server` |

Crates added later when there's actual code that wants to live in them:

- `eidetic-search` — composes `eidetic-ml` + `eidetic-db` for semantic search.
- `eidetic-e2e` — workspace-level end-to-end tests as a dedicated test crate (Cargo doesn't pick up `tests/` at workspace root).

**Rule:** there must be at least one concrete caller in another crate before a new crate exists. No empty skeletons.

---

## Conventions

### Cargo

- Workspace root has `[workspace.package]` and `[workspace.dependencies]`.
- Every sub-crate `Cargo.toml` opts in via `edition.workspace = true`, `rust-version.workspace = true`, `license.workspace = true`, `authors.workspace = true`. The parent block does **not** auto-flow.
- Sub-crates pull dependencies via `tokio = { workspace = true }` etc. Don't add a version anywhere except `[workspace.dependencies]`.

### Errors

- Library crates (`eidetic-core`, `eidetic-db`, `eidetic-ingest`, future `eidetic-ml`): define typed errors with `thiserror`. One enum per crate, in `src/error.rs`. Callers can match.
- Binary crates (`eidetic-cli`, future `eidetic-server`): use `anyhow::Result<()>` in `main`. Print and exit on error.
- No `unwrap()` outside tests. `expect()` only with a message that documents the invariant being asserted.

### Patterns

- **Newtype IDs** for every entity: `AssetId`, `PersonId`, `FaceId`. Prevents mixing IDs at the type level.
- **Repository traits** for DB access (`AssetsRepo`, `FacesRepo`). Other crates depend on the trait, not on `sqlx::Pool` directly.
- **`tokio::sync::mpsc`** for async pipelines (watcher → workers). Not `Arc<Mutex<Vec<…>>>`.
- **Sync by default**, async only at I/O boundaries.

### Testing

- Unit tests in-file with `#[cfg(test)] mod tests { … }`.
- Integration tests in `crates/<crate>/tests/`.
- DB tests use `testcontainers` against real Postgres + pgvector + VectorChord. **No SQL mocks.**
- End-to-end tests live in `crates/eidetic-e2e/tests/` (when that crate exists).

### Commits

Light Conventional Commits: `<type>: <imperative subject under 60 chars>`. Body explains *why*, not what.

Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `build`, `ci`.

Don't add `Co-Authored-By` lines.

---

## Don't do

- Don't put business logic in `eidetic-cli` or `eidetic-server` binaries. They wire dependencies and call into the libs. Logic lives in lib crates.
- Don't `unwrap()` outside tests.
- Don't add a new abstraction (trait, generic, builder) for a single caller. Wait for two.
- Don't commit ML model files. They're auto-downloaded to `~/.cache/eidetic/models/`.
- Don't add a new crate without updating this file's workspace map.
- Don't add a dependency anywhere except the workspace `[workspace.dependencies]` table.
- Don't write `cargo test` into pre-commit hooks. CI runs tests; pre-commit stays at fmt + clippy.
- Don't add `Co-Authored-By` to commit messages.
- Don't invent file layout. The structure described here and in `IMPLEMENTATION_KICKOFF.md` is canonical.

---

## Run / build / test

```bash
# From workspace root (this directory):

cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Single crate:
cargo build -p eidetic-core
cargo test -p eidetic-ingest

# CLI:
cargo run -p eidetic-cli -- <subcommand>
```

For database work, `eidetic-db` expects `EIDETIC_DATABASE_URL` set or a default `postgres://eidetic:eidetic@localhost:5432/eidetic`.

---

## Where to read more

- `docs/adr/` — every architectural decision with context, trade-offs, and alternatives considered.
- `goals.md` — product scope. What this project is and isn't.
- `README.md` — short user-facing description.
- `../STACK_AUDIT.md`, `../IMPLEMENTATION_KICKOFF.md` — planning artifacts that pre-date the repo. Live outside the workspace, not committed. Useful for historical context if present, but ADRs are the authoritative source for current decisions.
