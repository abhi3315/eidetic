-- Fingerprint of the last completed face-clustering run.
--
-- Clustering rebuilds an all-pairs similarity graph over every leftover
-- face, and a library's leftovers (background strangers, low-confidence
-- crops) only grow — so `eidetic faces` was paying the full quadratic cost
-- on every run even when nothing had changed since the last one.
--
-- One row records a hash of everything the outcome depends on: the face set
-- with its assignments and pins, the user's constraints, the parameters, and
-- the recognition model. Same fingerprint = same result, so the run is
-- skipped. Any new face, name, link, rejection, or model change produces a
-- different fingerprint and re-runs everything — results can never differ,
-- only repeated work disappears.
CREATE TABLE cluster_state (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    fingerprint  TEXT NOT NULL,
    clustered_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
