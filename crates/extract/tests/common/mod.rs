//! Renders an extractor's output and checks it against an expected file.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use odoo_changelog_core::Config;
use odoo_changelog_extract::{Delta, Extractor, Side, Span, Symbol, SymbolTable, delta};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct Row<'a> {
    kind: &'a str,
    qualname: String,
    line: u32,
    span: Span,
    attrs: Cow<'a, BTreeMap<String, Value>>,
}

#[derive(Serialize)]
struct Rendered<'a> {
    symbols: Vec<Row<'a>>,
    warnings: &'a [String],
}

/// The symbols `ex` finds in `blob` and keeps at `path`, qualified there with their attrs, in
/// `(kind, qualname)` order, and its warnings, as pretty JSON with a final newline.
pub fn render(ex: &dyn Extractor, path: &str, blob: &[u8]) -> String {
    let table = ex.extract(blob).unwrap();
    let mut symbols: Vec<Row> = table
        .iter()
        .filter(|s| ex.keeps(path, s))
        .map(|s| Row {
            kind: &s.kind,
            qualname: ex.qualify(path, s),
            line: s.line,
            span: s.span,
            attrs: ex.qualify_attrs(path, s),
        })
        .collect();
    symbols.sort_by(|a, b| (a.kind, &a.qualname).cmp(&(b.kind, &b.qualname)));
    let rendered = Rendered {
        symbols,
        warnings: table.warnings(),
    };
    serde_json::to_string_pretty(&rendered).unwrap() + "\n"
}

/// The `*.<ext>` files of `tests/fixtures/<dir>`, sorted.
pub fn fixtures(dir: &str, ext: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    files.sort();
    files
}

pub fn find<'a>(table: &'a SymbolTable, kind: &str, name: &str) -> &'a Symbol {
    table
        .iter()
        .find(|s| s.kind == kind && s.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no {kind} {name} in {:?}",
                table.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        })
}

/// The clones in `config/odoo-changelog.toml`, by repo name, for the ignored tests that read them.
pub fn clones() -> Vec<(String, PathBuf)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/odoo-changelog.toml");
    let cfg = Config::load(&path).unwrap();
    cfg.repos.into_iter().map(|r| (r.name, r.path)).collect()
}

/// The stdout of a read-only `git -C <clone> <args>`.
pub fn git(clone: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git").arg("-C").arg(clone).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// The blobs `oids` of `clone`, in order, through one read-only `git cat-file --batch`.
pub fn batch(clone: &Path, oids: &[&str]) -> Vec<Vec<u8>> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;

    let mut child = Command::new("git")
        .arg("-C")
        .arg(clone)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input: String = oids.iter().map(|oid| format!("{oid}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()).unwrap());
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut blobs = Vec::with_capacity(oids.len());
    for oid in oids {
        let mut header = String::new();
        stdout.read_line(&mut header).unwrap();
        let size: usize = match header.split_whitespace().collect::<Vec<_>>()[..] {
            [_, "blob", size] => size.parse().unwrap(),
            _ => panic!("{oid}: {header}"),
        };
        let mut blob = vec![0; size + 1];
        stdout.read_exact(&mut blob).unwrap();
        assert_eq!(blob.pop(), Some(b'\n'), "{oid}: no newline after the blob");
        blobs.push(blob);
    }
    writer.join().unwrap();
    assert!(child.wait().unwrap().success());
    blobs
}

/// The blob `oid` of the clone of `repo`.
pub fn odoo_blob(repo: &str, oid: &str) -> Vec<u8> {
    let (_, clone) = clones().into_iter().find(|(name, _)| name == repo).unwrap();
    git(&clone, &["cat-file", "blob", oid])
}

/// `src` with each `(from, to)` replaced in turn, failing when a `from` is not there, so that
/// an edit a test relies on cannot silently stop applying.
pub fn edit(src: &str, edits: &[(&str, &str)]) -> String {
    let mut out = src.to_string();
    for (from, to) in edits {
        assert!(out.contains(from), "{from:?} not found");
        out = out.replace(from, to);
    }
    out
}

/// With `ODOO_CHANGELOG_BLESS=1`, writes `actual` to `expected` instead of comparing them.
pub fn check(expected: &Path, actual: &str) {
    if std::env::var_os("ODOO_CHANGELOG_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(expected, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(expected)
        .unwrap_or_else(|e| panic!("{}: {e}; ODOO_CHANGELOG_BLESS=1 writes it", expected.display()));
    if want == actual {
        return;
    }
    let (want_lines, actual_lines): (Vec<&str>, Vec<&str>) = (want.lines().collect(), actual.lines().collect());
    let at = want_lines
        .iter()
        .zip(&actual_lines)
        .position(|(w, a)| w != a)
        .unwrap_or(want_lines.len().min(actual_lines.len()));
    let around = |lines: &[&str]| lines[at.saturating_sub(3)..(at + 4).min(lines.len())].join("\n");
    panic!(
        "{} differs from line {}; ODOO_CHANGELOG_BLESS=1 rewrites it\n--- expected\n{}\n--- actual\n{}",
        expected.display(),
        at + 1,
        around(&want_lines),
        around(&actual_lines),
    );
}

/// The deltas `ex` gives for `commit` against its parent in the community clone, each file read
/// on both sides it exists on, for the ignored tests that read it.
pub fn commit_deltas(ex: &dyn Extractor, commit: &str) -> Vec<Delta> {
    let (_, clone) = clones().into_iter().find(|(name, _)| name == "odoo").unwrap();
    let parent = format!("{commit}^");
    let listing = git(
        &clone,
        &["diff-tree", "-r", "--no-renames", "--name-status", &parent, commit],
    );
    let changed: Vec<(String, String)> = String::from_utf8(listing)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(status, path)| (status.to_string(), path.to_string()))
        .filter(|(_, path)| ex.claims(path))
        .collect();
    let table = |rev: &str, path: &str| ex.extract(&git(&clone, &["show", &format!("{rev}:{path}")])).unwrap();
    let tables: Vec<(Option<SymbolTable>, Option<SymbolTable>)> = changed
        .iter()
        .map(|(status, path)| {
            let old = (status != "A").then(|| table(&parent, path));
            let new = (status != "D").then(|| table(commit, path));
            (old, new)
        })
        .collect();
    let files: Vec<(Side, Side)> = changed
        .iter()
        .zip(&tables)
        .map(|((_, path), (old, new))| {
            (
                old.as_ref().map(|t| (path.as_str(), t)),
                new.as_ref().map(|t| (path.as_str(), t)),
            )
        })
        .collect();
    delta(ex, &files)
}
