-- Video support (ADR-0011).
--
-- Two pieces:
--   1. Probe metadata on `assets` — duration, codec and pixel dimensions,
--      filled by ffprobe at import time. NULL for images and for videos
--      imported while ffmpeg was absent (the thumbnail/embed backfills
--      re-probe those).
--   2. `frame_embeddings` — SigLIP vectors for sampled video frames, one row
--      per (asset, timestamp). Videos get *several* rows where images get one
--      row in `embeddings`; search takes the max over an asset's frames, so a
--      single matching moment surfaces the whole video, and the timestamp of
--      that moment rides along for reel generation to cut at.
--
-- Same conventions as 002_embeddings.sql: vectors are little-endian f32
-- BLOBs, `dim` makes a model change detectable, no ANN index by design.

ALTER TABLE assets ADD COLUMN duration_secs REAL;
ALTER TABLE assets ADD COLUMN video_codec TEXT;
ALTER TABLE assets ADD COLUMN pixel_width INTEGER;
ALTER TABLE assets ADD COLUMN pixel_height INTEGER;

CREATE TABLE frame_embeddings (
    asset_id TEXT    NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    -- Seconds from the start of the video this frame was sampled at.
    ts_secs  REAL    NOT NULL,
    dim      INTEGER NOT NULL,
    vector   BLOB    NOT NULL,
    PRIMARY KEY (asset_id, ts_secs)
) WITHOUT ROWID;

-- "which videos still need frame embeddings" is an anti-join on asset_id.
CREATE INDEX frame_embeddings_asset_idx ON frame_embeddings(asset_id);
