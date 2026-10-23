-- Each detected rename an upgrade event confirms, keyed by the upgrade event and then the event.
-- detected_confidence: the rename detector's own confidence, which the event's confidence
-- replaces with 1 while the match holds.
CREATE TABLE upgrade_matches (
    path                TEXT NOT NULL,
    line                INTEGER NOT NULL,
    col                 INTEGER NOT NULL,
    seq                 INTEGER NOT NULL,
    repo_id             INTEGER NOT NULL,
    sha                 TEXT NOT NULL,
    event_path          TEXT NOT NULL,
    extractor           TEXT NOT NULL,
    symbol_id           BLOB NOT NULL,
    detected_confidence REAL NOT NULL,
    PRIMARY KEY (path, line, col, seq, repo_id, sha, event_path, extractor, symbol_id)
) WITHOUT ROWID;
CREATE INDEX upgrade_matches_by_event ON upgrade_matches (repo_id, sha, event_path, extractor, symbol_id);
CREATE INDEX events_renamed ON events (op) WHERE op = 'renamed';

-- repo_id, sha: the earliest committed of the commits an upgrade event matched, NULL when it
-- matched none.
ALTER TABLE upgrade_events ADD COLUMN repo_id INTEGER;
ALTER TABLE upgrade_events ADD COLUMN sha TEXT;
