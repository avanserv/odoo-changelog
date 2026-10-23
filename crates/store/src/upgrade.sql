-- The upgrade repository commit the events were read from, and the parser's version.
CREATE TABLE upgrade_runs (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    sha TEXT NOT NULL,
    parser_version INTEGER NOT NULL
);
-- One row per rename, move, merge or removal an upgrade script declares. seq numbers the events of
-- one call, which a loop or a brace expansion runs several times. version is the branch the
-- script's folder upgrades to, NULL for 0.0.0, which runs on every upgrade. A partial event has
-- an argument that did not resolve: its field is NULL and source holds the call.
CREATE TABLE upgrade_events (
    path TEXT NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    module TEXT NOT NULL,
    folder TEXT NOT NULL,
    version TEXT,
    phase TEXT,                 -- pre, post, end, init; NULL for a helper module
    call TEXT NOT NULL,
    kind TEXT NOT NULL,         -- field, model, xmlid, view, module
    op TEXT NOT NULL,           -- renamed, moved, merged, removed
    model TEXT,
    old TEXT,
    new TEXT,
    old_module TEXT,
    new_module TEXT,
    partial INTEGER NOT NULL,
    source TEXT,
    PRIMARY KEY (path, line, col, seq)
) WITHOUT ROWID;
CREATE INDEX upgrade_events_by_name ON upgrade_events (kind, op, old, new);
