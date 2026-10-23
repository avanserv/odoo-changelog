use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::params;

use crate::Store;

/// An `upgrade_events` row, with what matching reads.
pub struct StoredUpgradeEvent {
    pub path: String,
    pub line: i64,
    pub col: i64,
    pub seq: i64,
    pub version: Option<String>,
    pub kind: String,
    pub op: String,
    pub model: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    pub partial: bool,
}

/// A `renamed` event whose commit ships in a known version.
pub struct StoredRename {
    pub repo_id: i64,
    pub sha: String,
    pub path: String,
    pub extractor: String,
    pub symbol_id: Vec<u8>,
    pub version: String,
    pub old_kind: String,
    pub old_qualname: String,
    pub kind: String,
    pub qualname: String,
}

pub struct MatchRow<'a> {
    pub upgrade: &'a StoredUpgradeEvent,
    pub rename: &'a StoredRename,
}

/// An event keyed as `events` keys it, for the `upgrade_matches` row `m`.
const MATCHED_EVENT: &str = "m.repo_id = events.repo_id AND m.sha = events.sha AND m.event_path = events.path
                             AND m.extractor = events.extractor AND m.symbol_id = events.symbol_id";

impl Store {
    /// In key order.
    pub fn upgrade_events(&self) -> Result<Vec<StoredUpgradeEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, line, col, seq, version, kind, op, model, old, new, partial FROM upgrade_events
              ORDER BY path, line, col, seq",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(StoredUpgradeEvent {
                path: r.get(0)?,
                line: r.get(1)?,
                col: r.get(2)?,
                seq: r.get(3)?,
                version: r.get(4)?,
                kind: r.get(5)?,
                op: r.get(6)?,
                model: r.get(7)?,
                old: r.get(8)?,
                new: r.get(9)?,
                partial: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// In key order.
    pub fn detected_renames(&self) -> Result<Vec<StoredRename>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.repo_id, e.sha, e.path, e.extractor, e.symbol_id, c.version, o.kind, o.qualname,
                    s.kind, s.qualname
               FROM events e
               JOIN commits c ON c.repo_id = e.repo_id AND c.sha = e.sha
               JOIN symbols s ON s.id = e.symbol_id
               JOIN symbols o ON o.id = e.old_symbol_id
              WHERE e.op = 'renamed' AND c.version IS NOT NULL
              ORDER BY e.repo_id, e.sha, e.path, e.extractor, e.symbol_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(StoredRename {
                repo_id: r.get(0)?,
                sha: r.get(1)?,
                path: r.get(2)?,
                extractor: r.get(3)?,
                symbol_id: r.get(4)?,
                version: r.get(5)?,
                old_kind: r.get(6)?,
                old_qualname: r.get(7)?,
                kind: r.get(8)?,
                qualname: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replaces every match with `rows`. The events of the old matches get the detector's
    /// confidence and signals back, unless extract rewrote them since; those of the new ones get
    /// confidence 1 and the `upgrade` signal, last in `signals`.
    pub fn replace_matches(&self, rows: &[MatchRow]) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE events SET confidence = m.detected_confidence, signals = json_remove(events.signals, '$[#-1]')
                   FROM upgrade_matches m
                  WHERE {MATCHED_EVENT} AND events.signals ->> '$[#-1]' = 'upgrade'"
            ),
            [],
        )?;
        self.conn.execute("DELETE FROM upgrade_matches", [])?;
        self.conn.execute(
            "UPDATE upgrade_events SET repo_id = NULL, sha = NULL WHERE sha IS NOT NULL",
            [],
        )?;
        let mut insert = self.conn.prepare(
            "INSERT INTO upgrade_matches (path, line, col, seq, repo_id, sha, event_path, extractor, symbol_id,
                                          detected_confidence)
             SELECT ?1, ?2, ?3, ?4, repo_id, sha, path, extractor, symbol_id, confidence FROM events
              WHERE repo_id = ?5 AND sha = ?6 AND path = ?7 AND extractor = ?8 AND symbol_id = ?9",
        )?;
        for MatchRow { upgrade: u, rename: r } in rows {
            insert.execute(params![
                u.path,
                u.line,
                u.col,
                u.seq,
                r.repo_id,
                r.sha,
                r.path,
                r.extractor,
                r.symbol_id
            ])?;
        }
        self.conn.execute(
            &format!(
                "UPDATE events SET confidence = 1.0, signals = json_insert(signals, '$[#]', 'upgrade')
                  WHERE op = 'renamed' AND EXISTS (SELECT 1 FROM upgrade_matches m WHERE {MATCHED_EVENT})"
            ),
            [],
        )?;
        self.conn.execute(
            "UPDATE upgrade_events AS u SET (repo_id, sha) = (
                    SELECT m.repo_id, m.sha FROM upgrade_matches m
                      JOIN commits c ON c.repo_id = m.repo_id AND c.sha = m.sha
                     WHERE m.path = u.path AND m.line = u.line AND m.col = u.col AND m.seq = u.seq
                     ORDER BY c.committed_at, m.repo_id, m.sha LIMIT 1)
              WHERE EXISTS (SELECT 1 FROM upgrade_matches m
                             WHERE m.path = u.path AND m.line = u.line AND m.col = u.col AND m.seq = u.seq)",
            [],
        )?;
        Ok(())
    }

    /// The versions some repo's walk resolved.
    pub fn walked_versions(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT name FROM versions")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    /// One rename of `a.b` into `a.c` in `c1` and one committed later in `c0`, which sorts first
    /// by key, and two upgrade events that declare it.
    fn store() -> Store {
        let store = Store {
            conn: Connection::open_in_memory().unwrap(),
        };
        store.migrate().unwrap();
        store
            .conn
            .execute_batch(
                "INSERT INTO repos (id, name, shallow) VALUES (1, 'odoo', 0);
                 INSERT INTO commits (repo_id, sha, branch, fp_idx, sub_idx, version, parents, is_merge,
                    author_name, author_email, authored_at, committed_at, subject, body, modules, related_prs)
                 VALUES (1, 'c1', '17.0', 1, 0, '17.0', '[]', 0, 'a', 'a@x', 0, 10, 's', '', '[]', '[]'),
                        (1, 'c0', '17.0', 0, 0, '17.0', '[]', 0, 'a', 'a@x', 0, 20, 's', '', '[]', '[]');
                 INSERT INTO symbols (id, kind, qualname) VALUES (x'01', 'model', 'a.b@m'), (x'02', 'model', 'a.c@m');
                 INSERT INTO events (repo_id, sha, path, extractor, symbol_id, op, line, old_symbol_id,
                    confidence, signals)
                 VALUES (1, 'c1', 'm/models.py', 'python', x'02', 'renamed', 1, x'01', 0.75, '[\"_order\"]'),
                        (1, 'c0', 'm/models.py', 'python', x'02', 'renamed', 1, x'01', 0.9, '[]');
                 INSERT INTO upgrade_events (path, line, col, seq, module, folder, version, call, kind, op,
                    old, new, partial)
                 VALUES ('migrations/m/17.0.1.0/pre-x.py', 3, 5, 0, 'm', '17.0.1.0', '17.0', 'rename_model',
                    'model', 'renamed', 'a.b', 'a.c', 0),
                        ('migrations/m/17.0.1.0/pre-x.py', 4, 5, 0, 'm', '17.0.1.0', '17.0', 'merge_model',
                    'model', 'merged', 'a.b', 'a.c', 0);",
            )
            .unwrap();
        store
    }

    /// The confidence and signals of the rename in `sha`.
    fn event(store: &Store, sha: &str) -> (f64, String) {
        store
            .conn
            .query_row("SELECT confidence, signals FROM events WHERE sha = ?1", [sha], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    }

    fn commit(store: &Store, line: i64) -> Option<String> {
        store
            .conn
            .query_row("SELECT sha FROM upgrade_events WHERE line = ?1", [line], |r| r.get(0))
            .unwrap()
    }

    /// Replaces the matches with `pairs` of indexes into the upgrade events and the renames, the
    /// renames in key order (`c0` first).
    fn rematch(store: &Store, pairs: &[(usize, usize)]) {
        let upgrade = store.upgrade_events().unwrap();
        let renames = store.detected_renames().unwrap();
        let rows: Vec<MatchRow> = pairs
            .iter()
            .map(|&(u, r)| MatchRow {
                upgrade: &upgrade[u],
                rename: &renames[r],
            })
            .collect();
        store.replace_matches(&rows).unwrap();
    }

    #[test]
    fn a_match_lasts_only_until_the_next_run() {
        let store = store();
        rematch(&store, &[(0, 1)]);
        assert_eq!(event(&store, "c1"), (1.0, r#"["_order","upgrade"]"#.into()));
        assert_eq!(commit(&store, 3).as_deref(), Some("c1"));
        rematch(&store, &[(0, 1)]);
        assert_eq!(event(&store, "c1"), (1.0, r#"["_order","upgrade"]"#.into()));
        rematch(&store, &[]);
        assert_eq!(event(&store, "c1"), (0.75, r#"["_order"]"#.into()));
        assert_eq!(commit(&store, 3), None);
    }

    #[test]
    fn an_event_extract_rewrote_keeps_its_new_confidence() {
        let store = store();
        rematch(&store, &[(0, 1)]);
        store
            .conn
            .execute(
                "UPDATE events SET confidence = 0.8, signals = '[\"_name\"]' WHERE sha = 'c1'",
                [],
            )
            .unwrap();
        rematch(&store, &[]);
        assert_eq!(event(&store, "c1"), (0.8, r#"["_name"]"#.into()));
    }

    #[test]
    fn an_upgrade_event_names_the_earliest_committed_match() {
        let store = store();
        rematch(&store, &[(0, 0), (0, 1)]);
        assert_eq!(commit(&store, 3).as_deref(), Some("c1"));
        assert_eq!(event(&store, "c0"), (1.0, r#"["upgrade"]"#.into()));
    }

    #[test]
    fn a_rename_two_events_match_is_raised_and_restored_once() {
        let store = store();
        rematch(&store, &[(0, 1), (1, 1)]);
        assert_eq!(event(&store, "c1"), (1.0, r#"["_order","upgrade"]"#.into()));
        rematch(&store, &[]);
        assert_eq!(event(&store, "c1"), (0.75, r#"["_order"]"#.into()));
    }
}
