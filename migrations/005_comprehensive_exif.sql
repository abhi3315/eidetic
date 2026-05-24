-- Comprehensive EXIF capture. Adds typed columns for the camera-settings
-- fields the detail page renders directly, plus a JSONB column for the long
-- tail of every tag kamadak-exif can parse.
--
-- All columns are nullable; backfill populates them via `eidetic backfill-exif`.
-- Per Postgres 11+ docs, ADD COLUMN ... NULL with no DEFAULT is a catalog-only
-- change (no table rewrite); safe to run with the application live.

ALTER TABLE assets
    ADD COLUMN lens_make          TEXT,
    ADD COLUMN lens_model         TEXT,
    ADD COLUMN focal_length       REAL,
    ADD COLUMN focal_length_35mm  REAL,
    ADD COLUMN aperture           REAL,
    ADD COLUMN shutter            TEXT,
    ADD COLUMN iso                INTEGER,
    ADD COLUMN orientation        SMALLINT,
    ADD COLUMN altitude           FLOAT8,
    ADD COLUMN gps_direction      FLOAT8,
    ADD COLUMN exif_raw           JSONB;
