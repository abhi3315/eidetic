-- Image embedding storage.
--
-- Per ADR-0002: pgvector + VectorChord (drop-in pgvector-compatible).
-- Per ADR-0004: SigLIP 2 base produces 768-dimensional vectors.
--
-- The vchordrq index is VectorChord's primary index method. If we
-- ever need to fall back to vanilla pgvector, the migration is:
--   DROP INDEX assets_embedding_idx;
--   CREATE INDEX assets_embedding_idx ON assets
--       USING hnsw (embedding vector_cosine_ops);
-- Schema and column type stay the same.

CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS vchord;

ALTER TABLE assets ADD COLUMN embedding vector(768);

CREATE INDEX assets_embedding_idx ON assets
    USING vchordrq (embedding vector_cosine_ops);
