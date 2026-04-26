# ADR-0001: Cargo workspace with flat crate layout

Date: 2026-04-26
Status: accepted

## Context

Eidetic will grow into several bounded modules: shared types, database access, file ingestion, ML inference, search composition, a CLI binary, and (later) an HTTP/WebDAV server. Layout choices on day one shape what's cheap to change later.

Two pressures specifically matter:

- **AI-assisted development.** Smaller, well-named crates double as context boundaries. An AI session working on the file watcher should be able to load just `crates/eidetic-ingest/` without dragging the rest of the codebase into context.
- **Dependency hygiene.** ML, async, and database stacks each pull large dependency trees. Workspace-level dependency tables stop version drift across crates.

## Decision

Use a Cargo workspace with a virtual root manifest (no `[package]` at the root), flat layout under `crates/`. Each crate is named exactly as its directory. Shared dependency versions live in `[workspace.dependencies]`. Sub-crate `Cargo.toml`s opt in to inherited package fields via `edition.workspace = true`, `rust-version.workspace = true`, etc.

Day-one crates:

- `eidetic-core` — shared types (`AssetId`, `Sha256`, errors, config). Zero deps on tokio/sqlx/ort.
- `eidetic-db` — sqlx pool, migration runner, repository traits + Postgres impls.
- `eidetic-ingest` — file watcher, hasher, content-addressable storage.
- `eidetic-cli` — binary that wires the others together.

Additional crates (`eidetic-ml`, `eidetic-search`, `eidetic-server`, `eidetic-e2e`) get added when there's concrete code that wants to live in them, not pre-emptively.

## Consequences

**Good:**

- One `cargo test --workspace` runs everything.
- Shared `Cargo.lock` means deterministic builds across all crates.
- `cargo build -p eidetic-core` builds just one crate when iterating fast.
- Cross-crate refactors (move a type, change a trait) are straightforward.
- AI sessions can scope to one crate cheaply.

**Bad / accepted:**

- Newcomers (including AI agents) must understand `[workspace.package]` doesn't auto-flow to sub-crates. Fix: `AGENTS.md` documents the `.workspace = true` opt-in pattern, and the sub-crate template in `IMPLEMENTATION_KICKOFF.md` shows it explicitly.
- `members = ["crates/*"]` requires every directory under `crates/` to be a valid crate. Stray notes or scratch files break the workspace. Mitigation: keep notes in `docs/`, not `crates/`.

## Alternatives considered

- **Single crate with internal modules.** Rejected: too coarse for AI context-loading. Forces `mod` boundaries to do double duty as both code organization and context boundaries, which they don't naturally serve.
- **Git submodules per component.** Rejected: heavy operational overhead for a solo project, and breaks the shared-lockfile property that makes workspaces useful.
- **Multi-repo (one repo per crate).** Rejected for the same reason — splits the change-coupling that exists between (e.g.) `eidetic-core` and `eidetic-db` across PRs in different repos.
