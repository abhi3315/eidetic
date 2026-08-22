-- Scene boundaries per video (goals-v0.5.md #3).
--
-- Detected once with ffmpeg's scene filter (a full decode), then persisted
-- so every consumer is cheap afterwards: frame sampling picks one frame per
-- scene instead of five fixed offsets, and reel cuts snap to the nearest
-- boundary instead of slicing mid-shot. Rows are scene-change timestamps;
-- a single-shot video legitimately has zero rows, so "was detection run" is
-- tracked by the consumer (frame_embeddings existing), not by this table.
CREATE TABLE video_scenes (
    asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    ts_secs  REAL NOT NULL,
    PRIMARY KEY (asset_id, ts_secs)
) WITHOUT ROWID;
