ALTER TABLE assets
    ADD COLUMN date_taken   TIMESTAMPTZ,
    ADD COLUMN latitude     FLOAT8,
    ADD COLUMN longitude    FLOAT8,
    ADD COLUMN camera_make  TEXT,
    ADD COLUMN camera_model TEXT;

CREATE INDEX assets_date_taken_idx
    ON assets (date_taken DESC)
    WHERE date_taken IS NOT NULL;
