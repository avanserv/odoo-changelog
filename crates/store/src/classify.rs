use anyhow::Result;
use rusqlite::params;

use crate::Store;

/// A commit whose classes are missing or were written by another classifier version.
pub struct Unclassified {
    pub sha: String,
    pub tag: Option<String>,
}

/// One event of a commit, keyed as `events` keys it, with what classification reads.
pub struct StoredEvent {
    pub path: String,
    pub extractor: String,
    pub symbol_id: Vec<u8>,
    pub kind: String,
    pub qualname: String,
    pub op: String,
    pub module: Option<String>,
    pub old_module: Option<String>,
    pub attrs_before: Option<String>,
    pub attrs_after: Option<String>,
}

pub struct ClassRow<'a> {
    pub category: Option<&'a str>,
    pub breaking: bool,
    pub rule_id: &'a str,
}

const CLEAR_EVENTS: &str = "UPDATE events SET category = NULL, breaking = NULL, rule_id = NULL";
const CLEAR_COMMITS: &str = "UPDATE commits SET category = NULL, breaking = NULL, rule_id = NULL, classified = NULL";

impl Store {
    /// Clears the classes of the commits `extractor` processed and of their events: an event's
    /// class depends on the other events of its commit.
    pub(crate) fn unclassify_extracted_by(&self, extractor: &str) -> Result<()> {
        let processed = "EXISTS (SELECT 1 FROM extracted x
                                  WHERE x.repo_id = {t}.repo_id AND x.sha = {t}.sha AND x.extractor = ?1)";
        for (clear, table) in [(CLEAR_EVENTS, "events"), (CLEAR_COMMITS, "commits")] {
            let sql = format!("{clear} WHERE {}", processed.replace("{t}", table));
            self.conn.execute(&sql, [extractor])?;
        }
        Ok(())
    }

    /// Clears every class, for an extractor no commit has been processed by yet.
    pub(crate) fn unclassify_all(&self) -> Result<()> {
        self.conn
            .execute(&format!("{CLEAR_EVENTS} WHERE rule_id IS NOT NULL"), [])?;
        self.conn
            .execute(&format!("{CLEAR_COMMITS} WHERE classified IS NOT NULL"), [])?;
        Ok(())
    }

    pub(crate) fn unclassify_commit(&self, repo_id: i64, sha: &str) -> Result<()> {
        for clear in [CLEAR_EVENTS, CLEAR_COMMITS] {
            self.conn
                .prepare_cached(&format!("{clear} WHERE repo_id = ?1 AND sha = ?2"))?
                .execute(params![repo_id, sha])?;
        }
        Ok(())
    }

    /// Commits every one of the `extractors` current extractors has processed and
    /// that `version` has not classified, in walk order. `sync_extractors` drops the marks of
    /// any other extractor, so counting them is enough.
    pub fn classify_candidates(&self, repo_id: i64, version: i64, extractors: usize) -> Result<Vec<Unclassified>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.sha, c.tag FROM commits c
              WHERE c.repo_id = ?1 AND c.classified IS NOT ?2
                AND (SELECT COUNT(*) FROM extracted e WHERE e.repo_id = c.repo_id AND e.sha = c.sha) = ?3
              ORDER BY c.branch, c.fp_idx, c.sub_idx",
        )?;
        let rows = stmt.query_map(params![repo_id, version, extractors as i64], |r| {
            Ok(Unclassified {
                sha: r.get(0)?,
                tag: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// In key order.
    pub fn commit_events(&self, repo_id: i64, sha: &str) -> Result<Vec<StoredEvent>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT e.path, e.extractor, e.symbol_id, s.kind, s.qualname, e.op, e.module, e.old_module,
                    e.attrs_before, e.attrs_after
               FROM events e JOIN symbols s ON s.id = e.symbol_id
              WHERE e.repo_id = ?1 AND e.sha = ?2
              ORDER BY e.path, e.extractor, e.symbol_id",
        )?;
        let rows = stmt.query_map(params![repo_id, sha], |r| {
            Ok(StoredEvent {
                path: r.get(0)?,
                extractor: r.get(1)?,
                symbol_id: r.get(2)?,
                kind: r.get(3)?,
                qualname: r.get(4)?,
                op: r.get(5)?,
                module: r.get(6)?,
                old_module: r.get(7)?,
                attrs_before: r.get(8)?,
                attrs_after: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Writes the classes of a commit's events and of the commit, marking it classified by
    /// `version`.
    pub fn set_classes(
        &self,
        repo_id: i64,
        sha: &str,
        version: i64,
        events: &[(&StoredEvent, ClassRow)],
        commit: ClassRow,
    ) -> Result<()> {
        let mut event = self.conn.prepare_cached(
            "UPDATE events SET category = ?1, breaking = ?2, rule_id = ?3
              WHERE repo_id = ?4 AND sha = ?5 AND path = ?6 AND extractor = ?7 AND symbol_id = ?8",
        )?;
        for (e, class) in events {
            event.execute(params![
                class.category,
                class.breaking,
                class.rule_id,
                repo_id,
                sha,
                e.path,
                e.extractor,
                e.symbol_id
            ])?;
        }
        self.conn
            .prepare_cached(
                "UPDATE commits SET category = ?1, breaking = ?2, rule_id = ?3, classified = ?4
                  WHERE repo_id = ?5 AND sha = ?6",
            )?
            .execute(params![
                commit.category,
                commit.breaking,
                commit.rule_id,
                version,
                repo_id,
                sha
            ])?;
        Ok(())
    }
}
