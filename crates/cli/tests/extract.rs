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
use odoo_changelog_extract::{Data, Extractor, Js, Manifest, Owl, Python, Registry, Scss, Span, Symbol, SymbolTable};
use odoo_changelog_git::{FileChange, FileSide, Repo};
use odoo_changelog_store::Store;
use rusqlite::Connection;

type Calls = Arc<Mutex<BTreeMap<String, usize>>>;

const SALE_EXT: &str = "addons/sale_ext/models/sale_order.py";

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
                    span: Span::default(),
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

/// The warnings `extractor` cached for the blob at `rev:path`.
fn cached_warnings(db: &Path, clone: &Path, extractor: &str, at: &str) -> Vec<String> {
    let blob = rev_parse(clone, at);
    let symbols: Vec<u8> = Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT symbols FROM blob_cache WHERE blob_oid = ?1 AND extractor = ?2",
            [&blob, extractor],
            |r| r.get(0),
        )
        .unwrap();
    SymbolTable::from_bytes(&symbols).unwrap().warnings().to_vec()
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
/// reverts, one blob at many paths, a blob that fails to parse, unclaimed files, a manifest per
/// module for `Manifest`, a model file for `Python` that gains a field and renames a method, a
/// view and an access CSV per module for `Data`, a registry entry per module for `Js`, and a
/// template and a stylesheet per module for `Owl` and `Scss`.
fn busy(name: &str) -> (PathBuf, PathBuf) {
    let (dir, mut fx, clone) = setup(name);
    for round in 0..3 {
        for branch in ["16.0", "17.0", "master"] {
            fx.git(&["checkout", "-q", branch]);
            let m = format!("addons/m{branch}_{round}");
            let body = format!("def a: {branch} {round}\ndef b: same\ndef c: {round}\n");
            // A directory with a dot is no module name, so the manifest goes in a sibling.
            let module = format!("addons/m{}_{round}", branch.replace('.', ""));
            let manifest = format!("{{'name': 'M', 'depends': ['base', 'm{branch}_{round}']}}\n");
            let view = format!(
                "<odoo><record id=\"view_form\" model=\"ir.ui.view\"><field name=\"arch\" type=\"xml\"><form><field name=\"f{round}\"/><field name=\"b{}\"/></form></field></record></odoo>\n",
                branch.replace('.', "")
            );
            let access = "id,name,model_id:id,group_id:id,perm_read\naccess_m,m,model_m,base.group_user,1\n";
            let app = format!(
                "/** @odoo-module **/\nimport {{ registry }} from \"@web/core/registry\";\nregistry.category(\"fields\").add(\"f{branch}_{round}\", {{}});\n"
            );
            let template =
                format!("<templates><t t-name=\"m.App\"><div t-ref=\"r{branch}_{round}\"/></t></templates>\n");
            let style = format!("$m-color: c{branch}_{round} !default;\n");
            fx.commit_files(
                &[
                    (&format!("{module}/__manifest__.py"), &manifest),
                    (&format!("{m}/views/data.xml"), &view),
                    (&format!("{m}/security/ir.model.access.csv"), access),
                    (&format!("{module}/static/src/app.js"), &app),
                    (&format!("{module}/static/src/app.xml"), &template),
                    (&format!("{module}/static/src/app.scss"), &style),
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
    let model =
        "class SaleOrder(models.Model):\n    _inherit = 'sale.order'\n\n    def _prepare(self):\n        return {}\n";
    fx.commit_files(&[(SALE_EXT, model)], "[ADD] sale_ext");
    let model = model
        .replace("_prepare", "_prepare_vals")
        .replace("\n\n", "\n    foo = fields.Char()\n\n");
    fx.commit_files(&[(SALE_EXT, &model)], "[IMP] sale_ext: foo");
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
        extract(
            &cfg,
            vec![
                py,
                xml,
                Box::new(Data),
                Box::new(Js),
                Box::new(Manifest),
                Box::new(Owl),
                Box::new(Python),
                Box::new(Scss),
            ],
            &opts,
        );
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
        AND (SELECT COUNT(*) FROM extracted e WHERE e.sha = c.sha) != 8";
    assert_eq!(
        count(&db, unmarked, []),
        0,
        "a commit was not marked by every extractor"
    );
    let python = "SELECT COUNT(*) FROM events WHERE extractor = 'python' AND path = ?1";
    assert!(count(&db, python, [SALE_EXT]) >= 4, "python gave too few events");
    let modules = "SELECT COUNT(*) FROM events WHERE extractor = 'manifest' AND op = 'added'";
    assert_eq!(count(&db, modules, []), 9, "one module per [ADD] commit");
    let views = "SELECT COUNT(*) FROM events e JOIN symbols s ON s.id = e.symbol_id
        WHERE e.extractor = 'data' AND e.op = 'added' AND s.kind = ?1";
    for (kind, added) in [("view", 9), ("view_node", 18), ("access", 9)] {
        assert_eq!(
            count(&db, views, [kind]),
            added,
            "{kind}: {added} over the 9 [ADD] commits"
        );
    }
    let js = "SELECT COUNT(*) FROM events e JOIN symbols s ON s.id = e.symbol_id
        WHERE e.extractor = 'js' AND e.op = 'added' AND s.kind = ?1";
    for kind in ["js_module", "js_registry"] {
        assert_eq!(count(&db, js, [kind]), 9, "{kind}: one per [ADD] commit");
    }
    let client = "SELECT COUNT(*) FROM events e JOIN symbols s ON s.id = e.symbol_id
        WHERE e.extractor IN ('owl', 'scss') AND e.op = 'added' AND s.kind = ?1";
    for kind in ["owl_template", "owl_node", "scss_variable"] {
        assert_eq!(count(&db, client, [kind]), 9, "{kind}: one per [ADD] commit");
    }
    let failed = "SELECT COUNT(*) FROM blob_cache WHERE symbols IS NULL AND error = 'line 2: !broken'";
    assert_eq!(count(&db, failed, []), 1);
    let broken = rev_parse(&clone, "origin/master~3");
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
    let unpaired = [
        FileChange {
            old: Some(side("addons/sale/a.py", "origin/master~3")),
            new: None,
        },
        FileChange {
            old: None,
            new: Some(side("addons/sale/b.py", "origin/master~2")),
        },
    ];
    assert_eq!(changes, unpaired);
    assert_eq!(
        worker.pair_renames(&changes, &|p| p.ends_with(".py")).unwrap(),
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

/// Pairing renames among every pending extractor's files would make one extractor's events
/// depend on which others run with it: a rename across extensions, or a deletion closer to
/// another extractor's addition than to its own.
#[test]
fn rename_pairing_ignores_other_extractors() {
    let (dir, mut fx, clone) = setup("extract-pairing");
    let lines: Vec<String> = (0..10).map(|i| format!("def l{i}: {i}\n")).collect();
    let near = |changed: usize| {
        let mut l = lines.clone();
        l.iter_mut()
            .take(changed)
            .for_each(|line| *line = line.replace(": ", ": edited "));
        l.concat()
    };
    fx.commit_files(
        &[
            ("addons/sale/x.py", "def f: one\n"),
            ("addons/sale/a.py", &lines.concat()),
        ],
        "[ADD] sale",
    );
    fx.mv("addons/sale/x.py", "addons/sale/x.xml", "[MOV] sale: x to xml");
    fx.git(&["rm", "-q", "addons/sale/a.py"]);
    fx.commit_files(
        &[("addons/sale/b.py", &near(3)), ("addons/sale/c.xml", &near(1))],
        "[REF] sale: a to b and c",
    );
    fetch(&clone);
    let run_all = |name: &str, runs: &[&[(&'static str, u32)]]| {
        let cfg = config(&dir, name, &clone, "");
        walk(&cfg);
        for extractors in runs {
            let extractors = extractors
                .iter()
                .map(|&(ext, version)| defs(ext, version, ext).0)
                .collect();
            extract(&cfg, extractors, &options());
        }
        dump(&dir.join(format!("{name}.db")))
    };

    let full = run_all("full", &[&[("py", 1), ("xml", 1)]]);
    assert_eq!(run_all("later", &[&[("py", 1)], &[("py", 1), ("xml", 1)]]), full);
    let bumped = run_all("bumped", &[&[("py", 1), ("xml", 1)], &[("py", 1), ("xml", 2)]]);
    assert_eq!(bumped, run_all("fresh", &[&[("py", 1), ("xml", 2)]]));

    let db = dir.join("full.db");
    let event = |path: &str, op: &str, name: &str, old: Option<&str>| {
        (
            path.to_string(),
            op.to_string(),
            format!("addons/sale:{name}"),
            old.map(String::from),
        )
    };
    assert_eq!(
        events(&db, &rev_parse(&clone, "origin/master~1")),
        [
            event("addons/sale/x.py", "removed", "f", None),
            event("addons/sale/x.xml", "added", "f", None),
        ]
    );
    let moved = events(&db, &rev_parse(&clone, "origin/master"));
    let py: Vec<_> = moved.iter().filter(|e| e.0 == "addons/sale/b.py").collect();
    let ops: Vec<&str> = py.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(ops, [["changed"; 3].as_slice(), &["moved"; 7]].concat(), "{moved:?}");
    assert!(py.iter().all(|e| e.3.as_deref() == Some("addons/sale/a.py")));
    assert!(moved.iter().all(|e| e.0 != "addons/sale/a.py"), "{moved:?}");
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
    for phase in ["setup", "diff", "parse", "cache", "write", "total"] {
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

#[test]
fn builtin_python_extractor_records_model_events() {
    let (dir, mut fx, clone) = setup("extract-python");
    let model = "class SaleOrder(models.Model):\n    _inherit = 'sale.order'\n    foo = fields.Char()\n";
    fx.commit_files(&[(SALE_EXT, model)], "[ADD] sale_ext");
    let shifted = format!("# Extends the sale order.\nfrom odoo import fields, models\n\n\n{model}");
    fx.commit_files(&[(SALE_EXT, &shifted)], "[IMP] sale_ext: imports");
    let required = shifted.replace("fields.Char()", "fields.Char(required=True)");
    fx.commit_files(&[(SALE_EXT, &required)], "[IMP] sale_ext: foo is required");
    fx.commit_files(
        &[("addons/sale_ext/models/broken.py", "def f(:\n")],
        "[ADD] sale_ext: broken",
    );
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let out = odoo_changelog(&cfg, &["extract"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("gave warnings"), "{output}");

    let db = dir.join("walk.db");
    let at = |rev: &str| events(&db, &rev_parse(&clone, rev));
    let field = |op: &str| {
        (
            SALE_EXT.to_string(),
            op.to_string(),
            "sale.order#foo@sale_ext".to_string(),
            None,
        )
    };
    assert!(
        at("origin/master~3").contains(&field("added")),
        "{:?}",
        at("origin/master~3")
    );
    assert_eq!(at("origin/master~2"), []);
    assert_eq!(at("origin/master~1"), [field("changed")]);
    let broken = cached_warnings(&db, &clone, "python", "origin/master:addons/sale_ext/models/broken.py");
    assert!(!broken.is_empty());
}

/// `(op, qualname, old_path, attrs_after)` of the manifest events of `sha`.
fn module_events(db: &Path, sha: &str) -> Vec<(String, String, Option<String>, Option<String>)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT e.op, s.qualname, e.old_path, e.attrs_after FROM events e JOIN symbols s ON s.id = e.symbol_id
              WHERE e.sha = ?1 AND e.extractor = 'manifest' ORDER BY e.path, s.qualname",
        )
        .unwrap();
    stmt.query_map([sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn builtin_manifest_extractor_records_module_events() {
    let (dir, mut fx, clone) = setup("extract-manifest");
    let openerp = "addons/sale/__openerp__.py";
    let manifest = "addons/sale/__manifest__.py";
    let model = "class SaleOrder(models.Model):\n    _name = 'sale.order'\n";
    let base = "{'name': 'Sales', 'depends': ['sales_team', 'payment', 'portal']}\n";
    fx.commit_files(
        &[(openerp, base), ("addons/sale/models/sale.py", model)],
        "[ADD] sale: manifest",
    );
    fx.mv(openerp, manifest, "[MOV] sale: rename the manifest");
    let edited = "{'name': 'Sales', 'depends': ['sales_team', 'account_payment', 'utm'],\n 'assets': {'web.assets_backend': ['sale/static/src/**/*', ('remove', 'sale/static/src/old/*')]}}\n";
    fx.commit_files(&[(manifest, edited)], "[IMP] sale: depends and assets");
    let reformatted = "{\n    \"name\": \"Sales\",\n    \"depends\": [\"sales_team\", \"account_payment\", \"utm\"],\n    \"assets\": {\n        \"web.assets_backend\": [\n            \"sale/static/src/**/*\",\n            (\"remove\", \"sale/static/src/old/*\"),\n        ],\n    },\n}\n";
    fx.commit_files(&[(manifest, reformatted)], "[REF] sale: reformat the manifest");
    fx.commit_files(
        &[(manifest, "{'name': 'Sales', 'depends': ['sales_team',\n")],
        "[BRK] sale",
    );
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let out = odoo_changelog(&cfg, &["extract"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("gave warnings"), "{output}");

    let db = dir.join("walk.db");
    let at = |rev: &str| module_events(&db, &rev_parse(&clone, rev));
    let added = at("origin/master~4");
    assert_eq!(added.len(), 1, "{added:?}");
    assert_eq!((added[0].0.as_str(), added[0].1.as_str()), ("added", "sale"));
    let on_manifests = "SELECT COUNT(*) FROM events WHERE path IN (?1, ?2) AND extractor != 'manifest'";
    assert_eq!(count(&db, on_manifests, [openerp, manifest]), 0);

    let moved = at("origin/master~3");
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(
        (moved[0].0.as_str(), moved[0].1.as_str(), moved[0].2.as_deref()),
        ("moved", "sale", Some(openerp))
    );

    let changed = at("origin/master~2");
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(changed[0].0, "changed");
    let after = changed[0].3.as_deref().unwrap();
    assert!(
        after.contains(r#""depends":["sales_team","account_payment","utm"]"#),
        "{after}"
    );
    let bundle = r#""web.assets_backend":["sale/static/src/**/*",["remove","sale/static/src/old/*"]]"#;
    assert!(after.contains(bundle), "{after}");

    assert_eq!(at("origin/master~1"), []);
    let broken = cached_warnings(&db, &clone, "manifest", &format!("origin/master:{manifest}"));
    assert!(!broken.is_empty());
}

/// `(op, kind, qualname, attrs_after)` of the data events of `sha`.
fn data_events(db: &Path, sha: &str) -> Vec<(String, String, String, Option<String>)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT e.op, s.kind, s.qualname, e.attrs_after FROM events e JOIN symbols s ON s.id = e.symbol_id
              WHERE e.sha = ?1 AND e.extractor = 'data' ORDER BY e.path, s.kind, s.qualname",
        )
        .unwrap();
    stmt.query_map([sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn builtin_data_extractor_records_view_events() {
    let (dir, mut fx, clone) = setup("extract-data");
    let views = "addons/sale/views/sale_views.xml";
    let access = "addons/sale/security/ir.model.access.csv";
    let form = r#"<odoo>
    <record id="view_order_form" model="ir.ui.view">
        <field name="model">sale.order</field>
        <field name="arch" type="xml">
            <form><sheet><group name="partner"><field name="partner_id"/><field name="date_order"/></group></sheet></form>
        </field>
    </record>
    <menuitem id="menu_sale_root" name="Sales">
        <menuitem id="menu_sale_orders" name="Orders" action="action_orders"/>
    </menuitem>
</odoo>
"#;
    let csv =
        "id,name,model_id:id,group_id:id,perm_read\naccess_sale_order,sale.order,model_sale_order,base.group_user,1\n";
    fx.commit_files(&[(views, form), (access, csv)], "[ADD] sale: views");
    let ext = r#"<odoo>
    <record id="view_order_form_ext" model="ir.ui.view">
        <field name="model">sale.order</field>
        <field name="inherit_id" ref="sale.view_order_form"/>
        <field name="arch" type="xml">
            <xpath expr="//field[@name='partner_id']" position="after"><field name="ext_id"/></xpath>
        </field>
    </record>
</odoo>
"#;
    fx.commit_files(&[("addons/sale_ext/views/views.xml", ext)], "[ADD] sale_ext: views");
    let reindented = form
        .replace("<form><sheet>", "\n<form>\n  <!-- the order -->\n  <sheet>\n")
        .replace("    ", "\t");
    fx.commit_files(&[(views, &reindented)], "[REF] sale: reindent");
    let removed = reindented.replace(r#"<field name="partner_id"/>"#, "");
    fx.commit_files(&[(views, &removed)], "[IMP] sale: no partner");
    fx.commit_files(
        &[(
            "addons/sale/views/broken.xml",
            "<odoo><record id=\"x\" model=\"m\"></odoo>\n",
        )],
        "[ADD] sale: broken",
    );
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let out = odoo_changelog(&cfg, &["extract"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("gave warnings"), "{output}");

    let db = dir.join("walk.db");
    let at = |rev: &str| data_events(&db, &rev_parse(&clone, rev));
    let ops = |events: &[(String, String, String, Option<String>)]| -> Vec<(String, String, String)> {
        events
            .iter()
            .map(|(op, kind, q, _)| (op.clone(), kind.clone(), q.clone()))
            .collect()
    };
    let added = at("origin/master~4");
    let row = |op: &str, kind: &str, qualname: &str| (op.to_string(), kind.to_string(), qualname.to_string());
    assert_eq!(
        ops(&added),
        [
            row("added", "access", "sale.access_sale_order@sale"),
            row("added", "menu", "sale.menu_sale_orders@sale"),
            row("added", "menu", "sale.menu_sale_root@sale"),
            row("added", "view", "sale.view_order_form@sale"),
            row(
                "added",
                "view_node",
                "sale.view_order_form#field[@name='date_order']@sale"
            ),
            row(
                "added",
                "view_node",
                "sale.view_order_form#field[@name='partner_id']@sale"
            ),
            row("added", "view_node", "sale.view_order_form#group[@name='partner']@sale"),
        ]
    );
    let menu = added[1].3.as_deref().unwrap();
    assert!(menu.contains(r#""action":"sale.action_orders""#), "{menu}");
    assert!(menu.contains(r#""parent":"sale.menu_sale_root""#), "{menu}");
    let rights = added[0].3.as_deref().unwrap();
    assert!(rights.contains(r#""model_id":"sale.model_sale_order""#), "{rights}");
    let on_data = "SELECT COUNT(*) FROM events WHERE path IN (?1, ?2) AND extractor != 'data'";
    assert_eq!(count(&db, on_data, [views, access]), 0);

    let inherit = at("origin/master~3");
    let view = inherit.iter().find(|e| e.1 == "view").unwrap();
    assert_eq!(view.2, "sale_ext.view_order_form_ext@sale_ext");
    let after = view.3.as_deref().unwrap();
    assert!(after.contains(r#""inherit_id":"sale.view_order_form""#), "{after}");
    let spec = r#""specs":[{"expr":"//field[@name='partner_id']","position":"after"}]"#;
    assert!(after.contains(spec), "{after}");

    assert_eq!(at("origin/master~2"), []);
    assert_eq!(
        ops(&at("origin/master~1")),
        [
            row("changed", "view", "sale.view_order_form@sale"),
            row(
                "removed",
                "view_node",
                "sale.view_order_form#field[@name='partner_id']@sale"
            ),
        ]
    );

    assert_eq!(at("origin/master"), []);
    let broken = cached_warnings(&db, &clone, "data", "origin/master:addons/sale/views/broken.xml");
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(
        broken[0].starts_with("line 1: ") && broken[0].contains("</record>"),
        "{broken:?}"
    );
}

/// `(op, kind, qualname, old_path, attrs_after)` of the js events of `sha`.
type JsEvent = (String, String, String, Option<String>, Option<String>);

fn js_events(db: &Path, sha: &str) -> Vec<JsEvent> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT e.op, s.kind, s.qualname, e.old_path, e.attrs_after FROM events e JOIN symbols s ON s.id = e.symbol_id
              WHERE e.sha = ?1 AND e.extractor = 'js' ORDER BY s.kind, s.qualname, e.op",
        )
        .unwrap();
    stmt.query_map([sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

const THING_FIELD: &str = r#"/** @odoo-module **/

import { Component } from "@odoo/owl";
import { registry } from "@web/core/registry";
import { helper } from "./utils";

export class ThingField extends Component {
    static template = "demo.ThingField";

    setup() {
        this.value = helper(this.props.value);
    }
}

export const thingField = { component: ThingField };

registry.category("fields").add("thing", thingField);
"#;

const LEGACY_THING: &str = r#"odoo.define('demo.thing_legacy', function (require) {
"use strict";

var FormController = require('web.FormController');
var fieldRegistry = require('web.field_registry');
var AbstractField = require('web.AbstractField');

var LegacyThing = AbstractField.extend({
    template: 'demo.LegacyThing',
    _render: function () {},
});

FormController.include({
    _onThing: function () {},
});

fieldRegistry.add('legacy_thing', LegacyThing);

return LegacyThing;
});
"#;

#[test]
fn builtin_js_extractor_records_registry_events() {
    let (dir, mut fx, clone) = setup("extract-js");
    let field = "addons/demo/static/src/fields/thing_field.js";
    let moved = "addons/demo/static/src/fields/thing/thing_field.js";
    fx.commit_files(
        &[
            (field, THING_FIELD),
            ("addons/demo/static/tests/t.js", THING_FIELD),
            ("addons/demo/static/lib/l.js", THING_FIELD),
        ],
        "[ADD] demo: thing field",
    );
    let reformatted = THING_FIELD
        .replace("    ", "\t")
        .replace(';', "")
        .replace('"', "'")
        .replace("{ component: ThingField }", "{\n\tcomponent: ThingField,\n}");
    fx.commit_files(&[(field, &reformatted)], "[REF] demo: reformat");
    let retemplated = reformatted.replace("demo.ThingField", "demo.ThingFieldV2");
    fx.commit_files(&[(field, &retemplated)], "[IMP] demo: new template");
    fx.mv(field, moved, "[MOV] demo: thing field");
    fx.commit_files(
        &[("addons/demo/static/src/legacy/thing.js", LEGACY_THING)],
        "[ADD] demo: legacy thing",
    );
    fx.commit_files(
        &[("addons/demo/static/src/broken.js", "export class Broken {\n    m() {\n")],
        "[ADD] demo: broken",
    );
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let out = odoo_changelog(&cfg, &["extract"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("gave warnings"), "{output}");

    let db = dir.join("walk.db");
    let at = |rev: &str| js_events(&db, &rev_parse(&clone, rev));
    let ops = |events: &[JsEvent]| -> Vec<(String, String, String)> {
        events
            .iter()
            .map(|(op, kind, q, _, _)| (op.clone(), kind.clone(), q.clone()))
            .collect()
    };
    let row = |op: &str, kind: &str, qualname: &str| (op.to_string(), kind.to_string(), qualname.to_string());

    let added = at("origin/master~5");
    assert_eq!(
        ops(&added),
        [
            row("added", "js_component", "@demo/fields/thing_field#ThingField"),
            row("added", "js_export", "@demo/fields/thing_field#ThingField"),
            row("added", "js_export", "@demo/fields/thing_field#thingField"),
            row("added", "js_module", "@demo/fields/thing_field"),
            row("added", "js_registry", "fields:thing@demo"),
        ]
    );
    let module = added.iter().find(|e| e.1 == "js_module").unwrap();
    let imports = module.4.as_deref().unwrap();
    assert!(imports.contains(r#"{"ref":"@demo/fields/utils#helper"}"#), "{imports}");
    let elsewhere = "SELECT COUNT(*) FROM events WHERE path IN (?1, ?2)";
    let unclaimed = ["addons/demo/static/tests/t.js", "addons/demo/static/lib/l.js"];
    assert_eq!(count(&db, elsewhere, unclaimed), 0);

    assert_eq!(at("origin/master~4"), []);
    assert_eq!(
        ops(&at("origin/master~3")),
        [row("changed", "js_component", "@demo/fields/thing_field#ThingField")]
    );

    let renamed = at("origin/master~2");
    let registry = renamed.iter().find(|e| e.1 == "js_registry").unwrap();
    assert_eq!(
        (registry.0.as_str(), registry.2.as_str(), registry.3.as_deref()),
        ("changed", "fields:thing@demo", Some(field))
    );
    let (new, old) = ("@demo/fields/thing/thing_field", "@demo/fields/thing_field");
    assert_eq!(
        ops(&renamed),
        [
            row("added", "js_component", &format!("{new}#ThingField")),
            row("removed", "js_component", &format!("{old}#ThingField")),
            row("added", "js_export", &format!("{new}#ThingField")),
            row("added", "js_export", &format!("{new}#thingField")),
            row("removed", "js_export", &format!("{old}#ThingField")),
            row("removed", "js_export", &format!("{old}#thingField")),
            row("added", "js_module", new),
            row("removed", "js_module", old),
            row("changed", "js_registry", "fields:thing@demo"),
        ]
    );

    assert_eq!(
        ops(&at("origin/master~1")),
        [
            row("added", "js_export", "demo.thing_legacy#default"),
            row("added", "js_module", "demo.thing_legacy"),
            row("added", "js_patch", "web.FormController@demo"),
            row("added", "js_registry", "web.field_registry:legacy_thing@demo"),
            row("added", "js_widget", "demo.thing_legacy#LegacyThing"),
        ]
    );

    let broken = cached_warnings(&db, &clone, "js", "origin/master:addons/demo/static/src/broken.js");
    assert!(!broken.is_empty());
}

/// `(op, kind, qualname, attrs_after)` of the owl and scss events of `sha`.
fn client_events(db: &Path, sha: &str) -> Vec<(String, String, String, Option<String>)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT e.op, s.kind, s.qualname, e.attrs_after FROM events e JOIN symbols s ON s.id = e.symbol_id
              WHERE e.sha = ?1 AND e.extractor IN ('owl', 'scss') ORDER BY s.kind, s.qualname, e.op",
        )
        .unwrap();
    stmt.query_map([sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

const DEMO_FORM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<templates xml:space="preserve">
    <t t-name="demo.Form">
        <div t-ref="root" class="o_demo_form">
            <button name="save">Save</button>
        </div>
    </t>
</templates>
"#;

const DEMO_FORM_EXT: &str = r#"<templates>
    <t t-inherit="demo.Form" t-inherit-mode="extension">
        <xpath expr="//button[@name='save']" position="after">
            <span class="o_demo_hint">Saved on exit</span>
        </xpath>
    </t>
</templates>
"#;

const DEMO_VARIABLES: &str = "$o-demo-color: #714b67 !default;\n\n@mixin o-demo-mixin($a: 1) {\n    color: $a;\n}\n";

#[test]
fn builtin_owl_and_scss_extractors_record_events() {
    let (dir, mut fx, clone) = setup("extract-owl-scss");
    let form = "addons/demo/static/src/form.xml";
    let ext = "addons/demo_ext/static/src/form.xml";
    let variables = "addons/demo/static/src/scss/primary_variables.scss";
    let theme = "addons/theme_demo/static/src/scss/primary_variables.scss";
    let theme_variables = "$o-demo-color: #017e84;\n";
    fx.commit_files(
        &[
            (form, DEMO_FORM),
            (ext, DEMO_FORM_EXT),
            (variables, DEMO_VARIABLES),
            (theme, theme_variables),
            ("addons/demo/static/tests/form.xml", DEMO_FORM),
            ("addons/demo/static/lib/x.xml", DEMO_FORM),
        ],
        "[ADD] demo: templates and stylesheets",
    );
    let reformatted_form = DEMO_FORM
        .replace("    ", "\t")
        .replace(
            r#"t-ref="root" class="o_demo_form""#,
            "class=\"o_demo_form\"\n t-ref=\"root\"",
        )
        .replace("<button", "<!-- the only action -->\n<button");
    let reformatted_variables = DEMO_VARIABLES
        .replace(": #714b67", ":   #714b67")
        .replace("    color", "  // the colour\n  color");
    fx.commit_files(
        &[(form, &reformatted_form), (variables, &reformatted_variables)],
        "[REF] demo: reformat",
    );
    let saveless = reformatted_form.replace(r#"<button name="save">Save</button>"#, "");
    fx.commit_files(&[(form, &saveless)], "[IMP] demo: no save button");
    fx.commit_files(
        &[(theme, "$o-demo-color: #017e84 !default;\n")],
        "[IMP] theme_demo: default",
    );
    fx.commit_files(
        &[
            (
                "addons/demo/static/src/broken.xml",
                "<templates><t t-name=\"demo.Broken\"></div></templates>\n",
            ),
            ("addons/demo/static/src/scss/broken.scss", "$o-demo-a: 1;\n/* open\n"),
        ],
        "[ADD] demo: broken",
    );
    fetch(&clone);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let out = odoo_changelog(&cfg, &["extract"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(output.contains("gave warnings"), "{output}");

    let db = dir.join("walk.db");
    let at = |rev: &str| client_events(&db, &rev_parse(&clone, rev));
    let ops = |events: &[(String, String, String, Option<String>)]| -> Vec<(String, String, String)> {
        events
            .iter()
            .map(|(op, kind, q, _)| (op.clone(), kind.clone(), q.clone()))
            .collect()
    };
    let row = |op: &str, kind: &str, qualname: &str| (op.to_string(), kind.to_string(), qualname.to_string());

    let added = at("origin/master~4");
    assert_eq!(
        ops(&added),
        [
            row("added", "owl_extension", "demo.Form@demo_ext"),
            row("added", "owl_node", "demo.Form#button[@name='save']@demo"),
            row("added", "owl_node", "demo.Form#div[@t-ref='root']@demo"),
            row("added", "owl_template", "demo.Form@demo"),
            row("added", "scss_mixin", "o-demo-mixin@demo"),
            row("added", "scss_variable", "$o-demo-color@demo"),
            row("added", "scss_variable", "$o-demo-color@theme_demo"),
        ]
    );
    let extension = added[0].3.as_deref().unwrap();
    for part in [r#""inherit":"demo.Form""#, r#""mode":"extension""#] {
        assert!(extension.contains(part), "{extension}");
    }
    assert!(added[5].3.as_deref().unwrap().contains(r#""default":true"#));
    assert!(!added[6].3.as_deref().unwrap().contains("default"));
    let elsewhere = "SELECT COUNT(*) FROM events WHERE path IN (?1, ?2)";
    let unclaimed = ["addons/demo/static/tests/form.xml", "addons/demo/static/lib/x.xml"];
    assert_eq!(count(&db, elsewhere, unclaimed), 0);

    assert_eq!(at("origin/master~3"), []);
    assert_eq!(
        ops(&at("origin/master~2")),
        [
            row("removed", "owl_node", "demo.Form#button[@name='save']@demo"),
            row("changed", "owl_template", "demo.Form@demo"),
        ]
    );
    assert_eq!(
        ops(&at("origin/master~1")),
        [row("changed", "scss_variable", "$o-demo-color@theme_demo")]
    );

    let broken =
        |extractor: &str, path: &str| cached_warnings(&db, &clone, extractor, &format!("origin/master:{path}"));
    let xml = broken("owl", "addons/demo/static/src/broken.xml");
    assert!(xml.len() == 1 && xml[0].starts_with("line 1: "), "{xml:?}");
    assert_eq!(
        broken("scss", "addons/demo/static/src/scss/broken.scss"),
        ["line 2: unclosed comment"]
    );
    assert_eq!(
        ops(&at("origin/master")),
        [row("added", "scss_variable", "$o-demo-a@demo")]
    );
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
    // Prints the run's counts, the file sides that gave warnings among them.
    let _ = tracing_subscriber::fmt().with_target(false).try_init();

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
        let t = run(&cfg, &store, &Registry::builtin(), &opts).unwrap();
        println!("{run_name}");
        for (phase, spent) in [
            ("setup", t.setup),
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
