//! SQLite persistence. Writers take plain data so the store does not depend on git types.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use odoo_changelog_core::CommitMeta;
use rusqlite::{Connection, OptionalExtension, params};

const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    // The clone path depends on the machine and on how the config path was spelled.
    "ALTER TABLE repos DROP COLUMN path;",
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
}
