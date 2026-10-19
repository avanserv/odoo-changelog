//! SQLite persistence. Writers take plain data so the store does not depend on git types.

use std::path::Path;

use anyhow::{Context, Result};
use odoo_changelog_core::CommitMeta;
use rusqlite::{Connection, OptionalExtension, params};

const MIGRATIONS: &[&str] = &[include_str!("schema.sql")];

pub struct Store {
    pub conn: Connection,
}

pub struct VersionRow<'a> {
    pub name: &'a str,
    pub ord: i64,
    pub head_sha: &'a str,
    pub fork_sha: &'a str,
    pub fork_fp_idx: i64,
    pub fork_source: &'a str,
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

    pub fn upsert_repo(&self, name: &str, path: &str, shallow: bool) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO repos (name, path, shallow) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET path = excluded.path, shallow = excluded.shallow",
            params![name, path, shallow],
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
}
