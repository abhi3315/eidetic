-- Speech transcripts for videos (goals-v0.5.md #4).
--
-- Whisper output, one row per spoken segment with its time span. Transcripts
-- are RECALL FUEL, not display text: casual home-video audio (noise, code-
-- switched speech) transcribes imperfectly, so segments feed search and jump
-- links but are never rendered as subtitles.
--
-- transcript_runs marks "transcription ran" per asset (same idempotency
-- contract as face_detection_runs) — a silent or speechless video legitimately
-- produces zero transcript rows.

CREATE TABLE transcripts (
    asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    ts_secs  REAL NOT NULL,
    end_secs REAL NOT NULL,
    text     TEXT NOT NULL
);

CREATE INDEX transcripts_asset_idx ON transcripts(asset_id);

-- External-content FTS5 over the segment text, kept in sync by triggers.
CREATE VIRTUAL TABLE transcripts_fts USING fts5(
    text,
    content='transcripts',
    content_rowid='rowid'
);

CREATE TRIGGER transcripts_ai AFTER INSERT ON transcripts BEGIN
    INSERT INTO transcripts_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER transcripts_ad AFTER DELETE ON transcripts BEGIN
    INSERT INTO transcripts_fts(transcripts_fts, rowid, text)
    VALUES ('delete', old.rowid, old.text);
END;

CREATE TABLE transcript_runs (
    asset_id      TEXT PRIMARY KEY NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    segment_count INTEGER NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
