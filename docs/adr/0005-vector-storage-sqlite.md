# ADR-0005: Vector storage = unified SQLite + pluggable vector index

Date: 2026-07-23
Status: accepted (supersedes ADR-0002)

## Context

ADR-0002 chose Postgres + pgvector + VectorChord. That was the right call for a server-based, scale-oriented deployment. Two things changed since:

1. **The deployment goal flipped.** Eidetic is being re-platformed from macOS to cross-platform (Linux + Windows), and the target is now a self-contained, no-server, no-Docker artifact per OS (ADR-0009). A Postgres server is the single largest thing between a user and "download, run." `goals.md` lists "anything that requires the cloud" as an anti-goal; a background database daemon is the local echo of that.
2. **The scale ceiling is confirmed low.** `goals.md` is explicit: one person, one machine, never multi-user, never SaaS. A personal library — even a heavy shooter over 20 years — is tens of thousands to a few hundred thousand vectors. VectorChord's ANN index earns its keep past ~1M; below that it's complexity we pay for and don't use.

ADR-0002 rejected SQLite and LanceDB. Both rejections were tied to assumptions that no longer hold:

- *"SQLite: filter+vector composition limited; scale ceiling lower."* True, but the ceiling is now a non-issue, and filtered search at this size is an exact brute-force scan.
- *"LanceDB doesn't give SQL joins without a parallel relational store."* Still true — and still why LanceDB isn't the answer. Eidetic is relational-first: hash dedup, `COUNT(*) FILTER` stats, EXIF/place filters, and the roadmap's asset↔face↔person joins. That's SQLite's home turf, not a columnar vector engine's.

The decision was stress-tested with three independent analyses (a pro-LanceDB architect, a durability-first skeptic, and a neutral reviewer that read every query in `PgAssetsRepo`). All three landed on SQLite; even the LanceDB advocate capped at 65% confidence and conceded the relational losses.

## Decision

Use **unified SQLite** as the single store — all relational metadata *and* embeddings in one file — accessed through the existing single-repo seam (`PgAssetsRepo`, to be renamed). Vector search sits behind a small **`VectorIndex` trait** so the index strategy is swappable without touching callers.

- **Storage:** sqlx `sqlite` backend (bundled `libsqlite3-sys`, statically compiled). One file under `~/.cache/eidetic/`. `EIDETIC_DATABASE_URL` and `docker-compose.yml` go away.
- **Embeddings:** their own table (not a nullable column on `assets`) — keyed by asset id, dimension-tagged. Keeps "which assets are unembedded" a clean anti-join, and lets a second embedding type (face vectors) land as another table without schema churn.
- **Vector search v1:** exact brute-force cosine (embeddings are L2-normalised → a dot product) computed in Rust behind the trait. No extension, no native index lib — the lowest-risk path, and the most *accurate* (100% recall).
- **Escalation (only past ~1M vectors):** swap the trait impl to `usearch` (embedded HNSW, mmap-able) or sqlite-vec's ANN once it stabilises. No store change, no caller change.

Migrations: port the six `migrations/*.sql` to SQLite dialect (`$1`→`?`, JSONB→JSON1 text, `gen_random_uuid()`→app-side, `TIMESTAMPTZ`→ISO-8601 text). sqlx's `migrate!` runner stays.

## Consequences

**Good:**

- No server, no Docker, no connection string — serves the single-artifact goal (ADR-0009) directly.
- One store, one transaction boundary — no vector↔metadata sync problem, and the roadmap's joins stay trivial SQL.
- Brute-force is *exact* — strictly better recall than the ANN index it replaces, at personal scale with imperceptible latency.
- SQLite is the most durable format available (Library of Congress recommended storage format; on-disk format backward-compatible since 2004, supported through 2050). For a library meant to outlive the tool, that matters.
- The `VectorIndex` trait means "start exact, escalate to ANN" needs no re-architecture.

**Bad / accepted:**

- Brute-force scales linearly. Low tens of ms at a few hundred thousand; past ~1M it degrades and we take the usearch escalation. Acceptable given the anti-goals.
- SQLite has no native `vector`/UUID/`TIMESTAMPTZ`/JSONB types — the repo encodes them (sqlx `uuid`/`chrono` map to text/blob). One-time repo-layer cost.
- sqlite-vec is a loadable extension and sqlx's extension-loading is currently rough — a reason v1 does brute-force-in-Rust rather than adopt sqlite-vec now.

## Alternatives considered

- **Keep Postgres + VectorChord (ADR-0002).** Rejected: the server is the deployment cost we're removing, and the scale it buys is disowned by `goals.md`.
- **LanceDB.** Reconsidered seriously (rewrite cost was explicitly off the table) — genuinely the better engine for a multimodal, vector-first, scale workload, and its multiple-vector-columns story fits the faces/reels roadmap. Rejected because eidetic is relational-first (no joins, no `ON CONFLICT` dedup, no `COUNT FILTER` without app-side plumbing), its Rust API is pre-1.0 with "expect breaking changes," and the Lance format is ~3 years old vs SQLite's decades. Revisit only if eidetic pivots to multimodal-first (video/reels dominant).
- **sqlite-vec as the v1 index.** Deferred, not rejected: right idea (vectors in SQLite), but alpha and awkward to load via sqlx today. The trait lets us adopt it later with zero caller churn.
- **usearch from day one.** Rejected for v1: HNSW is approximate and adds an index to persist/sync; unnecessary at current scale. It's the documented escalation, not the start.

## References

- `goals.md` — personal, single-machine, never SaaS
- SQLite recommended storage format: https://sqlite.org/locrsf.html
- SQLite long-term support (through 2050): https://sqlite.org/lts.html
- usearch: https://github.com/unum-cloud/usearch
- sqlite-vec ANN tracking issue: https://github.com/asg017/sqlite-vec/issues/25
- Supersedes ADR-0002.
