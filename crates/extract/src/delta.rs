use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::{Extractor, Symbol, SymbolId, SymbolTable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Added,
    Removed,
    /// In both sides with different attrs.
    Changed,
    /// In both sides with equal attrs, across a renamed file. Keeps replayed events on the file
    /// each symbol lives in.
    Moved,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Added => "added",
            Op::Removed => "removed",
            Op::Changed => "changed",
            Op::Moved => "moved",
        }
    }
}

/// One symbol-level change in one file.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub id: SymbolId,
    pub kind: String,
    pub qualname: String,
    pub op: Op,
    /// The new side's line, or the old side's for a removal.
    pub line: u32,
    /// Canonical JSON of the attrs on each side the symbol exists in.
    pub attrs_before: Option<String>,
    pub attrs_after: Option<String>,
}

/// The changes from `old` to `new`, each a `(path, table)` of one file, in `(kind, qualname)`
/// order. Each side is qualified with its own path, attrs included, and compared after that.
pub fn delta(ex: &dyn Extractor, old: Option<(&str, &SymbolTable)>, new: Option<(&str, &SymbolTable)>) -> Vec<Delta> {
    let renamed = matches!((old, new), (Some((a, _)), Some((b, _))) if a != b);
    let mut before = qualified(ex, old);
    let mut out = Vec::new();
    for (key, after) in qualified(ex, new) {
        let (op, attrs_before) = match before.remove(&key) {
            None => (Op::Added, None),
            Some((_, b)) if b != after.1 => (Op::Changed, Some(json(&b))),
            Some((_, b)) if renamed => (Op::Moved, Some(json(&b))),
            Some(_) => continue,
        };
        out.push(Delta {
            id: SymbolId::new(&key.0, &key.1),
            line: after.0.line,
            attrs_before,
            attrs_after: Some(json(&after.1)),
            op,
            kind: key.0,
            qualname: key.1,
        });
    }
    out.extend(before.into_iter().map(|((kind, qualname), (b, attrs))| Delta {
        id: SymbolId::new(&kind, &qualname),
        line: b.line,
        attrs_before: Some(json(&attrs)),
        attrs_after: None,
        op: Op::Removed,
        kind,
        qualname,
    }));
    out.sort_by(|a, b| (&a.kind, &a.qualname).cmp(&(&b.kind, &b.qualname)));
    out
}

type Qualified<'a> = BTreeMap<(String, String), (&'a Symbol, Cow<'a, BTreeMap<String, Value>>)>;

/// Keeps the first symbol when `qualify` gives two the same name.
fn qualified<'a>(ex: &dyn Extractor, side: Option<(&str, &'a SymbolTable)>) -> Qualified<'a> {
    let mut out = Qualified::new();
    if let Some((path, table)) = side {
        for symbol in table.iter() {
            out.entry((symbol.kind.clone(), ex.qualify(path, symbol)))
                .or_insert_with(|| (symbol, ex.qualify_attrs(path, symbol)));
        }
    }
    out
}

fn json(attrs: &BTreeMap<String, Value>) -> String {
    serde_json::to_string(attrs).expect("attrs always serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    /// Prefixes the parent directory, so a rename within a directory keeps qualnames.
    struct ByDir;

    impl Extractor for ByDir {
        fn name(&self) -> &'static str {
            "by-dir"
        }
        fn version(&self) -> u32 {
            1
        }
        fn claims(&self, _: &str) -> bool {
            true
        }
        fn extract(&self, _: &[u8]) -> anyhow::Result<SymbolTable> {
            unreachable!("the tests build tables by hand")
        }
        fn qualify(&self, path: &str, symbol: &Symbol) -> String {
            let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
            format!("{dir}:{}", symbol.name)
        }
    }

    fn table(defs: &[(&str, u32, &str)]) -> SymbolTable {
        let mut t = SymbolTable::default();
        for (name, line, body) in defs {
            t.insert(Symbol {
                kind: "def".into(),
                name: (*name).into(),
                line: *line,
                span: Span {
                    start: *line * 10,
                    end: *line * 10 + 1,
                },
                attrs: [("body".to_string(), Value::from(*body))].into(),
            });
        }
        t
    }

    fn ops(deltas: &[Delta]) -> Vec<(&str, &str)> {
        deltas.iter().map(|d| (d.qualname.as_str(), d.op.as_str())).collect()
    }

    #[test]
    fn every_op() {
        let old = table(&[("gone", 1, "x"), ("kept", 2, "x"), ("edited", 3, "x")]);
        let new = table(&[("kept", 9, "x"), ("edited", 3, "y"), ("fresh", 4, "x")]);
        let d = delta(&ByDir, Some(("a/f.py", &old)), Some(("a/f.py", &new)));
        assert_eq!(
            ops(&d),
            [("a:edited", "changed"), ("a:fresh", "added"), ("a:gone", "removed")]
        );
        let edited = &d[0];
        assert_eq!(edited.attrs_before.as_deref(), Some(r#"{"body":"x"}"#));
        assert_eq!(edited.attrs_after.as_deref(), Some(r#"{"body":"y"}"#));
        assert_eq!(d[2].line, 1);
        assert_eq!((d[2].attrs_before.is_some(), d[2].attrs_after.is_none()), (true, true));
        assert_eq!(d[0].id, SymbolId::new("def", "a:edited"));
    }

    #[test]
    fn span_and_line_edits_are_no_event() {
        let old = table(&[("f", 1, "x")]);
        let new = table(&[("f", 7, "x")]);
        assert_ne!(old.iter().next().unwrap().span, new.iter().next().unwrap().span);
        assert!(delta(&ByDir, Some(("a/f.py", &old)), Some(("a/f.py", &new))).is_empty());
    }

    #[test]
    fn rename_within_a_directory_moves() {
        let old = table(&[("f", 1, "x"), ("g", 2, "x")]);
        let new = table(&[("f", 1, "x"), ("g", 2, "y")]);
        let d = delta(&ByDir, Some(("a/old.py", &old)), Some(("a/new.py", &new)));
        assert_eq!(ops(&d), [("a:f", "moved"), ("a:g", "changed")]);
    }

    #[test]
    fn rename_across_directories_qualifies_each_side_with_its_path() {
        let t = table(&[("f", 1, "x")]);
        let d = delta(&ByDir, Some(("a/f.py", &t)), Some(("b/f.py", &t)));
        assert_eq!(ops(&d), [("a:f", "removed"), ("b:f", "added")]);
    }

    /// Qualnames ignore the path, and `ref` holds the parent directory.
    struct RefByDir;

    impl Extractor for RefByDir {
        fn name(&self) -> &'static str {
            "ref-by-dir"
        }
        fn version(&self) -> u32 {
            1
        }
        fn claims(&self, _: &str) -> bool {
            true
        }
        fn extract(&self, _: &[u8]) -> anyhow::Result<SymbolTable> {
            unreachable!("the tests build tables by hand")
        }
        fn qualify_attrs<'a>(&self, path: &str, symbol: &'a Symbol) -> Cow<'a, BTreeMap<String, Value>> {
            let mut attrs = symbol.attrs.clone();
            let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
            attrs.insert("ref".into(), dir.into());
            Cow::Owned(attrs)
        }
    }

    #[test]
    fn qualify_attrs_drives_comparison_and_output() {
        let t = table(&[("f", 1, "x")]);
        let d = delta(&RefByDir, Some(("a/f.py", &t)), Some(("b/f.py", &t)));
        assert_eq!(ops(&d), [("f", "changed")]);
        assert_eq!(d[0].attrs_before.as_deref(), Some(r#"{"body":"x","ref":"a"}"#));
        assert_eq!(d[0].attrs_after.as_deref(), Some(r#"{"body":"x","ref":"b"}"#));

        let d = delta(&RefByDir, Some(("a/f.py", &t)), Some(("a/g.py", &t)));
        assert_eq!(ops(&d), [("f", "moved")]);
        let removed = delta(&RefByDir, Some(("a/f.py", &t)), None);
        assert_eq!(removed[0].attrs_before.as_deref(), Some(r#"{"body":"x","ref":"a"}"#));
    }

    #[test]
    fn missing_side_adds_or_removes_everything() {
        let t = table(&[("f", 1, "x")]);
        assert_eq!(ops(&delta(&ByDir, None, Some(("a/f.py", &t)))), [("a:f", "added")]);
        assert_eq!(ops(&delta(&ByDir, Some(("a/f.py", &t)), None)), [("a:f", "removed")]);
    }
}
