-- Image embedding storage (ADR-0005).
--
-- Embeddings live in their own table rather than as a nullable column on
-- `assets`. That keeps "which assets still need embedding" a plain anti-join,
-- avoids rewriting wide asset rows on every embed, and leaves room for a
-- second embedding type (face vectors, ADR-pending) as another table with the
-- same shape.
--
-- `vector` is the raw f32 sequence in little-endian byte order. `dim` records
-- the length so a model change (e.g. 768 -> 1152 per ADR-0007) is detectable
-- rather than silently mixing dimensions in one search.
--
-- There is no ANN index: search is exact brute-force cosine computed in Rust
-- behind the `VectorIndex` trait. At personal-library scale that is both the
-- most accurate option (100% recall) and fast enough. See ADR-0005 for the
-- escalation path.

CREATE TABLE embeddings (
    asset_id TEXT    PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    dim      INTEGER NOT NULL,
    vector   BLOB    NOT NULL
);
