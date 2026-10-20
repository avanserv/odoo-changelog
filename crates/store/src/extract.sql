-- Extractor versions the stored events, marks and cache rows were made with.
CREATE TABLE extractors (
    name    TEXT NOT NULL PRIMARY KEY,
    version INTEGER NOT NULL
) WITHOUT ROWID;

-- id: blake3(kind, 0x00, qualname).
CREATE TABLE symbols (
    id       BLOB NOT NULL PRIMARY KEY,
    kind     TEXT NOT NULL,
    qualname TEXT NOT NULL
) WITHOUT ROWID;

-- One symbol change in one file of a non-merge commit, against its parent.
-- op: added, removed, changed (attrs differ) or moved (same attrs, renamed file).
-- path: the new path, or the old one for a deletion; old_path is set when a rename changed it.
CREATE TABLE events (
    repo_id      INTEGER NOT NULL,
    sha          TEXT NOT NULL,
    path         TEXT NOT NULL,
    extractor    TEXT NOT NULL,
    symbol_id    BLOB NOT NULL REFERENCES symbols(id),
    op           TEXT NOT NULL,
    old_path     TEXT,
    line         INTEGER NOT NULL,
    attrs_before TEXT,
    attrs_after  TEXT,
    PRIMARY KEY (repo_id, sha, path, extractor, symbol_id),
    FOREIGN KEY (repo_id, sha) REFERENCES commits(repo_id, sha)
) WITHOUT ROWID;
CREATE INDEX events_symbol ON events (symbol_id);

-- Commits each extractor has processed, so one that yielded no events is not extracted again.
CREATE TABLE extracted (
    repo_id   INTEGER NOT NULL,
    sha       TEXT NOT NULL,
    extractor TEXT NOT NULL,
    PRIMARY KEY (repo_id, sha, extractor),
    FOREIGN KEY (repo_id, sha) REFERENCES commits(repo_id, sha)
) WITHOUT ROWID;

-- What an extractor version made of a blob. symbols is NULL and error set when the blob failed
-- to parse, so it is not parsed again.
CREATE TABLE blob_cache (
    blob_oid          TEXT NOT NULL,
    extractor         TEXT NOT NULL,
    extractor_version INTEGER NOT NULL,
    symbols           BLOB,
    error             TEXT,
    PRIMARY KEY (blob_oid, extractor, extractor_version)
) WITHOUT ROWID;
