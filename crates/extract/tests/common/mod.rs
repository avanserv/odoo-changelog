//! Renders an extractor's output and checks it against an expected file.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use odoo_changelog_extract::{Extractor, Python, Span};
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

/// The Python symbols of `blob` qualified at `path`, in `(kind, qualname)` order, and its
/// warnings, as pretty JSON with a final newline.
pub fn render(path: &str, blob: &[u8]) -> String {
    let table = Python.extract(blob).unwrap();
    let mut symbols: Vec<Row> = table
        .iter()
        .map(|s| Row {
            kind: &s.kind,
            qualname: Python.qualify(path, s),
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

/// With `ODOO_CHANGELOG_BLESS=1`, writes `actual` to `expected` instead of comparing them.
pub fn check(expected: &Path, actual: &str) {
    if std::env::var_os("ODOO_CHANGELOG_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(expected, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(expected)
        .unwrap_or_else(|e| panic!("{}: {e}; ODOO_CHANGELOG_BLESS=1 writes it", expected.display()));
    assert_eq!(
        want,
        actual,
        "{} differs; ODOO_CHANGELOG_BLESS=1 rewrites it",
        expected.display()
    );
}
