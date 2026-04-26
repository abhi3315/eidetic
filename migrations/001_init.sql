-- Initial schema for Eidetic.
--
-- Establishes the assets table: one row per ingested file, with the
-- SHA-256 hash as the deduplication key and a CAS-style storage path.
--
-- Embedding columns and face-related tables land in later migrations,
-- gated by ADR-0004 (embedding model) and ADR-pending (face stack).

CREATE TABLE assets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    hash CHAR(64) UNIQUE NOT NULL,
    original_filename TEXT NOT NULL,
    storage_path TEXT NOT NULL,
    file_size BIGINT NOT NULL,
    mime_type TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    imported_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX assets_imported_at_idx ON assets (imported_at DESC);
