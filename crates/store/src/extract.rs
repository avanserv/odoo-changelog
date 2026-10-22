use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::Store;

/// A non-merge commit walk recorded, with the extractors that already processed it.
pub struct PendingCommit {
    pub sha: String,
    pub version: Option<String>,
    pub done: Vec<String>,
}

/// `symbols` is `None` and `error` set for a blob the extractor failed on.
pub struct CachedBlob {
    pub symbols: Option<Vec<u8>>,
    pub error: Option<String>,
}

pub struct EventRow<'a> {
    pub sha: &'a str,
    pub path: &'a str,
    pub extractor: &'a str,
    pub symbol_id: &'a [u8],
    pub kind: &'a str,
    pub qualname: &'a str,
    pub op: &'a str,
    pub module: Option<&'a str>,
    pub line: i64,
    pub attrs_before: Option<&'a str>,
    pub attrs_after: Option<&'a str>,
    /// Where a moved or renamed symbol was; it keeps the kind of the new one.
    pub old: Option<OldSide<'a>>,
}

pub struct OldSide<'a> {
    pub symbol_id: &'a [u8],
    pub qualname: &'a str,
    pub path: &'a str,
    pub module: Option<&'a str>,
    /// A rename's confidence and the signals that matched; `None` for a move.
    pub rename: Option<(f64, &'a [String])>,
}

impl Store {
    pub fn repo_id(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT id FROM repos WHERE name = ?1", [name], |r| r.get(0))
            .optional()?)
    }

    /// Drops the events, marks and stale cache rows of every stored extractor that `current`
    /// lacks or has at another version, and the classes they took part in, then records
    /// `current`. Returns the names it purged.
    pub fn sync_extractors(&self, current: &[(&str, i64)]) -> Result<Vec<String>> {
        let stored: Vec<(String, i64)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT name, version FROM extractors ORDER BY name")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        if current.iter().any(|(n, _)| !stored.iter().any(|(s, _)| s == n)) {
            self.unclassify_all()?;
        }
        let mut purged = Vec::new();
        for (name, version) in stored {
            let now = current.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
            if now == Some(version) {
                continue;
            }
            self.unclassify_extracted_by(&name)?;
            self.conn.execute("DELETE FROM events WHERE extractor = ?1", [&name])?;
            self.conn
                .execute("DELETE FROM extracted WHERE extractor = ?1", [&name])?;
            self.conn.execute(
                "DELETE FROM blob_cache WHERE extractor = ?1 AND extractor_version IS NOT ?2",
                params![name, now],
            )?;
            self.conn.execute("DELETE FROM extractors WHERE name = ?1", [&name])?;
            purged.push(name);
        }
        if !purged.is_empty() {
            self.conn.execute(
                "DELETE FROM symbols WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.symbol_id = symbols.id)
                   AND NOT EXISTS (SELECT 1 FROM events e WHERE e.old_symbol_id = symbols.id)",
                [],
            )?;
        }
        for (name, version) in current {
            self.conn.execute(
                "INSERT INTO extractors (name, version) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET version = excluded.version",
                params![name, version],
            )?;
        }
        Ok(purged)
    }

    /// Every non-merge commit of the repo, in walk order: `(branch, fp_idx, sub_idx)`.
    pub fn extract_candidates(&self, repo_id: i64) -> Result<Vec<PendingCommit>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.sha, c.version,
                    (SELECT json_group_array(extractor) FROM
                        (SELECT e.extractor FROM extracted e
                          WHERE e.repo_id = c.repo_id AND e.sha = c.sha ORDER BY e.extractor))
               FROM commits c
              WHERE c.repo_id = ?1 AND c.is_merge = 0
              ORDER BY c.branch, c.fp_idx, c.sub_idx",
        )?;
        let rows = stmt.query_map([repo_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, String>(2)?))
        })?;
        rows.map(|row| {
            let (sha, version, done) = row?;
            Ok(PendingCommit {
                sha,
                version,
                done: serde_json::from_str(&done)?,
            })
        })
        .collect()
    }

    pub fn cached_blob(&self, oid: &str, extractor: &str, version: i64) -> Result<Option<CachedBlob>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT symbols, error FROM blob_cache
              WHERE blob_oid = ?1 AND extractor = ?2 AND extractor_version = ?3",
        )?;
        Ok(stmt
            .query_row(params![oid, extractor, version], |r| {
                Ok(CachedBlob {
                    symbols: r.get(0)?,
                    error: r.get(1)?,
                })
            })
            .optional()?)
    }

    pub fn insert_cached_blob(
        &self,
        oid: &str,
        extractor: &str,
        version: i64,
        symbols: Option<&[u8]>,
        error: Option<&str>,
    ) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO blob_cache (blob_oid, extractor, extractor_version, symbols, error)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        stmt.execute(params![oid, extractor, version, symbols, error])?;
        Ok(())
    }

    pub fn insert_events(&self, repo_id: i64, rows: &[EventRow]) -> Result<()> {
        let mut symbol = self
            .conn
            .prepare_cached("INSERT OR IGNORE INTO symbols (id, kind, qualname) VALUES (?1, ?2, ?3)")?;
        let mut event = self.conn.prepare_cached(
            "INSERT INTO events (repo_id, sha, path, extractor, symbol_id, op, module, line,
                attrs_before, attrs_after, old_symbol_id, old_path, old_module, confidence, signals)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for e in rows {
            let rename = e.old.as_ref().and_then(|o| o.rename);
            let signals = rename.map(|(_, s)| serde_json::to_string(s)).transpose()?;
            symbol.execute(params![e.symbol_id, e.kind, e.qualname])?;
            if let Some(old) = &e.old {
                symbol.execute(params![old.symbol_id, e.kind, old.qualname])?;
            }
            event.execute(params![
                repo_id,
                e.sha,
                e.path,
                e.extractor,
                e.symbol_id,
                e.op,
                e.module,
                e.line,
                e.attrs_before,
                e.attrs_after,
                e.old.as_ref().map(|o| o.symbol_id),
                e.old.as_ref().map(|o| o.path),
                e.old.as_ref().and_then(|o| o.module),
                rename.map(|(c, _)| c),
                signals
            ])?;
        }
        Ok(())
    }

    /// Clears the commit's classes, since its events may have changed.
    pub fn mark_extracted(&self, repo_id: i64, sha: &str, extractors: &[&str]) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare_cached("INSERT INTO extracted (repo_id, sha, extractor) VALUES (?1, ?2, ?3)")?;
        for extractor in extractors {
            stmt.execute(params![repo_id, sha, extractor])?;
        }
        self.unclassify_commit(repo_id, sha)
    }
}
