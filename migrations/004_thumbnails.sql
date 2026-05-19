-- Thumbnails: per-asset generation tracking.
--
-- `thumbnails_generated = TRUE` only when both the 256px and 1024px
-- JPEG thumbnails wrote successfully to the on-disk CAS-shaped path
-- under `<library_dir>/.thumbs/{s,m}/`. False is the default and
-- the retry state — `eidetic thumbnail` finds these rows.

ALTER TABLE assets
    ADD COLUMN thumbnails_generated BOOLEAN NOT NULL DEFAULT FALSE;
