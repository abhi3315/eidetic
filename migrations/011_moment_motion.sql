-- Per-moment motion for video frames (goals-v0.9.md phase 2).
--
-- `motion` is the mean absolute luma change between the sampled frame and a
-- companion frame decoded a fraction of a second later (see
-- eidetic-ingest::motion), in 0..1. It is measured from decoded pixels at
-- embed time, so it lives beside the frame it describes.
--
-- NULL means "not measured", never "still": every row that existed before
-- this migration is NULL, as is any moment whose companion frame could not be
-- decoded. A measured still shot is 0.0. Nothing here is backfilled — that
-- needs ffmpeg and the original files, so it is an explicit command
-- (`eidetic embed --backfill-motion`), not a migration side effect.
--
-- Photos have no row here at all: a photo is not a zero-motion video, and
-- the `embeddings` table deliberately gains no motion column.
ALTER TABLE frame_embeddings ADD COLUMN motion REAL
    CHECK (motion IS NULL OR (motion >= 0.0 AND motion <= 1.0));
