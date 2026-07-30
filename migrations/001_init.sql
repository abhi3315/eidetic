-- Initial schema for Eidetic on embedded SQLite (ADR-0005).
--
-- This replaces the six Postgres migrations that preceded it. The SQLite
-- database is always created fresh (no deployment ever ran the Postgres
-- schema and then switched), so the incremental ALTERs are collapsed into
-- one CREATE TABLE. The Postgres history lives in git.
--
-- Type mapping notes (SQLite has no native UUID/JSONB/TIMESTAMPTZ):
--   * `id` is a UUID stored as lowercase hyphenated TEXT so the database is
--     readable with the plain `sqlite3` CLI.
--   * timestamps are RFC3339 TEXT in UTC.
--   * `exif_raw` is a JSON document stored as TEXT (JSON1 functions apply).
--   * booleans are INTEGER 0/1, which is what sqlx maps `bool` to.

CREATE TABLE assets (
    id                   TEXT    PRIMARY KEY NOT NULL,
    hash                 TEXT    UNIQUE NOT NULL,
    original_filename    TEXT    NOT NULL,
    storage_path         TEXT    NOT NULL,
    file_size            INTEGER NOT NULL,
    mime_type            TEXT,
    created_at           TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    imported_at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    -- EXIF: date/location/camera
    date_taken           TEXT,
    latitude             REAL,
    longitude            REAL,
    camera_make          TEXT,
    camera_model         TEXT,

    -- EXIF: camera settings + the long tail
    lens_make            TEXT,
    lens_model           TEXT,
    focal_length         REAL,
    focal_length_35mm    REAL,
    aperture             REAL,
    shutter              TEXT,
    iso                  INTEGER,
    orientation          INTEGER,
    altitude             REAL,
    gps_direction        REAL,
    exif_raw             TEXT,

    -- Offline reverse-geocoded place columns
    country_code         TEXT,
    country_name         TEXT,
    admin1               TEXT,
    place                TEXT,
    place_distance_m     REAL,

    -- Thumbnail generation tracking
    thumbnails_generated INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX assets_imported_at_idx ON assets (imported_at DESC);

CREATE INDEX assets_date_taken_idx
    ON assets (date_taken DESC)
    WHERE date_taken IS NOT NULL;
