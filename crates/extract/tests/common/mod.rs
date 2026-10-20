//! Renders an extractor's output and checks it against an expected file.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use odoo_changelog_extract::{Extractor, Span, Symbol, SymbolTable};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct Row<'a> {
    kind: &'a str,
    qualname: String,
    line: u32,
    span: Span,
    attrs: &'a BTreeMap<String, Value>,
}

#[derive(Serialize)]
struct Rendered<'a> {
    symbols: Vec<Row<'a>>,
    warnings: &'a [String],
}

/// The symbols `ex` finds in `blob`, qualified at `path`, in `(kind, qualname)` order, and its
/// warnings, as pretty JSON with a final newline.
pub fn render(ex: &dyn Extractor, path: &str, blob: &[u8]) -> String {
    let table = ex.extract(blob).unwrap();
    let mut symbols: Vec<Row> = table
        .iter()
        .map(|s| Row {
            kind: &s.kind,
            qualname: ex.qualify(path, s),
            line: s.line,
            span: s.span,
            attrs: &s.attrs,
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
