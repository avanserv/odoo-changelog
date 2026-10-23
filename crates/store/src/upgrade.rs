use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::Store;

pub struct UpgradeEventRow<'a> {
    pub path: &'a str,
    pub line: i64,
    pub col: i64,
    pub seq: i64,
    pub module: &'a str,
    pub folder: &'a str,
    pub version: Option<&'a str>,
    pub phase: Option<&'a str>,
    pub call: &'a str,
    pub kind: &'a str,
    pub op: &'a str,
    pub model: Option<&'a str>,
    pub old: Option<&'a str>,
    pub new: Option<&'a str>,
    pub old_module: Option<&'a str>,
    pub new_module: Option<&'a str>,
    pub partial: bool,
    pub source: Option<&'a str>,
}

impl Store {
    /// The upgrade repository sha and parser version the stored events come from.
    pub fn upgrade_run(&self) -> Result<Option<(String, i64)>> {
        Ok(self
            .conn
            .query_row("SELECT sha, parser_version FROM upgrade_runs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
    }

    /// Replaces every upgrade event with `rows`, read from `sha` by `parser_version`.
    pub fn replace_upgrade(&self, sha: &str, parser_version: i64, rows: &[UpgradeEventRow]) -> Result<()> {
        self.conn.execute("DELETE FROM upgrade_events", [])?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO upgrade_events (path, line, col, seq, module, folder, version, phase, call, kind,
                                         op, model, old, new, old_module, new_module, partial, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        )?;
        for r in rows {
            stmt.execute(params![
                r.path,
                r.line,
                r.col,
                r.seq,
                r.module,
                r.folder,
                r.version,
                r.phase,
                r.call,
                r.kind,
                r.op,
                r.model,
                r.old,
                r.new,
                r.old_module,
                r.new_module,
                r.partial,
                r.source
            ])?;
        }
        self.conn.execute(
            "INSERT INTO upgrade_runs (id, sha, parser_version) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET sha = excluded.sha, parser_version = excluded.parser_version",
            params![sha, parser_version],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn row(path: &str) -> UpgradeEventRow<'_> {
        UpgradeEventRow {
            path,
            line: 3,
            col: 5,
            seq: 0,
            module: "sale",
            folder: "17.0.1.2",
            version: Some("17.0"),
            phase: Some("pre"),
            call: "rename_field",
            kind: "field",
            op: "renamed",
            model: Some("sale.order"),
            old: Some("a"),
            new: Some("b"),
            old_module: None,
            new_module: None,
            partial: false,
            source: None,
        }
    }

    #[test]
    fn a_run_replaces_the_last() {
        let store = Store {
            conn: Connection::open_in_memory().unwrap(),
        };
        store.migrate().unwrap();
        assert_eq!(store.upgrade_run().unwrap(), None);
        store.replace_upgrade("aaa", 1, &[row("x.py"), row("y.py")]).unwrap();
        store.replace_upgrade("bbb", 2, &[row("z.py")]).unwrap();
        assert_eq!(store.upgrade_run().unwrap(), Some(("bbb".into(), 2)));
        let paths: Vec<String> = store
            .conn
            .prepare("SELECT path FROM upgrade_events")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(paths, ["z.py"]);
    }
}
