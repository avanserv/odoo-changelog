//! Two runs on the same refs must produce identical databases, and so must an incremental walk
//! and a full rebuild. The fixture is a small Odoo-shaped repository built with the git CLI:
//! version branches forked from master, forward-port merges and a branch forked between runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;
use rusqlite::types::Value;

struct Fixture {
    upstream: PathBuf,
    clock: u64,
}

impl Fixture {
    fn git(&self, args: &[&str]) {
        git(&self.upstream, args, self.clock);
    }

    /// Fixed author, committer and dates, so every run of the test hashes to the same shas.
    fn commit(&mut self, file: &str, message: &str) {
        self.clock += 60;
        let path = self.upstream.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{message}\n")).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", message]);
    }

    fn merge(&mut self, branch: &str, message: &str) {
        self.clock += 60;
        self.git(&["merge", "-q", "--no-ff", "-m", message, branch]);
    }
}

fn git(dir: &Path, args: &[&str], clock: u64) {
    let date = format!("@{} +0000", 1_700_000_000 + clock);
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=master", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn walk(config: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_odoo-changelog"))
        .args(["--config", config.to_str().unwrap(), "walk"])
        .env("RUST_LOG", "warn")
        .status()
        .unwrap();
    assert!(status.success(), "walk failed with {}", config.display());
}

fn config(dir: &Path, name: &str, clone: &Path) -> PathBuf {
    let path = dir.join(format!("{name}.toml"));
    let text = format!(
        "db = {:?}\nfirst_version = \"16.0\"\n\n[[repos]]\nname = \"odoo\"\npath = {:?}\n",
        dir.join(format!("{name}.db")),
        clone,
    );
    std::fs::write(&path, text).unwrap();
    path
}

/// Every row of every table, each table sorted by all of its columns.
fn dump(db: &Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = tables
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = Vec::new();
    for table in tables {
        let stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let n = stmt.column_count();
        let order = (1..=n).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                let values = (0..n).map(|i| r.get::<_, Value>(i)).collect::<Result<Vec<_>, _>>()?;
                Ok(format!("{table}: {values:?}"))
            })
            .unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out
}

#[test]
fn incremental_walk_equals_full_rebuild() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("determinism");
    let _ = std::fs::remove_dir_all(&dir);
    let upstream = dir.join("upstream");
    std::fs::create_dir_all(&upstream).unwrap();
    let mut fx = Fixture { upstream, clock: 0 };
    fx.git(&["init", "-q"]);

    fx.commit("odoo/release.py", "[ADD] base: version_info = (15, 5)");
    fx.commit("addons/sale/models.py", "[IMP] sale: order lines");
    fx.git(&["checkout", "-q", "-b", "16.0"]);
    fx.commit("addons/sale/models.py", "[FIX] sale: rounding\n\ntask-1001\nopw-2002");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("addons/stock/models.py", "[IMP] stock: moves");
    fx.git(&["checkout", "-q", "-b", "17.0"]);
    fx.commit("addons/stock/models.py", "[FIX] stock: quants");
    fx.git(&["checkout", "-q", "master"]);
    fx.merge("16.0", "[MERGE] forward port 16.0");

    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", "upstream", "clone"], 0);
    let incremental = config(&dir, "incremental", &clone);
    walk(&incremental);

    fx.git(&["checkout", "-q", "16.0"]);
    fx.commit("addons/sale/views.xml", "[FIX] sale: form view");
    fx.git(&["checkout", "-q", "17.0"]);
    fx.commit("addons/stock/views.xml", "[FIX] stock: picking view");
    fx.merge("16.0", "[MERGE] forward port 16.0");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("addons/account/models.py", "[IMP] account: moves");
    fx.git(&["checkout", "-q", "-b", "saas-17.1"]);
    fx.commit("addons/account/models.py", "[FIX] account: taxes");
    fx.git(&["checkout", "-q", "master"]);
    fx.merge("17.0", "[MERGE] forward port 17.0");
    git(&clone, &["fetch", "-q", "origin"], 0);
    walk(&incremental);

    let full = config(&dir, "full", &clone);
    walk(&full);
    let rebuilt = dump(&dir.join("full.db"));
    // Every commit but the root, which master reached before its oldest fork.
    assert_eq!(rebuilt.iter().filter(|r| r.starts_with("commits: ")).count(), 11);
    assert_eq!(dump(&dir.join("incremental.db")), rebuilt);

    walk(&full);
    assert_eq!(
        dump(&dir.join("full.db")),
        rebuilt,
        "a re-run on the same refs changed the database"
    );
}
