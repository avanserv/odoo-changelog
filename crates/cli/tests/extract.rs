//! The extract pipeline against fixtures walked with the binary. `Defs` stands in for a real
//! extractor: it parses `def <name>: <body>` lines and counts how often it parses each blob.

mod common;

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Fixture, config, dump, git, init, odoo_changelog, rev_parse, setup, walk};
use gix::ObjectId;
use odoo_changelog::extract::{Options, Timings, run};
use odoo_changelog_core::{Config, Version};
use odoo_changelog_extract::{Extractor, Registry, Symbol, SymbolTable};
use odoo_changelog_git::{FileChange, FileSide, Repo};
use odoo_changelog_store::Store;
use rusqlite::Connection;

type Calls = Arc<Mutex<BTreeMap<String, usize>>>;

struct Defs {
    name: &'static str,
    version: u32,
    ext: &'static str,
    /// Blob content → number of parses.
    calls: Calls,
}

impl Extractor for Defs {
    fn name(&self) -> &'static str {
        self.name
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn claims(&self, path: &str) -> bool {
        path.rsplit_once('.').is_some_and(|(_, ext)| ext == self.ext)
    }

    fn extract(&self, blob: &[u8]) -> anyhow::Result<SymbolTable> {
        let text = String::from_utf8_lossy(blob);
        *self.calls.lock().unwrap().entry(text.to_string()).or_default() += 1;
        // Finishes out of submission order, so a result that follows thread timing shows.
        std::thread::sleep(Duration::from_millis(blob.len() as u64 % 3));
        let mut table = SymbolTable::default();
        for (i, line) in text.lines().enumerate() {
            if line.starts_with('!') {
                anyhow::bail!("line {}: {line}", i + 1);
            }
            if let Some((name, body)) = line.strip_prefix("def ").and_then(|l| l.split_once(": ")) {
                table.insert(Symbol {
                    kind: "def".into(),
                    name: name.into(),
                    line: i as u32 + 1,
                    attrs: [("body".to_string(), body.into())].into(),
                });
            }
        }
        Ok(table)
    }

    /// A rename within a directory keeps qualnames, and one across directories does not.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
        format!("{dir}:{}", symbol.name)
    }
}

fn defs(name: &'static str, version: u32, ext: &'static str) -> (Box<dyn Extractor>, Calls) {
    let calls = Calls::default();
    let defs = Defs {
        name,
        version,
        ext,
        calls: calls.clone(),
    };
    (Box::new(defs), calls)
}

fn options() -> Options {
    Options {
        repos: Vec::new(),
        from: None,
        to: None,
        threads: None,
    }
}

fn extract(config: &Path, extractors: Vec<Box<dyn Extractor>>, opts: &Options) -> Timings {
    let cfg = Config::load(config).unwrap();
    let store = Store::open(&cfg.db).unwrap();
    run(&cfg, &store, &Registry::new(extractors).unwrap(), opts).unwrap()
}

fn total(calls: &Calls) -> usize {
    calls.lock().unwrap().values().sum()
}

fn count(db: &Path, sql: &str, params: impl rusqlite::Params) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row(sql, params, |r| r.get(0))
        .unwrap()
}

/// `(path, op, qualname, old_path)` of every event of `sha`, in that order.
fn events(db: &Path, sha: &str) -> Vec<(String, String, String, Option<String>)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT e.path, e.op, s.qualname, e.old_path FROM events e JOIN symbols s ON s.id = e.symbol_id
              WHERE e.sha = ?1 ORDER BY e.path, s.qualname",
        )
        .unwrap();
    stmt.query_map([sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn fetch(clone: &Path) {
    git(clone, &["fetch", "-q", "origin"], 0);
}

fn rm(fx: &mut Fixture, file: &str, message: &str) {
    fx.clock += 60;
    fx.git(&["rm", "-q", file]);
    fx.git(&["commit", "-q", "-m", message]);
}

fn revert_head(fx: &mut Fixture, message: &str) {
    fx.clock += 60;
    fx.git(&["revert", "--no-commit", "HEAD"]);
    fx.git(&["commit", "-q", "-m", message]);
}

#[test]
fn same_blob_is_parsed_once() {
    let (dir, mut fx, clone) = setup("extract-once");
    fx.git(&["checkout", "-q", "16.0"]);
    fx.commit_files(&[("addons/sale/a.py", "def f: one\n")], "[ADD] sale: a");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit_files(&[("addons/stock/b.py", "def f: one\n")], "[ADD] stock: b");
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let (py, calls) = defs("py", 1, "py");
    extract(&cfg, vec![py], &options());

    assert_eq!(calls.lock().unwrap()["def f: one\n"], 1);
    let blob = rev_parse(&clone, "origin/16.0:addons/sale/a.py");
    assert_eq!(blob, rev_parse(&clone, "origin/master:addons/stock/b.py"));
    let db = dir.join("walk.db");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM blob_cache WHERE blob_oid = ?1", [&blob]),
        1
    );
    for rev in ["origin/16.0", "origin/master"] {
        assert_eq!(events(&db, &rev_parse(&clone, rev)).len(), 1, "{rev}");
    }
}

#[test]
fn cache_hits_across_runs() {
    let (dir, mut fx, clone) = setup("extract-cache");
    fx.commit_files(&[("addons/sale/a.py", "def f: one\n")], "[ADD] sale: a");
    fx.commit_files(&[("addons/sale/a.py", "def f: two\n")], "[IMP] sale: a");
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);
    let (py, calls) = defs("py", 1, "py");
    extract(&cfg, vec![py], &options());
    let first = total(&calls);

    fx.commit_files(&[("addons/sale/a.py", "def f: one\n")], "[REV] sale: a");
    fetch(&clone);
    walk(&cfg);
    let (py, calls) = defs("py", 1, "py");
    extract(&cfg, vec![py], &options());

    assert!(first > 0);
    assert_eq!(total(&calls), 0, "the second run parsed a blob the first one cached");
    let events = events(&dir.join("walk.db"), &rev_parse(&clone, "origin/master"));
    assert_eq!(
        events,
        [(
            "addons/sale/a.py".into(),
            "changed".into(),
            "addons/sale:f".into(),
            None
        )]
    );
}

/// Python and XML blobs on every branch.
fn two_kinds(name: &str) -> (PathBuf, Fixture, PathBuf) {
    let (dir, mut fx, clone) = setup(name);
    for branch in ["16.0", "17.0", "master"] {
        fx.git(&["checkout", "-q", branch]);
        let m = format!("addons/m{branch}");
        fx.commit_files(
            &[
                (&format!("{m}/models.py"), &format!("def a: {branch}\n")),
                (&format!("{m}/views.xml"), &format!("def v: {branch}\n")),
            ],
            &format!("[ADD] m{branch}"),
        );
    }
    fetch(&clone);
    (dir, fx, clone)
}

#[test]
fn version_bump_invalidates_only_that_extractor() {
    let (dir, _fx, clone) = two_kinds("extract-bump");
    let cfg = config(&dir, "incremental", &clone, "");
    walk(&cfg);
    let db = dir.join("incremental.db");
    let (py, _) = defs("py", 1, "py");
    let (xml, _) = defs("xml", 1, "xml");
    extract(&cfg, vec![py, xml], &options());
    let xml_rows = |db: &Path| {
        dump(db)
            .into_iter()
            .filter(|r| r.contains("\"xml\""))
            .collect::<Vec<_>>()
    };
    let xml_before = xml_rows(&db);

    let (py, py_calls) = defs("py", 2, "py");
    let (xml, xml_calls) = defs("xml", 1, "xml");
    extract(&cfg, vec![py, xml], &options());

    let py_blobs = count(&db, "SELECT COUNT(*) FROM blob_cache WHERE extractor = 'py'", []);
    assert!(py_blobs > 0);
    assert_eq!(
        total(&py_calls),
        py_blobs as usize,
        "py did not parse each blob once again"
    );
    assert_eq!(total(&xml_calls), 0);
    let stale = "SELECT COUNT(*) FROM blob_cache WHERE extractor = 'py' AND extractor_version = 1";
    assert_eq!(count(&db, stale, []), 0);
    assert_eq!(xml_rows(&db), xml_before);

    let fresh = config(&dir, "fresh", &clone, "");
    walk(&fresh);
    let (py, _) = defs("py", 2, "py");
    let (xml, _) = defs("xml", 1, "xml");
    extract(&fresh, vec![py, xml], &options());
    assert_eq!(dump(&db), dump(&dir.join("fresh.db")));
}

/// About 30 commits across 16.0, 17.0 and master: forward-port merges, renames, deletions,
/// reverts, one blob at many paths, a blob that fails to parse and unclaimed files.
fn busy(name: &str) -> (PathBuf, PathBuf) {
    let (dir, mut fx, clone) = setup(name);
    for round in 0..3 {
        for branch in ["16.0", "17.0", "master"] {
            fx.git(&["checkout", "-q", branch]);
            let m = format!("addons/m{branch}_{round}");
            let body = format!("def a: {branch} {round}\ndef b: same\ndef c: {round}\n");
            fx.commit_files(
                &[
                    (&format!("{m}/__init__.py"), "def init: shared\n"),
                    (&format!("{m}/models.py"), &body),
                    (&format!("{m}/views.xml"), &format!("def v: {branch}\n")),
                    (&format!("{m}/notes.txt"), "unclaimed\n"),
                ],
                &format!("[ADD] m{branch}_{round}"),
            );
        }
    }
    fx.git(&["checkout", "-q", "16.0"]);
    fx.commit_files(&[("addons/m16.0_0/models.py", "def a: fixed\n")], "[FIX] m16.0_0");
    revert_head(&mut fx, "[REV] m16.0_0");
    rm(&mut fx, "addons/m16.0_1/views.xml", "[REM] m16.0_1: views");
    fx.git(&["checkout", "-q", "17.0"]);
    fx.merge("16.0", "[MERGE] forward port 16.0");
    fx.mv("addons/m17.0_0/models.py", "addons/m17.0_0/core.py", "[MOV] m17.0_0");
    fx.git(&["checkout", "-q", "master"]);
    fx.git(&["mv", "addons/mmaster_0/models.py", "addons/mmaster_0/main.py"]);
    let edited = "def a: master 0\ndef b: same\ndef c: edited\n";
    fx.commit_files(&[("addons/mmaster_0/main.py", edited)], "[REF] mmaster_0");
    fx.mv(
        "addons/mmaster_1/models.py",
        "addons/moved/models.py",
        "[MOV] mmaster_1",
    );
    fx.commit_files(&[("addons/broken/x.py", "def a: x\n!broken\n")], "[ADD] broken");
    fx.merge("17.0", "[MERGE] forward port 17.0");
    fetch(&clone);
    (dir, clone)
}

#[test]
fn thread_count_does_not_change_the_database() {
    let (dir, clone) = busy("extract-threads");
    let mut dumps = Vec::new();
    for threads in [1, 8] {
        let cfg = config(&dir, &format!("t{threads}"), &clone, "");
        walk(&cfg);
        let (py, _) = defs("py", 1, "py");
        let (xml, _) = defs("xml", 1, "xml");
        let opts = Options {
            threads: NonZeroUsize::new(threads),
            ..options()
        };
        extract(&cfg, vec![py, xml], &opts);
        dumps.push(dump(&dir.join(format!("t{threads}.db"))));
    }
    let n = dumps[0].iter().filter(|r| r.starts_with("events: ")).count();
    assert!(n > 30, "only {n} events");
    assert_eq!(dumps[0], dumps[1]);

    let db = dir.join("t1.db");
    let merges = "SELECT COUNT(*) FROM commits c WHERE is_merge = 1 AND (
        EXISTS (SELECT 1 FROM extracted e WHERE e.sha = c.sha) OR EXISTS (SELECT 1 FROM events e WHERE e.sha = c.sha))";
    assert_eq!(count(&db, merges, []), 0, "a merge was extracted");
    let unmarked = "SELECT COUNT(*) FROM commits c WHERE is_merge = 0
        AND (SELECT COUNT(*) FROM extracted e WHERE e.sha = c.sha) != 2";
    assert_eq!(
        count(&db, unmarked, []),
        0,
        "a commit was not marked by both extractors"
    );
    let failed = "SELECT COUNT(*) FROM blob_cache WHERE symbols IS NULL AND error = 'line 2: !broken'";
    assert_eq!(count(&db, failed, []), 1);
    let broken = rev_parse(&clone, "origin/master~1");
    assert!(
        events(&db, &broken).is_empty(),
        "a file that failed to parse gave events"
    );
}

#[test]
fn unclaimed_blobs_are_never_read() {
    let (dir, mut fx, clone) = setup("extract-unclaimed");
    fx.commit_files(
        &[
            ("addons/sale/notes.txt", "never read\n"),
            ("addons/sale/a.py", "def f: one\n"),
        ],
        "[ADD] sale: notes",
    );
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let blob = rev_parse(&clone, "origin/master:addons/sale/notes.txt");
    let loose = clone.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    assert!(loose.exists(), "the fetch packed the blob; the test cannot remove it");
    std::fs::remove_file(&loose).unwrap();

    let (py, _) = defs("py", 1, "py");
    extract(&cfg, vec![py], &options());
    let events = events(&dir.join("walk.db"), &rev_parse(&clone, "origin/master"));
    assert_eq!(events.len(), 1);
}

#[test]
fn renames_are_paired() {
    let (dir, mut fx, clone) = setup("extract-renames");
    let body = "def a: 1\ndef b: 2\ndef c: 3\ndef d: 4\ndef e: 5\n";
    fx.commit_files(
        &[
            ("addons/sale/a.py", body),
            ("addons/sale/c.py", "def c: pure\n"),
            ("addons/sale/e.py", "def e: gone\n"),
        ],
        "[ADD] sale",
    );
    fx.git(&["mv", "addons/sale/a.py", "addons/sale/b.py"]);
    let edited = body.replace("def e: 5", "def e: five");
    fx.commit_files(&[("addons/sale/b.py", &edited)], "[REF] sale: a to b");
    fx.mv("addons/sale/c.py", "addons/sale/d.py", "[MOV] sale: c to d");
    fx.mv("addons/sale/e.py", "addons/sale/e.txt", "[MOV] sale: e to text");
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);
    let (py, _) = defs("py", 1, "py");
    extract(&cfg, vec![py], &options());

    let db = dir.join("walk.db");
    let sha = |rev: &str| rev_parse(&clone, rev);
    let from_a = Some("addons/sale/a.py".to_string());
    let event = |path: &str, op: &str, name: &str, old: &Option<String>| {
        (
            path.to_string(),
            op.to_string(),
            format!("addons/sale:{name}"),
            old.clone(),
        )
    };
    let moved = ["a", "b", "c", "d"].map(|n| event("addons/sale/b.py", "moved", n, &from_a));
    let mut expected = moved.to_vec();
    expected.push(event("addons/sale/b.py", "changed", "e", &from_a));
    assert_eq!(events(&db, &sha("origin/master~2")), expected);
    let from_c = Some("addons/sale/c.py".to_string());
    assert_eq!(
        events(&db, &sha("origin/master~1")),
        [event("addons/sale/d.py", "moved", "c", &from_c)]
    );
    assert_eq!(
        events(&db, &sha("origin/master")),
        [event("addons/sale/e.py", "removed", "e", &None)]
    );

    let repo = Repo::open("odoo", &clone, "origin").unwrap();
    let mut worker = repo.shared().worker();
    let oid = ObjectId::from_hex(sha("origin/master~2").as_bytes()).unwrap();
    let changes = worker.changed_files(oid, &|p| p.ends_with(".py")).unwrap().unwrap();
    let side = |path: &str, rev: &str| FileSide {
        path: path.into(),
        blob: ObjectId::from_hex(sha(&format!("{rev}:{path}")).as_bytes()).unwrap(),
    };
    assert_eq!(
        changes,
        [FileChange {
            old: Some(side("addons/sale/a.py", "origin/master~3")),
            new: Some(side("addons/sale/b.py", "origin/master~2")),
        }]
    );
}

/// Whether `sha` has a parent the clone lacks, which a shallow file entry alone does not say:
/// git lists a commit at the depth limit even when another branch brought its parent in.
fn parent_missing(clone: &Path, sha: &str) -> bool {
    let out = Command::new("git")
        .args(["cat-file", "commit", sha])
        .current_dir(clone)
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_prefix("parent "))
        .any(|parent| {
            !Command::new("git")
                .args(["cat-file", "-e", parent])
                .current_dir(clone)
                .status()
                .unwrap()
                .success()
        })
}

#[test]
fn shallow_boundary_is_skipped() {
    let (dir, mut fx) = init("extract-shallow");
    fx.commit("odoo/release.py", "version_info = (15, 5)");
    fx.commit("addons/sale/models.py", "[IMP] sale: order lines");
    fx.git(&["checkout", "-q", "-b", "16.0"]);
    for i in 1..=7 {
        fx.commit_files(&[("addons/sale/models.py", &format!("def f: {i}\n"))], "[FIX] sale");
    }
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("odoo/release.py", "version_info = (16, 1)");
    fx.commit("addons/stock/models.py", "[IMP] stock: moves");
    fx.git(&["checkout", "-q", "-b", "17.0"]);
    fx.commit("addons/stock/models.py", "[FIX] stock: quants");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("odoo/release.py", "version_info = (17, 1)");
    fx.commit("addons/account/models.py", "[IMP] account: moves");
    // Depth 5 keeps 16.0's last five commits, the oldest of which lacks its parent.
    let upstream = format!("file://{}", fx.upstream.display());
    let args = ["clone", "-q", "--depth", "5", "--no-single-branch", &upstream, "clone"];
    git(&dir, &args, 0);
    let clone = dir.join("clone");
    let cfg = config(&dir, "walk", &clone, "release_file = \"odoo/release.py\"\n");
    walk(&cfg);

    let (py, _) = defs("py", 1, "py");
    extract(&cfg, vec![py], &options());

    let db = dir.join("walk.db");
    let shallow = std::fs::read_to_string(clone.join(".git/shallow")).unwrap();
    let boundary: Vec<&str> = shallow
        .lines()
        .filter(|sha| parent_missing(&clone, sha))
        .filter(|sha| count(&db, "SELECT COUNT(*) FROM commits WHERE sha = ?1", [sha]) == 1)
        .collect();
    assert!(!boundary.is_empty(), "no boundary commit was walked");
    for sha in boundary {
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM extracted WHERE sha = ?1", [sha]),
            0,
            "{sha}"
        );
    }
    assert!(count(&db, "SELECT COUNT(*) FROM extracted", []) > 0);
}

#[test]
fn incremental_range_equals_full() {
    let (dir, clone) = busy("extract-range");
    let incremental = config(&dir, "incremental", &clone, "");
    walk(&incremental);
    let to_16 = Options {
        to: Version::parse_branch("16.0"),
        ..options()
    };
    let (py, _) = defs("py", 1, "py");
    extract(&incremental, vec![py], &to_16);
    let db = dir.join("incremental.db");
    let outside = "SELECT COUNT(*) FROM extracted e JOIN commits c ON c.sha = e.sha WHERE c.version != '16.0'";
    assert_eq!(count(&db, outside, []), 0);
    assert!(count(&db, "SELECT COUNT(*) FROM extracted", []) > 0);
    let (py, _) = defs("py", 1, "py");
    extract(&incremental, vec![py], &options());

    let full = config(&dir, "full", &clone, "");
    walk(&full);
    let (py, _) = defs("py", 1, "py");
    extract(&full, vec![py], &options());
    assert_eq!(dump(&db), dump(&dir.join("full.db")));
}

#[test]
fn extract_command_prints_timings_and_checks_its_arguments() {
    let (dir, _, clone) = setup("extract-cli");
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let out = odoo_changelog(&cfg, &["extract", "--timings"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for phase in ["diff", "parse", "cache", "write", "total"] {
        assert!(
            stdout.lines().any(|l| l.starts_with(phase)),
            "no {phase} line in {stdout}"
        );
    }

    for (args, expected) in [
        (&["extract", "--repo", "foo"][..], "no repo named foo"),
        (&["extract", "--from", "17"][..], "17 is not a version"),
    ] {
        let out = odoo_changelog(&cfg, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} succeeded");
        assert!(stderr.contains(expected), "unexpected error: {stderr}");
    }
}

/// Lines per Python or XML file, the cheapest extractor that reads every claimed blob.
struct Lines;

impl Extractor for Lines {
    fn name(&self) -> &'static str {
        "lines"
    }

    fn version(&self) -> u32 {
        1
    }

    fn claims(&self, path: &str) -> bool {
        path.ends_with(".py") || path.ends_with(".xml")
    }

    fn extract(&self, blob: &[u8]) -> anyhow::Result<SymbolTable> {
        let mut table = SymbolTable::default();
        table.insert(Symbol {
            kind: "file".into(),
            name: String::new(),
            line: 1,
            attrs: [("lines".to_string(), blob.split(|b| *b == b'\n').count().into())].into(),
        });
        Ok(table)
    }

    fn qualify(&self, path: &str, _: &Symbol) -> String {
        path.into()
    }
}

#[test]
#[ignore = "reads the Odoo clones in config/odoo-changelog.toml"]
fn real_odoo_17_to_18() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/odoo-changelog.toml");
    let mut cfg = Config::load(&path).unwrap();
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("real-odoo");
    let _ = std::fs::remove_dir_all(&scratch);
    cfg.db = scratch.join("odoo.db");
    // Master's walk starts at the oldest fork, so 17.0 is all the history the range needs.
    cfg.first_version = "17.0".into();
    let store = Store::open(&cfg.db).unwrap();
    let only = ["odoo".to_string()];

    let start = std::time::Instant::now();
    odoo_changelog::walk::run(&cfg, &store, &only).unwrap();
    println!("walk     {:>9.2}", start.elapsed().as_secs_f64());

    let opts = Options {
        repos: only.to_vec(),
        from: Version::parse_branch("17.0"),
        to: Version::parse_branch("18.0"),
        threads: None,
    };
    for run_name in ["cold", "warm"] {
        if run_name == "warm" {
            // Forget the marks but keep the cache, to time a run that parses nothing.
            let conn = Connection::open(&cfg.db).unwrap();
            conn.execute_batch("DELETE FROM events; DELETE FROM extracted; DELETE FROM symbols;")
                .unwrap();
        }
        let t = run(&cfg, &store, &Registry::new(vec![Box::new(Lines)]).unwrap(), &opts).unwrap();
        println!("{run_name}");
        for (phase, spent) in [
            ("diff", t.diff),
            ("parse", t.parse),
            ("cache", t.cache),
            ("write", t.write),
        ] {
            println!("{phase:<8} {:>9.2}", spent.as_secs_f64());
        }
    }
    let events = count(&cfg.db, "SELECT COUNT(*) FROM events", []);
    let commits = count(&cfg.db, "SELECT COUNT(DISTINCT sha) FROM extracted", []);
    let blobs = count(&cfg.db, "SELECT COUNT(*) FROM blob_cache", []);
    println!("{commits} commits, {blobs} blobs, {events} events");
}
