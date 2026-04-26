# ADR-0002: Vector storage = Postgres + pgvector + VectorChord

Date: 2026-04-26
Status: accepted

## Context

Eidetic needs vector similarity search to power semantic queries ("dog on beach", "Kashmir trip"). Image embeddings will be 768-dim (SigLIP 2 base, see ADR-0004). Face embeddings will be 512-dim. Initial library size is small (low thousands of vectors); long-term target is in the low millions.

The vector-store decision is structural — it shapes the schema, the migration story, the deployment topology, and how query composition (vector similarity + filter on date/person/location) works.

## Decision

Use **PostgreSQL** as the database, with two extensions:

- **pgvector** — the `vector` type and basic index types (HNSW, IVFFlat).
- **VectorChord** — a drop-in pgvector-compatible index extension that's the successor to `pgvecto.rs`. Same SQL, same data types, better indexing speed and storage efficiency.

Schema example:

```sql
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS vchord;

ALTER TABLE assets ADD COLUMN embedding vector(768);

CREATE INDEX assets_embedding_idx ON assets
  USING vchordrq (embedding vector_cosine_ops);
```

Deploy the `tensorchord/vchord-postgres` Docker image which ships pgvector + VectorChord pre-installed.

## Consequences

**Good:**

- One database for everything. SQL joins between `assets`, `faces`, `persons`, EXIF JSONB, and embeddings just work — critical for queries like "find sunsets that contain Alice taken in 2024."
- Backups, replication, point-in-time-recovery all work via standard Postgres tooling.
- VectorChord's pgvector compatibility means we can drop the index and recreate it as a vanilla pgvector HNSW index without touching any schema or application code if VectorChord ever blocks us.
- pgvector 0.8's iterative scans handle filtered vector queries cleanly (no overfiltering surprise when the filter is selective).
- Immich (the reference implementation in this space) made the same call. Their docs explicitly state: *"Contextual CLIP search is powered by the VectorChord extension."*

**Bad / accepted:**

- Postgres + extensions = larger operational footprint than an embedded option (e.g., LanceDB).
- VectorChord requires either Docker or manual extension install; not in apt/brew yet.
- pgvector at very large scale (>10M vectors) may need pgvectorscale's DiskANN. We'll cross that when we get there.

## Alternatives considered

- **LanceDB.** Embedded, columnar Lance format, no server. Tempting for a CLI-first project. Rejected because we'll want SQL joins between assets ↔ faces ↔ persons ↔ locations. LanceDB doesn't give us that without a parallel relational store — defeats the simplicity of being embedded.
- **Qdrant.** Excellent vector engine, Rust-native. Rejected because it's vector-first by design — relational data goes in a sidecar database. Two services to operate, queries fan out across both, no joins.
- **sqlite-vec.** Embedded, no separate process. Rejected because filter+vector composition is limited compared to pgvector's iterative scans, and the long-term scale ceiling is lower than we'd want.
- **pgvecto.rs.** VectorChord is its successor, by the same team (tensorchord). New code goes here.
- **Bare pgvector (no VectorChord).** Workable but suboptimal. VectorChord is a drop-in upgrade with measurable indexing/storage wins. Cost of adding it is one `CREATE EXTENSION`.

## References

- pgvector 0.8 release notes (iterative scans): https://www.postgresql.org/about/news/pgvector-080-released-2952/
- VectorChord: https://github.com/tensorchord/VectorChord
- Immich uses VectorChord: https://docs.immich.app/features/searching/
