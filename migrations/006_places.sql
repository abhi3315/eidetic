-- Offline reverse-geocoded place columns. Populated by `eidetic
-- backfill-places` (one-time) and inline in the import path going forward.
-- All nullable; the import path skips geocoding silently if the GeoNames
-- dataset isn't cached locally.
--
-- country_code is ISO 3166-1 alpha-2 (e.g., "IN", "US"). CHAR(2) is the
-- right shape - the value is always exactly two ASCII letters or NULL.
--
-- place_distance_m is the great-circle distance from the photo's GPS to
-- the matched city's centroid, in metres. Surfaced on the detail page as
-- a debugging aid so wrong-looking attributions stay visible.

ALTER TABLE assets
    ADD COLUMN country_code     CHAR(2),
    ADD COLUMN country_name     TEXT,
    ADD COLUMN admin1           TEXT,
    ADD COLUMN place            TEXT,
    ADD COLUMN place_distance_m REAL;
