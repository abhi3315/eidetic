-- Producer identity for every model-derived row.
--
-- Until now a derived row recorded only its width (`dim`), and a completion
-- marker recorded only that *something* had run. Both are proxies for the
-- question that actually matters — "which model produced this?" — and both
-- fail:
--
--   * `dim` cannot separate two models of the same width. That is not
--     hypothetical: EIDETIC_FACE_MODEL=auraface and =buffalo_l both emit
--     512-dim embeddings from completely different weights, so a switch
--     between them silently mixes incompatible vectors into one similarity
--     space.
--   * A bare completion marker makes "we transcribed this with whisper-small"
--     indistinguishable from "we transcribed this with large-v3-turbo", so
--     changing the model leaves the whole library looking finished.
--
-- With `model_id` on the row, "is there work to do?" becomes a question about
-- the *active* model, and two models can coexist during a migration instead
-- of one silently shadowing the other.
--
-- Ids are the stable strings in eidetic-ml (`siglip::MODEL_ID`,
-- `face::MODEL_ID`, `speech::MODEL_ID`), not HuggingFace repo paths: a repo
-- can be renamed, and the id has to outlive that.

-- SigLIP image embeddings. The backfill is exact rather than a guess: the
-- three shipped variants have distinct widths, so `dim` determines the model.
ALTER TABLE embeddings ADD COLUMN model_id TEXT;
UPDATE embeddings SET model_id = CASE dim
    WHEN 768  THEN 'siglip2-base-patch16-256'
    WHEN 1024 THEN 'siglip2-large-patch16-384'
    WHEN 1152 THEN 'siglip2-so400m-patch14-384'
    ELSE 'unknown'
END;

-- Video frame embeddings, same model and same exact backfill.
ALTER TABLE frame_embeddings ADD COLUMN model_id TEXT;
UPDATE frame_embeddings SET model_id = CASE dim
    WHEN 768  THEN 'siglip2-base-patch16-256'
    WHEN 1024 THEN 'siglip2-large-patch16-384'
    WHEN 1152 THEN 'siglip2-so400m-patch14-384'
    ELSE 'unknown'
END;

-- Face recognition embeddings.
--
-- This backfill IS an assumption, and the honest one to make: auraface and
-- buffalo_l are both 512-dim, so existing rows carry nothing that identifies
-- which produced them. Every row is attributed to the default pair, because
-- an install that never set EIDETIC_FACE_MODEL used it. If a library really
-- was built with a non-default pair, the cost is that `eidetic faces` sees no
-- work until EIDETIC_FACE_MODEL is set back — recoverable, and far better
-- than mixing two 512-dim spaces silently, which is today's behaviour.
--
-- NULL embedding means detection ran but recognition did not; those rows keep
-- a NULL model_id so they are not falsely attributed to any model.
ALTER TABLE faces ADD COLUMN model_id TEXT;
UPDATE faces SET model_id = 'auraface-v1' WHERE embedding IS NOT NULL;

-- Detection runs carry the *detector*, which is a different model from the
-- recognizer above and changes independently of it.
ALTER TABLE face_detection_runs ADD COLUMN model_id TEXT;
UPDATE face_detection_runs SET model_id = 'yunet-2023mar';

-- Transcript runs carry the whisper model. Like faces, nothing in an existing
-- row identifies it, so the default (`small`) is assumed for the same reason.
ALTER TABLE transcript_runs ADD COLUMN model_id TEXT;
UPDATE transcript_runs SET model_id = 'whisper-small';

-- Pending-work scans filter on (asset, model_id), so index that direction.
-- The embeddings tables are already keyed by asset_id, and adding model_id to
-- their lookup keeps the anti-join index-served instead of falling back to a
-- scan once a library holds two models at once.
CREATE INDEX embeddings_model_idx       ON embeddings (model_id);
CREATE INDEX frame_embeddings_model_idx ON frame_embeddings (model_id);
CREATE INDEX faces_model_idx            ON faces (model_id) WHERE model_id IS NOT NULL;
