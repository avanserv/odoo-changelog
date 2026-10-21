-- Events are rebuilt from the blob cache: moves now pair across files and modules, and a change
-- keeps only the attrs that differ.
DROP TABLE events;
DELETE FROM extracted;
DELETE FROM symbols;

-- One symbol change in one non-merge commit, against its parent, as one extractor sees it.
-- op: added, removed, changed (same file, attrs differ) or moved (another file than before).
-- path and module: the new side's, or the old side's for a removal. module is NULL outside one.
-- attrs_before, attrs_after: the whole side for added and removed; for changed and moved, the
-- entries whose value differs, down into nested objects, both NULL for a move with equal attrs.
-- old_symbol_id, old_path, old_module: where a moved symbol was; NULL for every other op.
CREATE TABLE events (
    repo_id       INTEGER NOT NULL,
    sha           TEXT NOT NULL,
    path          TEXT NOT NULL,
    extractor     TEXT NOT NULL,
    symbol_id     BLOB NOT NULL REFERENCES symbols(id),
    op            TEXT NOT NULL,
    module        TEXT,
    line          INTEGER NOT NULL,
    attrs_before  TEXT,
    attrs_after   TEXT,
    old_symbol_id BLOB REFERENCES symbols(id),
    old_path      TEXT,
    old_module    TEXT,
    PRIMARY KEY (repo_id, sha, path, extractor, symbol_id),
    FOREIGN KEY (repo_id, sha) REFERENCES commits(repo_id, sha)
) WITHOUT ROWID;
CREATE INDEX events_symbol ON events (symbol_id);
CREATE INDEX events_old_symbol ON events (old_symbol_id) WHERE old_symbol_id IS NOT NULL;
