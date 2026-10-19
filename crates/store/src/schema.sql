-- Applied once per schema version; see MIGRATIONS in lib.rs.
CREATE TABLE repos (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE,
    path    TEXT NOT NULL,
    shallow INTEGER NOT NULL
);

CREATE TABLE versions (
    repo_id     INTEGER NOT NULL REFERENCES repos(id),
    name        TEXT NOT NULL,
    ord         INTEGER NOT NULL,
    head_sha    TEXT NOT NULL,
    fork_sha    TEXT NOT NULL,
    fork_fp_idx INTEGER NOT NULL,
    fork_source TEXT NOT NULL,
    PRIMARY KEY (repo_id, name)
) WITHOUT ROWID;

-- branch: the branch whose walk first reached the commit (older versions walk first).
-- fp_idx: index of the first-parent step that brought the commit in; sub_idx 0 is that step
-- itself, 1.. are the commits its merge introduced, oldest first.
-- version: the release the change first ships in (master commits map through fork points).
-- major: the X.0 release that includes it (saas-17.2 → 18.0), for major-to-major migrations.
CREATE TABLE commits (
    repo_id      INTEGER NOT NULL REFERENCES repos(id),
    sha          TEXT NOT NULL,
    branch       TEXT NOT NULL,
    fp_idx       INTEGER NOT NULL,
    sub_idx      INTEGER NOT NULL,
    version      TEXT,
    major        TEXT,
    parents      TEXT NOT NULL,
    is_merge     INTEGER NOT NULL,
    author_name  TEXT NOT NULL,
    author_email TEXT NOT NULL,
    authored_at  INTEGER NOT NULL,
    committed_at INTEGER NOT NULL,
    subject      TEXT NOT NULL,
    body         TEXT NOT NULL,
    tag          TEXT,
    modules      TEXT NOT NULL,
    task         INTEGER,
    opw          INTEGER,
    pr           TEXT,
    related_prs  TEXT NOT NULL,
    original_sha TEXT,
    PRIMARY KEY (repo_id, sha)
) WITHOUT ROWID;

CREATE INDEX commits_order ON commits (repo_id, branch, fp_idx, sub_idx);
CREATE INDEX commits_version ON commits (repo_id, version);
CREATE INDEX commits_major ON commits (repo_id, major);
CREATE INDEX commits_pr ON commits (pr);
CREATE INDEX commits_original ON commits (original_sha);

CREATE TABLE runs (
    repo_id  INTEGER NOT NULL REFERENCES repos(id),
    branch   TEXT NOT NULL,
    last_sha TEXT NOT NULL,
    last_fp  INTEGER NOT NULL,
    PRIMARY KEY (repo_id, branch)
) WITHOUT ROWID;
