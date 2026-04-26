# Eidetic

Self-hosted media intelligence system. A personal photo and video library with semantic search, deduplication, and (eventually) face grouping and prompt-driven reel generation.

Written in Rust. Built primarily with AI assistance.

## Status

Pre-alpha. Workspace skeleton only — no member crates yet.

## Quick start

```bash
# One-time: install pre-commit hook (runs fmt + clippy on commit)
./scripts/install-hooks.sh

# Build everything
cargo build --workspace

# Hash a file (smoke test)
cargo run -p eidetic-cli -- hash <path>
```

Postgres + pgvector + VectorChord (via the `tensorchord/vchord-postgres` Docker image) is required for db-backed work — not for the standalone `hash` command.

## Documentation

- [`AGENTS.md`](AGENTS.md) — context for AI agents working on this codebase
- [`docs/adr/`](docs/adr/) — architecture decisions
- [`goals.md`](goals.md) — what this project is and isn't
- [`../STACK_AUDIT.md`](../STACK_AUDIT.md) — verified stack choices (2026-04-26)
- [`../IMPLEMENTATION_KICKOFF.md`](../IMPLEMENTATION_KICKOFF.md) — how we're building this

## License

MIT OR Apache-2.0 at your option.
