//! Fixture helpers shared by the integration tests.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use rusqlite::types::Value;

pub struct Fixture {
    pub upstream: PathBuf,
    pub clock: u64,
}

impl Fixture {
    pub fn git(&self, args: &[&str]) {
        git(&self.upstream, args, self.clock);
    }

    /// Fixed author, committer and dates, so every run of the test hashes to the same shas.
    pub fn commit(&mut self, file: &str, message: &str) {
        self.clock += 60;
        let path = self.upstream.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{message}\n")).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", message]);
    }

    pub fn merge(&mut self, branch: &str, message: &str) {
        self.clock += 60;
        self.git(&["merge", "-q", "--no-ff", "-m", message, branch]);
    }

    /// Merges `branch`, conflicts allowed, then writes `files` over the result and commits it.
    pub fn merge_resolving(&mut self, branch: &str, files: &[(&str, &str)], message: &str) {
        self.clock += 60;
        git_status(
            &self.upstream,
            &["merge", "-q", "--no-ff", "--no-commit", branch],
            self.clock,
        );
        for (file, content) in files {
            self.write(file, content);
            self.git(&["add", file]);
        }
        self.git(&["commit", "-q", "-m", message]);
    }

    pub fn write(&self, file: &str, content: &str) {
        let path = self.upstream.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
    }

    /// Writes each `(file, content)` and commits them together.
    pub fn commit_files(&mut self, files: &[(&str, &str)], message: &str) {
        self.clock += 60;
        for (file, content) in files {
            self.write(file, content);
            self.git(&["add", file]);
        }
        self.git(&["commit", "-q", "-m", message]);
    }

    pub fn mv(&mut self, from: &str, to: &str, message: &str) {
        self.clock += 60;
        let parent = self.upstream.join(to);
        std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
        self.git(&["mv", from, to]);
        self.git(&["commit", "-q", "-m", message]);
    }
}

pub fn git(dir: &Path, args: &[&str], clock: u64) {
    assert!(git_status(dir, args, clock), "git {args:?} failed in {}", dir.display());
}

/// Whether git succeeded, with the environment `git` pins.
pub fn git_status(dir: &Path, args: &[&str], clock: u64) -> bool {
    let date = format!("@{} +0000", 1_700_000_000 + clock);
    Command::new("git")
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
        .unwrap()
        .success()
}

/// A relative `config` resolves against the target tmpdir, the parent of every fixture directory.
pub fn odoo_changelog(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_odoo-changelog"))
        .args(["--config", config.to_str().unwrap()])
        .args(args)
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
        .env("RUST_LOG", "warn")
        .output()
        .unwrap()
}

pub fn walk_output(config: &Path) -> Output {
    odoo_changelog(config, &["walk"])
}

pub fn walk(config: &Path) {
    let out = walk_output(config);
    assert!(
        out.status.success(),
        "walk failed with {}: {}",
        config.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Writes `<name>.toml` pointing at `<name>.db`, with `extra` TOML appended.
pub fn config(dir: &Path, name: &str, clone: &Path, extra: &str) -> PathBuf {
    let path = dir.join(format!("{name}.toml"));
    let text = format!(
        "db = {:?}\nfirst_version = \"16.0\"\n\n[[repos]]\nname = \"odoo\"\npath = {:?}\n{extra}",
        dir.join(format!("{name}.db")),
        clone,
    );
    std::fs::write(&path, text).unwrap();
    path
}

pub fn rev_parse(repo: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// An empty upstream repository in a fresh `name` directory.
pub fn init(name: &str) -> (PathBuf, Fixture) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let upstream = dir.join("upstream");
    std::fs::create_dir_all(&upstream).unwrap();
    let fx = Fixture { upstream, clock: 0 };
    fx.git(&["init", "-q"]);
    (dir, fx)
}

/// Upstream with 16.0 and 17.0 forked from master and a forward-port merge, cloned to `clone`.
pub fn setup(name: &str) -> (PathBuf, Fixture, PathBuf) {
    let (dir, mut fx) = init(name);
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
    (dir, fx, clone)
}

/// Every row of every table, each table sorted by all of its columns.
pub fn dump(db: &Path) -> Vec<String> {
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
