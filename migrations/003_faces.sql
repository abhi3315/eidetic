-- Face detection, embedding and person grouping (ADR-0010).
--
-- Three concerns, deliberately separated:
--   1. `faces`       — what the detector found, one row per face per asset.
--   2. `persons`     — a named cluster.
--   3. constraints   — the user's corrections, stored as durable facts.
--
-- (3) is the part that matters. Clustering is heuristic and will be wrong
-- sometimes, so the user has to be able to fix it. If corrections lived only as
-- cluster state, a re-cluster would wipe them — which is exactly what Immich's
-- documented full reset does ("deletes all previously assigned names"). Storing
-- them as separate must-link / must-not-link rows means re-clustering is always
-- safe to run: the clusterer reads the constraints and honours them.

CREATE TABLE persons (
    id         TEXT PRIMARY KEY NOT NULL,
    -- NULL until the user names them: clustering produces unnamed candidates.
    name       TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- Set when the user merges B into A; the row is kept so a later re-cluster
    -- that re-splits them can be re-merged instead of silently undoing the fix.
    merged_into TEXT REFERENCES persons(id) ON DELETE SET NULL
);

CREATE INDEX persons_name_idx ON persons (name) WHERE name IS NOT NULL;

CREATE TABLE faces (
    id       TEXT PRIMARY KEY NOT NULL,
    asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,

    -- Bounding box in original-image pixels.
    bbox_x      REAL NOT NULL,
    bbox_y      REAL NOT NULL,
    bbox_w      REAL NOT NULL,
    bbox_h      REAL NOT NULL,

    -- The five landmarks, already normalised to template order
    -- (left eye, right eye, nose, left mouth, right mouth). Stored so a crop
    -- can be re-aligned later without re-running detection.
    lm_left_eye_x     REAL NOT NULL,
    lm_left_eye_y     REAL NOT NULL,
    lm_right_eye_x    REAL NOT NULL,
    lm_right_eye_y    REAL NOT NULL,
    lm_nose_x         REAL NOT NULL,
    lm_nose_y         REAL NOT NULL,
    lm_left_mouth_x   REAL NOT NULL,
    lm_left_mouth_y   REAL NOT NULL,
    lm_right_mouth_x  REAL NOT NULL,
    lm_right_mouth_y  REAL NOT NULL,

    -- Detector confidence. Used as a quality gate: a low-score face must never
    -- seed a cluster, because bad embeddings become false bridges between
    -- distinct people.
    score REAL NOT NULL,

    -- Little-endian f32 blob, same convention as `embeddings.vector`. `dim`
    -- records the width so a model change (128-dim sface vs 512-dim auraface)
    -- is detectable instead of silently mixing incompatible vectors.
    embedding BLOB,
    dim       INTEGER,

    -- Which person this face is currently attributed to, and how that
    -- happened. 'user' assignments are frozen: the clusterer must never
    -- reassign them.
    person_id         TEXT REFERENCES persons(id) ON DELETE SET NULL,
    assignment_source TEXT NOT NULL DEFAULT 'auto'
        CHECK (assignment_source IN ('auto', 'user')),

    detected_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX faces_asset_idx  ON faces (asset_id);
CREATE INDEX faces_person_idx ON faces (person_id) WHERE person_id IS NOT NULL;
-- Drives the "which faces still need clustering" scan.
CREATE INDEX faces_unassigned_idx ON faces (id) WHERE person_id IS NULL;

-- Per-asset detection tracking. Separate from `faces` because "we looked and
-- found zero faces" must be distinguishable from "we have not looked yet" —
-- otherwise every faceless photo is retried on every run.
CREATE TABLE face_detection_runs (
    asset_id    TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    face_count  INTEGER NOT NULL,
    detected_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- "This face is NOT that person." Written when the user rejects an
-- attribution. The assigner must consult this before attaching a face, and
-- re-clustering must honour it.
CREATE TABLE face_person_rejections (
    face_id    TEXT NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
    person_id  TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (face_id, person_id)
);

-- "These two faces are (not) the same person", independent of any current
-- cluster. `must_link` = 1 records a merge, 0 records a split, so a
-- re-cluster reproduces the user's intent rather than re-deriving it.
--
-- face_a < face_b is enforced so a pair cannot be stored twice in
-- opposite orders.
CREATE TABLE face_links (
    face_a     TEXT NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
    face_b     TEXT NOT NULL REFERENCES faces(id) ON DELETE CASCADE,
    must_link  INTEGER NOT NULL CHECK (must_link IN (0, 1)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (face_a, face_b),
    CHECK (face_a < face_b)
);
