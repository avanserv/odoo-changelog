//! SQLite persistence. Writers take plain data so the store does not depend on git types.

mod classify;
mod extract;
mod matches;
mod upgrade;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use odoo_changelog_core::CommitMeta;
use rusqlite::{Connection, OptionalExtension, params};

pub use classify::{ClassRow, StoredEvent, Unclassified};
pub use extract::{CachedBlob, EventRow, OldSide, PendingCommit, RenameRow};
pub use matches::{MatchRow, StoredRename, StoredUpgradeEvent};
pub use upgrade::UpgradeEventRow;

const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    // The clone path depends on the machine and on how the config path was spelled.
    "ALTER TABLE repos DROP COLUMN path;",
    include_str!("extract.sql"),
    include_str!("moves.sql"),
    include_str!("renames.sql"),
    include_str!("classify.sql"),
    include_str!("referrers.sql"),
    include_str!("similarity.sql"),
    include_str!("upgrade.sql"),
    include_str!("matches.sql"),
];

pub struct Store {
    conn: Connection,
}

pub struct VersionRow<'a> {
    pub name: &'a str,
    pub ord: i64,
    pub head_sha: &'a str,
    pub fork_sha: &'a str,
    pub fork_fp_idx: i64,
    pub fork_source: &'a str,
}

pub struct VersionStats {
    pub repo: String,
    /// `?` for commits no version claims yet.
    pub version: String,
    pub commits: i64,
    pub merges: i64,
}

pub struct CommitRow<'a> {
    pub sha: &'a str,
    pub branch: &'a str,
    pub fp_idx: i64,
    pub sub_idx: i64,
    pub parents: &'a [String],
    pub author_name: &'a str,
    pub author_email: &'a str,
    pub authored_at: i64,
    pub committed_at: i64,
    pub message: &'a str,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let current: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", i as i64 + 1)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Runs `f` in one transaction, committed only when `f` succeeds.
    pub fn in_transaction<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let out = f()?;
        tx.commit()?;
        Ok(out)
    }

    pub fn upsert_repo(&self, name: &str, shallow: bool) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO repos (name, shallow) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET shallow = excluded.shallow",
            params![name, shallow],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM repos WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn replace_versions(&self, repo_id: i64, rows: &[VersionRow]) -> Result<()> {
        self.conn
            .execute("DELETE FROM versions WHERE repo_id = ?1", [repo_id])?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO versions (repo_id, name, ord, head_sha, fork_sha, fork_fp_idx, fork_source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for v in rows {
            stmt.execute(params![
                repo_id,
                v.name,
                v.ord,
                v.head_sha,
                v.fork_sha,
                v.fork_fp_idx,
                v.fork_source
            ])?;
        }
        Ok(())
    }

    /// Version name → fork sha recorded by the last walk (empty when unrelated).
    pub fn version_forks(&self, repo_id: i64) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, fork_sha FROM versions WHERE repo_id = ?1")?;
        let rows = stmt.query_map([repo_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn run_state(&self, repo_id: i64, branch: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT last_sha, last_fp FROM runs WHERE repo_id = ?1 AND branch = ?2",
                params![repo_id, branch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn set_run_state(&self, repo_id: i64, branch: &str, sha: &str, fp: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO runs (repo_id, branch, last_sha, last_fp) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repo_id, branch) DO UPDATE SET last_sha = excluded.last_sha, last_fp = excluded.last_fp",
            params![repo_id, branch, sha, fp],
        )?;
        Ok(())
    }

    /// Inserts a commit unless an earlier walk already claimed it; returns whether it was new.
    pub fn insert_commit(&self, repo_id: i64, c: &CommitRow) -> Result<bool> {
        let meta = CommitMeta::parse(c.message);
        let (subject, body) = c.message.split_once('\n').unwrap_or((c.message, ""));
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR IGNORE INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, tag, modules,
                task, opw, pr, related_prs, original_sha)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        )?;
        let n = stmt.execute(params![
            repo_id,
            c.sha,
            c.branch,
            c.fp_idx,
            c.sub_idx,
            serde_json::to_string(c.parents)?,
            c.parents.len() > 1,
            c.author_name,
            c.author_email,
            c.authored_at,
            c.committed_at,
            subject.trim_end(),
            body.trim(),
            meta.tag,
            serde_json::to_string(&meta.modules)?,
            meta.task.map(|t| t as i64),
            meta.opw.map(|t| t as i64),
            meta.pr,
            serde_json::to_string(&meta.related_prs)?,
            meta.original_sha,
        ])?;
        Ok(n == 1)
    }

    /// The branch whose walk recorded `sha`.
    pub fn commit_branch(&self, repo_id: i64, sha: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT branch FROM commits WHERE repo_id = ?1 AND sha = ?2")?;
        Ok(stmt.query_row(params![repo_id, sha], |r| r.get(0)).optional()?)
    }

    /// Recomputes the release of every commit from the current fork table: a stable branch
    /// commit belongs to its branch; a master commit to the first version forked after it.
    pub fn assign_versions(&self, repo_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE commits SET version = branch WHERE repo_id = ?1 AND branch != 'master'",
            [repo_id],
        )?;
        self.conn.execute(
            "UPDATE commits SET version = COALESCE(
                (SELECT v.name FROM versions v
                  WHERE v.repo_id = commits.repo_id AND v.fork_fp_idx >= commits.fp_idx
                  ORDER BY v.ord LIMIT 1),
                'master')
             WHERE repo_id = ?1 AND branch = 'master'",
            [repo_id],
        )?;
        self.conn.execute(
            "UPDATE commits SET major = CASE
                WHEN version LIKE 'saas-%'
                THEN (CAST(substr(version, 6, instr(substr(version, 6), '.') - 1) AS INTEGER) + 1) || '.0'
                ELSE version END
             WHERE repo_id = ?1",
            [repo_id],
        )?;
        Ok(())
    }

    /// Commit counts per repo and version, versions in release order.
    pub fn stats(&self) -> Result<Vec<VersionStats>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.name, COALESCE(c.version, '?'), COUNT(*), SUM(c.is_merge)
               FROM commits c
               JOIN repos r ON r.id = c.repo_id
               LEFT JOIN versions v ON v.repo_id = c.repo_id AND v.name = c.version
              GROUP BY r.name, c.version
              ORDER BY r.name, COALESCE(v.ord, 1e9)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(VersionStats {
                repo: r.get(0)?,
                version: r.get(1)?,
                commits: r.get(2)?,
                merges: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_v1_without_losing_commits() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, name, path, shallow) VALUES (7, 'odoo', '/somewhere/odoo', 0);
             INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, modules,
                related_prs)
             VALUES (7, 'abc', 'master', 0, 0, '[]', 0, 'a', 'a@x', 0, 0, 's', '', '[]', '[]');",
        )
        .unwrap();

        let store = Store { conn };
        store.migrate().unwrap();

        let version: i64 = store.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let commits: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM commits WHERE repo_id = 7", [], |r| r.get(0))
            .unwrap();
        assert_eq!(commits, 1);
        let has_path: bool = store
            .conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('repos') WHERE name = 'path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!has_path);
        assert_eq!(store.upsert_repo("odoo", false).unwrap(), 7);
    }

    #[test]
    fn migrates_v2_adds_extract_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, name, shallow) VALUES (7, 'odoo', 0);
             INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, modules,
                related_prs)
             VALUES (7, 'abc', 'master', 0, 0, '[]', 0, 'a', 'a@x', 0, 0, 's', '', '[]', '[]');",
        )
        .unwrap();

        let store = Store { conn };
        store.migrate().unwrap();

        let commits: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM commits WHERE repo_id = 7", [], |r| r.get(0))
            .unwrap();
        assert_eq!(commits, 1);
        for table in ["extractors", "symbols", "events", "extracted", "blob_cache"] {
            let n: i64 = store
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table}");
        }
        let candidates = store.extract_candidates(7).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].done.is_empty());
    }

    #[test]
    fn migrates_v3_rebuilds_events_and_keeps_the_cache() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, name, shallow) VALUES (7, 'odoo', 0);
             INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, modules,
                related_prs)
             VALUES (7, 'abc', 'master', 0, 0, '[]', 0, 'a', 'a@x', 0, 0, 's', '', '[]', '[]');
             INSERT INTO extractors (name, version) VALUES ('py', 1);
             INSERT INTO symbols (id, kind, qualname) VALUES (x'01', 'def', 'f');
             INSERT INTO events (repo_id, sha, path, extractor, symbol_id, op, line, attrs_after)
             VALUES (7, 'abc', 'f.py', 'py', x'01', 'added', 1, '{}');
             INSERT INTO extracted (repo_id, sha, extractor) VALUES (7, 'abc', 'py');
             INSERT INTO blob_cache (blob_oid, extractor, extractor_version, symbols)
             VALUES ('b1', 'py', 1, x'00');",
        )
        .unwrap();

        let store = Store { conn };
        store.migrate().unwrap();

        for (table, n) in [
            ("events", 0),
            ("extracted", 0),
            ("symbols", 0),
            ("blob_cache", 1),
            ("extractors", 1),
        ] {
            assert_eq!(count(&store, &format!("SELECT COUNT(*) FROM {table}")), n, "{table}");
        }
        let columns = count(
            &store,
            "SELECT COUNT(*) FROM pragma_table_info('events')
              WHERE name IN ('module', 'old_symbol_id', 'old_path', 'old_module')",
        );
        assert_eq!(columns, 4);
        assert!(store.extract_candidates(7).unwrap()[0].done.is_empty());
    }

    #[test]
    fn migrates_v4_rebuilds_events_with_rename_columns() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, name, shallow) VALUES (7, 'odoo', 0);
             INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, modules,
                related_prs)
             VALUES (7, 'abc', 'master', 0, 0, '[]', 0, 'a', 'a@x', 0, 0, 's', '', '[]', '[]');
             INSERT INTO extractors (name, version) VALUES ('py', 1);
             INSERT INTO symbols (id, kind, qualname) VALUES (x'01', 'def', 'f');
             INSERT INTO events (repo_id, sha, path, extractor, symbol_id, op, line, attrs_after)
             VALUES (7, 'abc', 'f.py', 'py', x'01', 'added', 1, '{}');
             INSERT INTO extracted (repo_id, sha, extractor) VALUES (7, 'abc', 'py');
             INSERT INTO blob_cache (blob_oid, extractor, extractor_version, symbols)
             VALUES ('b1', 'py', 1, x'00');",
        )
        .unwrap();

        let store = Store { conn };
        store.migrate().unwrap();

        for (table, n) in [
            ("events", 0),
            ("extracted", 0),
            ("symbols", 0),
            ("blob_cache", 1),
            ("extractors", 1),
        ] {
            assert_eq!(count(&store, &format!("SELECT COUNT(*) FROM {table}")), n, "{table}");
        }
        let columns = count(
            &store,
            "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name IN ('confidence', 'signals')",
        );
        assert_eq!(columns, 2);
        assert!(store.extract_candidates(7).unwrap()[0].done.is_empty());
    }

    #[test]
    fn migrates_v5_adds_class_columns_and_keeps_events() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, name, shallow) VALUES (7, 'odoo', 0);
             INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, modules,
                related_prs)
             VALUES (7, 'abc', 'master', 0, 0, '[]', 0, 'a', 'a@x', 0, 0, 's', '', '[]', '[]');
             INSERT INTO symbols (id, kind, qualname) VALUES (x'01', 'def', 'f');
             INSERT INTO events (repo_id, sha, path, extractor, symbol_id, op, line, attrs_after)
             VALUES (7, 'abc', 'f.py', 'py', x'01', 'added', 1, '{}');
             INSERT INTO extracted (repo_id, sha, extractor) VALUES (7, 'abc', 'py');",
        )
        .unwrap();

        // Only this migration: a later one rebuilds the events.
        conn.execute_batch(MIGRATIONS[5]).unwrap();
        let store = Store { conn };

        assert_eq!(count(&store, "SELECT COUNT(*) FROM events WHERE category IS NULL"), 1);
        let columns = "SELECT COUNT(*) FROM pragma_table_info(?1)
                        WHERE name IN ('category', 'breaking', 'rule_id', 'classified')";
        for (table, n) in [("events", 3), ("commits", 4)] {
            let found: i64 = store.conn.query_row(columns, [table], |r| r.get(0)).unwrap();
            assert_eq!(found, n, "{table}");
        }
        let unclassified = store.classify_candidates(7, 1, 1).unwrap();
        assert_eq!(unclassified.len(), 1);
    }

    #[test]
    fn migrates_v6_rebuilds_events_with_referrers_and_keeps_the_cache() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute_batch(
            "INSERT INTO repos (id, name, shallow) VALUES (7, 'odoo', 0);
             INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, parents, is_merge,
                author_name, author_email, authored_at, committed_at, subject, body, modules,
                related_prs, category, breaking, rule_id, classified)
             VALUES (7, 'abc', 'master', 0, 0, '[]', 0, 'a', 'a@x', 0, 0, 's', '', '[]', '[]',
                'technical', 0, 'data', 1);
             INSERT INTO symbols (id, kind, qualname) VALUES (x'01', 'def', 'f');
             INSERT INTO events (repo_id, sha, path, extractor, symbol_id, op, line, attrs_after)
             VALUES (7, 'abc', 'f.py', 'py', x'01', 'added', 1, '{}');
             INSERT INTO extracted (repo_id, sha, extractor) VALUES (7, 'abc', 'py');
             INSERT INTO extractors (name, version) VALUES ('py', 1);
             INSERT INTO blob_cache (blob_oid, extractor, extractor_version, symbols)
             VALUES ('b1', 'py', 1, x'5b5d');",
        )
        .unwrap();

        let store = Store { conn };
        store.migrate().unwrap();

        for (table, n) in [("events", 0), ("extracted", 0), ("symbols", 0), ("blob_cache", 1)] {
            assert_eq!(count(&store, &format!("SELECT COUNT(*) FROM {table}")), n, "{table}");
        }
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM commits WHERE classified IS NULL"),
            1
        );
        let column = "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name = 'referrers'";
        assert_eq!(count(&store, column), 1);
        assert!(store.extract_candidates(7).unwrap()[0].done.is_empty());
    }

    #[test]
    fn classes_are_written_and_dropped_when_events_change() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let repo_id = store.upsert_repo("odoo", false).unwrap();
        let commit = CommitRow {
            sha: "abc",
            branch: "master",
            fp_idx: 0,
            sub_idx: 0,
            parents: &[],
            author_name: "a",
            author_email: "a@x",
            authored_at: 0,
            committed_at: 0,
            message: "[FIX] sale: x",
        };
        store.insert_commit(repo_id, &commit).unwrap();
        store.sync_extractors(&[("py", 1), ("xml", 1)]).unwrap();
        store
            .insert_events(
                repo_id,
                &[EventRow {
                    sha: "abc",
                    path: "f",
                    extractor: "py",
                    symbol_id: &[1u8; 32],
                    kind: "def",
                    qualname: "f",
                    op: "added",
                    module: None,
                    line: 1,
                    attrs_before: None,
                    attrs_after: Some("{}"),
                    old: None,
                }],
            )
            .unwrap();
        store.mark_extracted(repo_id, "abc", &["py"]).unwrap();
        assert!(
            store.classify_candidates(repo_id, 1, 2).unwrap().is_empty(),
            "xml is pending"
        );
        store.mark_extracted(repo_id, "abc", &["xml"]).unwrap();

        let candidates = store.classify_candidates(repo_id, 1, 2).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tag.as_deref(), Some("FIX"));
        let events = store.commit_events(repo_id, "abc").unwrap();
        assert_eq!((events[0].kind.as_str(), events[0].op.as_str()), ("def", "added"));
        let class = |rule_id| ClassRow {
            category: Some("technical"),
            breaking: true,
            rule_id,
        };
        store
            .set_classes(
                repo_id,
                "abc",
                1,
                &[(&events[0], class("removed"))],
                class("commit-majority"),
            )
            .unwrap();
        assert!(store.classify_candidates(repo_id, 1, 2).unwrap().is_empty());
        assert_eq!(
            store.classify_candidates(repo_id, 2, 2).unwrap().len(),
            1,
            "a new version reclassifies"
        );
        let row = "SELECT category || ' ' || breaking || ' ' || rule_id FROM events";
        let written: String = store.conn.query_row(row, [], |r| r.get(0)).unwrap();
        assert_eq!(written, "technical 1 removed");

        let classify = || {
            store
                .set_classes(
                    repo_id,
                    "abc",
                    1,
                    &[(&events[0], class("removed"))],
                    class("commit-majority"),
                )
                .unwrap()
        };
        let cleared = |store: &Store| {
            let commits = "SELECT COUNT(*) FROM commits
                            WHERE category IS NULL AND breaking IS NULL AND rule_id IS NULL AND classified IS NULL";
            let events = "SELECT COUNT(*) FROM events WHERE category IS NULL AND breaking IS NULL AND rule_id IS NULL";
            count(store, commits) == 1 && count(store, events) == count(store, "SELECT COUNT(*) FROM events")
        };

        // A new extractor has processed no commit yet.
        store.sync_extractors(&[("js", 1), ("py", 1), ("xml", 1)]).unwrap();
        assert!(cleared(&store), "adding js left classes");
        store.sync_extractors(&[("py", 1), ("xml", 1)]).unwrap();
        classify();
        // Purging xml leaves py's event, whose class was computed beside xml's.
        store.sync_extractors(&[("py", 1), ("xml", 2)]).unwrap();
        assert_eq!(count(&store, "SELECT COUNT(*) FROM events"), 1);
        assert!(cleared(&store), "purging xml left classes");
        store.mark_extracted(repo_id, "abc", &["xml"]).unwrap();
        classify();
        store.mark_extracted(repo_id, "abc", &["js"]).unwrap();
        assert!(cleared(&store), "a new mark left classes");
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store.conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn sync_extractors_purges_only_stale() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let repo_id = store.upsert_repo("odoo", false).unwrap();
        store
            .insert_commit(
                repo_id,
                &CommitRow {
                    sha: "abc",
                    branch: "master",
                    fp_idx: 0,
                    sub_idx: 0,
                    parents: &[],
                    author_name: "a",
                    author_email: "a@x",
                    authored_at: 0,
                    committed_at: 0,
                    message: "s",
                },
            )
            .unwrap();
        assert!(store.sync_extractors(&[("py", 1), ("xml", 1)]).unwrap().is_empty());
        // The xml event is a rename, whose old symbol outlives a purge of py too.
        let signals = ["type".to_string(), "refs".to_string()];
        let referrers = ["xml-caller".to_string()];
        let old = OldSide {
            symbol_id: &[3u8; 32],
            kind: "rule",
            qualname: "xml-old",
            path: "g",
            module: None,
            rename: Some(RenameRow {
                confidence: 0.75,
                signals: &signals,
                referrers: &referrers,
                similarity: Some(0.8),
            }),
        };
        for (extractor, id, old) in [("py", [1u8; 32], None), ("xml", [2u8; 32], Some(old))] {
            store.insert_cached_blob("b1", extractor, 1, Some(b"[]"), None).unwrap();
            store
                .insert_events(
                    repo_id,
                    &[EventRow {
                        sha: "abc",
                        path: "f",
                        extractor,
                        symbol_id: &id,
                        kind: "def",
                        qualname: extractor,
                        op: if old.is_some() { "renamed" } else { "added" },
                        module: None,
                        line: 1,
                        attrs_before: None,
                        attrs_after: Some("{}"),
                        old,
                    }],
                )
                .unwrap();
            store.mark_extracted(repo_id, "abc", &[extractor]).unwrap();
        }

        let rename: (f64, String, String, f64) = store
            .conn
            .query_row(
                "SELECT confidence, signals, referrers, similarity FROM events WHERE op = 'renamed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            rename,
            (
                0.75,
                r#"["type","refs"]"#.to_string(),
                r#"["xml-caller"]"#.to_string(),
                0.8
            )
        );
        let old_kind: String = store
            .conn
            .query_row("SELECT kind FROM symbols WHERE qualname = 'xml-old'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(old_kind, "rule");

        assert_eq!(store.sync_extractors(&[("py", 2), ("xml", 1)]).unwrap(), ["py"]);
        for table in ["events", "extracted", "blob_cache"] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE extractor = 'xml'");
            assert_eq!(count(&store, &sql), 1, "{table}");
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE extractor = 'py'");
            assert_eq!(count(&store, &sql), 0, "{table}");
        }
        assert_eq!(count(&store, "SELECT COUNT(*) FROM symbols"), 2);
        assert_eq!(count(&store, "SELECT version FROM extractors WHERE name = 'py'"), 2);
        assert_eq!(store.extract_candidates(repo_id).unwrap()[0].done, ["xml"]);

        assert_eq!(store.sync_extractors(&[("py", 2)]).unwrap(), ["xml"]);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM blob_cache"), 0);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM symbols"), 0);
    }
}
