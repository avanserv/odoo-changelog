use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::{Extractor, Symbol, SymbolId, SymbolTable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Added,
    Removed,
    /// In both sides of one file with different attrs.
    Changed,
    /// At another path than before: across a renamed file, or removed from one file and added to
    /// another under the same qualname once each side's `@<module>` suffix is stripped.
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

/// One side of a changed file, `None` when the file does not exist on that side.
pub type Side<'a> = Option<(&'a str, &'a SymbolTable)>;

/// One symbol-level change in one commit.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub id: SymbolId,
    pub kind: String,
    pub qualname: String,
    pub op: Op,
    /// The new side's path, or the old side's for a removal.
    pub path: String,
    pub module: Option<String>,
    /// The new side's line, or the old side's for a removal.
    pub line: u32,
    /// Where a moved symbol was.
    pub old: Option<Origin>,
    /// Canonical JSON attrs: the whole side for an addition or a removal, and for a change or a
    /// move only the entries whose value differs, down into nested objects; both `None` for a
    /// move with equal attrs.
    pub attrs_before: Option<String>,
    pub attrs_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    pub id: SymbolId,
    pub qualname: String,
    pub path: String,
    pub module: Option<String>,
}

/// The changes one commit makes to the files one extractor reads, each an `(old, new)` pair, in
/// `(path, kind, qualname)` order. Each side is qualified with its own path, attrs included, and
/// compared after that. A symbol removed from one file and added to another is one move only when
/// it is alone in this commit to leave and to enter under its kind and unscoped name.
pub fn delta(ex: &dyn Extractor, files: &[(Side, Side)]) -> Vec<Delta> {
    let mut out = Vec::new();
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for &(old, new) in files {
        let mut before = qualified(ex, old);
        for (key, after) in qualified(ex, new) {
            match before.remove_entry(&key) {
                None => added.push((key, after)),
                Some(b) if b.1.path != after.path => out.push(moved(ex, b, (key, after))),
                Some((_, b)) if b.attrs != after.attrs => {
                    let (attrs_before, attrs_after) = changed_keys(&b.attrs, &after.attrs);
                    out.push(event(ex, Op::Changed, key, &after, attrs_before, attrs_after, None));
                }
                Some(_) => {}
            }
        }
        removed.extend(before);
    }

    let pairs: Vec<(usize, usize)> = {
        let mut groups: Groups = BTreeMap::new();
        for (i, ((kind, qualname), f)) in removed.iter().enumerate() {
            groups
                .entry((kind, unscoped(ex, f.path, qualname)))
                .or_default()
                .0
                .push(i);
        }
        for (i, ((kind, qualname), f)) in added.iter().enumerate() {
            groups
                .entry((kind, unscoped(ex, f.path, qualname)))
                .or_default()
                .1
                .push(i);
        }
        groups
            .into_values()
            .filter_map(|(r, a)| match (r.as_slice(), a.as_slice()) {
                (&[r], &[a]) => Some((r, a)),
                _ => None,
            })
            .collect()
    };
    let mut removed: Vec<_> = removed.into_iter().map(Some).collect();
    let mut added: Vec<_> = added.into_iter().map(Some).collect();
    for (r, a) in pairs {
        let (r, a) = (removed[r].take(), added[a].take());
        out.push(moved(ex, r.expect("paired once"), a.expect("paired once")));
    }
    for (key, f) in added.into_iter().flatten() {
        let attrs = Some(json(&f.attrs));
        out.push(event(ex, Op::Added, key, &f, None, attrs, None));
    }
    for (key, f) in removed.into_iter().flatten() {
        let attrs = Some(json(&f.attrs));
        out.push(event(ex, Op::Removed, key, &f, attrs, None, None));
    }
    out.sort_by(|a, b| (&a.path, &a.kind, &a.qualname).cmp(&(&b.path, &b.kind, &b.qualname)));
    out
}

/// A symbol on one side of one file, under its `(kind, qualname)`.
struct Found<'a> {
    path: &'a str,
    line: u32,
    attrs: Cow<'a, BTreeMap<String, Value>>,
}

type Key = (String, String);

/// The indexes of the removed and of the added symbols under each `(kind, unscoped qualname)`.
type Groups<'a> = BTreeMap<(&'a str, &'a str), (Vec<usize>, Vec<usize>)>;

/// The symbols `keeps` lets through, keeping the first when `qualify` gives two the same name.
fn qualified<'a>(ex: &dyn Extractor, side: Side<'a>) -> BTreeMap<Key, Found<'a>> {
    let mut out = BTreeMap::new();
    if let Some((path, table)) = side {
        for symbol in table.iter().filter(|s| ex.keeps(path, s)) {
            out.entry((symbol.kind.clone(), ex.qualify(path, symbol)))
                .or_insert_with(|| found(ex, path, symbol));
        }
    }
    out
}

fn found<'a>(ex: &dyn Extractor, path: &'a str, symbol: &'a Symbol) -> Found<'a> {
    Found {
        path,
        line: symbol.line,
        attrs: ex.qualify_attrs(path, symbol),
    }
}

/// `qualname` without the `@<module>` that `qualify` appends for the file at `path`.
fn unscoped<'q>(ex: &dyn Extractor, path: &str, qualname: &'q str) -> &'q str {
    let module = ex.module(path).unwrap_or_default();
    qualname
        .strip_suffix(module)
        .and_then(|q| q.strip_suffix('@'))
        .unwrap_or(qualname)
}

fn moved(ex: &dyn Extractor, (old_key, before): (Key, Found), (key, after): (Key, Found)) -> Delta {
    let (attrs_before, attrs_after) = changed_keys(&before.attrs, &after.attrs);
    let origin = Origin {
        id: SymbolId::new(&old_key.0, &old_key.1),
        qualname: old_key.1,
        path: before.path.to_string(),
        module: ex.module(before.path).map(str::to_string),
    };
    event(ex, Op::Moved, key, &after, attrs_before, attrs_after, Some(origin))
}

fn event(
    ex: &dyn Extractor,
    op: Op,
    (kind, qualname): Key,
    at: &Found,
    attrs_before: Option<String>,
    attrs_after: Option<String>,
    old: Option<Origin>,
) -> Delta {
    Delta {
        id: SymbolId::new(&kind, &qualname),
        kind,
        qualname,
        op,
        path: at.path.to_string(),
        module: ex.module(at.path).map(str::to_string),
        line: at.line,
        old,
        attrs_before,
        attrs_after,
    }
}

/// The attrs whose value differs between the sides, `None` on both sides when they are equal.
fn changed_keys(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> (Option<String>, Option<String>) {
    if before == after {
        return (None, None);
    }
    let object = |attrs: &BTreeMap<String, Value>| Value::Object(attrs.clone().into_iter().collect());
    let (b, a) = differing(&object(before), &object(after));
    (Some(b.to_string()), Some(a.to_string()))
}

/// Each side's entries whose value differs from the other's, down into the objects both sides
/// hold: a key only one side has shows on that side only.
fn differing(before: &Value, after: &Value) -> (Value, Value) {
    let (Value::Object(b), Value::Object(a)) = (before, after) else {
        return (before.clone(), after.clone());
    };
    let (mut out_b, mut out_a) = (Map::new(), Map::new());
    for (key, bv) in b {
        match a.get(key) {
            None => {
                out_b.insert(key.clone(), bv.clone());
            }
            Some(av) if av != bv => {
                let (bv, av) = differing(bv, av);
                out_b.insert(key.clone(), bv);
                out_a.insert(key.clone(), av);
            }
            Some(_) => {}
        }
    }
    for (key, av) in a.iter().filter(|(k, _)| !b.contains_key(*k)) {
        out_a.insert(key.clone(), av.clone());
    }
    (Value::Object(out_b), Value::Object(out_a))
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

    /// Suffixes `@<parent directory>`, which is also the module, like Odoo's model members.
    struct AtDir;

    impl Extractor for AtDir {
        fn name(&self) -> &'static str {
            "at-dir"
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
            format!("{}@{}", symbol.name, self.module(path).unwrap_or_default())
        }
        fn module<'a>(&self, path: &'a str) -> Option<&'a str> {
            path.rsplit_once('/').map(|(d, _)| d)
        }
    }

    fn symbol(kind: &str, name: &str, line: u32, attrs: &[(&str, Value)]) -> Symbol {
        Symbol {
            kind: kind.into(),
            name: name.into(),
            line,
            span: Span {
                start: line * 10,
                end: line * 10 + 1,
            },
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }

    fn table(defs: &[(&str, u32, &str)]) -> SymbolTable {
        let mut t = SymbolTable::default();
        for (name, line, body) in defs {
            t.insert(symbol("def", name, *line, &[("body", Value::from(*body))]));
        }
        t
    }

    /// `delta` of a commit changing one file.
    fn one(ex: &dyn Extractor, old: Side, new: Side) -> Vec<Delta> {
        delta(ex, &[(old, new)])
    }

    fn ops(deltas: &[Delta]) -> Vec<(&str, &str)> {
        deltas.iter().map(|d| (d.qualname.as_str(), d.op.as_str())).collect()
    }

    fn attrs(d: &Delta) -> (Option<&str>, Option<&str>) {
        (d.attrs_before.as_deref(), d.attrs_after.as_deref())
    }

    #[test]
    fn every_op() {
        let old = table(&[("gone", 1, "x"), ("kept", 2, "x"), ("edited", 3, "x")]);
        let new = table(&[("kept", 9, "x"), ("edited", 3, "y"), ("fresh", 4, "x")]);
        let d = one(&ByDir, Some(("a/f.py", &old)), Some(("a/f.py", &new)));
        assert_eq!(
            ops(&d),
            [("a:edited", "changed"), ("a:fresh", "added"), ("a:gone", "removed")]
        );
        assert_eq!(attrs(&d[0]), (Some(r#"{"body":"x"}"#), Some(r#"{"body":"y"}"#)));
        assert_eq!(attrs(&d[1]), (None, Some(r#"{"body":"x"}"#)));
        assert_eq!(attrs(&d[2]), (Some(r#"{"body":"x"}"#), None));
        assert_eq!(d[2].line, 1);
        assert_eq!(d[0].id, SymbolId::new("def", "a:edited"));
        assert!(
            d.iter()
                .all(|d| d.path == "a/f.py" && d.module.as_deref() == Some("a") && d.old.is_none())
        );
    }

    #[test]
    fn changed_keeps_only_the_keys_that_differ() {
        let mut old = SymbolTable::default();
        old.insert(symbol(
            "field",
            "f",
            1,
            &[
                ("type", "char".into()),
                ("required", false.into()),
                ("help", "h".into()),
            ],
        ));
        let mut new = SymbolTable::default();
        new.insert(symbol(
            "field",
            "f",
            1,
            &[
                ("type", "char".into()),
                ("required", true.into()),
                ("index", true.into()),
            ],
        ));
        let d = one(&ByDir, Some(("a/f.py", &old)), Some(("a/f.py", &new)));
        assert_eq!(ops(&d), [("a:f", "changed")]);
        assert_eq!(
            attrs(&d[0]),
            (
                Some(r#"{"help":"h","required":false}"#),
                Some(r#"{"index":true,"required":true}"#)
            )
        );
    }

    #[test]
    fn changed_keeps_only_the_nested_keys_that_differ() {
        let field = |args: Value| {
            let mut t = SymbolTable::default();
            t.insert(symbol("field", "f", 1, &[("type", "Char".into()), ("args", args)]));
            t
        };
        let old = field(serde_json::json!({"string": "F", "required": false, "size": 5}));
        let new = field(serde_json::json!({"string": "F", "required": true, "index": true}));
        let d = one(&ByDir, Some(("a/f.py", &old)), Some(("a/f.py", &new)));
        assert_eq!(
            attrs(&d[0]),
            (
                Some(r#"{"args":{"required":false,"size":5}}"#),
                Some(r#"{"args":{"index":true,"required":true}}"#)
            )
        );
    }

    #[test]
    fn span_and_line_edits_are_no_event() {
        let old = table(&[("f", 1, "x")]);
        let new = table(&[("f", 7, "x")]);
        assert_ne!(old.iter().next().unwrap().span, new.iter().next().unwrap().span);
        assert!(one(&ByDir, Some(("a/f.py", &old)), Some(("a/f.py", &new))).is_empty());
    }

    #[test]
    fn rename_within_a_directory_moves() {
        let old = table(&[("f", 1, "x"), ("g", 2, "x")]);
        let new = table(&[("f", 1, "x"), ("g", 2, "y")]);
        let d = one(&ByDir, Some(("a/old.py", &old)), Some(("a/new.py", &new)));
        assert_eq!(ops(&d), [("a:f", "moved"), ("a:g", "moved")]);
        assert_eq!(attrs(&d[0]), (None, None));
        assert_eq!(attrs(&d[1]), (Some(r#"{"body":"x"}"#), Some(r#"{"body":"y"}"#)));
        let old = d[0].old.as_ref().unwrap();
        assert_eq!((old.path.as_str(), old.id), ("a/old.py", d[0].id));
    }

    #[test]
    fn rename_across_directories_qualifies_each_side_with_its_path() {
        let t = table(&[("f", 1, "x")]);
        let d = one(&ByDir, Some(("a/f.py", &t)), Some(("b/f.py", &t)));
        assert_eq!(ops(&d), [("a:f", "removed"), ("b:f", "added")]);
    }

    #[test]
    fn a_symbol_leaving_one_file_for_another_moves() {
        let t = table(&[("f", 1, "x"), ("g", 2, "x")]);
        let rest = table(&[("g", 2, "x")]);
        let target = table(&[("f", 5, "y")]);
        let d = delta(
            &ByDir,
            &[
                (Some(("a/x.py", &t)), Some(("a/x.py", &rest))),
                (None, Some(("a/y.py", &target))),
            ],
        );
        assert_eq!(ops(&d), [("a:f", "moved")]);
        assert_eq!((d[0].path.as_str(), d[0].line), ("a/y.py", 5));
        assert_eq!(d[0].old.as_ref().unwrap().path, "a/x.py");
        assert_eq!(attrs(&d[0]), (Some(r#"{"body":"x"}"#), Some(r#"{"body":"y"}"#)));
    }

    #[test]
    fn a_symbol_leaving_one_module_for_another_moves() {
        let t = table(&[("f", 1, "x")]);
        let d = delta(
            &AtDir,
            &[
                (Some(("sale/x.py", &t)), None),
                (None, Some(("sale_management/x.py", &t))),
            ],
        );
        assert_eq!(ops(&d), [("f@sale_management", "moved")]);
        assert_eq!(d[0].module.as_deref(), Some("sale_management"));
        assert_eq!(
            d[0].old,
            Some(Origin {
                id: SymbolId::new("def", "f@sale"),
                qualname: "f@sale".into(),
                path: "sale/x.py".into(),
                module: Some("sale".into()),
            })
        );
        assert_eq!(attrs(&d[0]), (None, None));
    }

    #[test]
    fn ambiguous_moves_stay_removals_and_additions() {
        let t = table(&[("f", 1, "x")]);
        let d = delta(
            &AtDir,
            &[
                (Some(("a/x.py", &t)), None),
                (None, Some(("b/x.py", &t))),
                (None, Some(("c/x.py", &t))),
            ],
        );
        assert_eq!(ops(&d), [("f@a", "removed"), ("f@b", "added"), ("f@c", "added")]);
    }

    #[test]
    fn kinds_never_pair() {
        let mut old = SymbolTable::default();
        old.insert(symbol("field", "f", 1, &[]));
        let mut new = SymbolTable::default();
        new.insert(symbol("method", "f", 1, &[]));
        let d = delta(
            &AtDir,
            &[(Some(("a/x.py", &old)), None), (None, Some(("b/x.py", &new)))],
        );
        assert_eq!(ops(&d), [("f@a", "removed"), ("f@b", "added")]);
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
        let d = one(&RefByDir, Some(("a/f.py", &t)), Some(("b/f.py", &t)));
        assert_eq!(ops(&d), [("f", "moved")]);
        assert_eq!(attrs(&d[0]), (Some(r#"{"ref":"a"}"#), Some(r#"{"ref":"b"}"#)));

        let d = one(&RefByDir, Some(("a/f.py", &t)), Some(("a/g.py", &t)));
        assert_eq!(ops(&d), [("f", "moved")]);
        let removed = one(&RefByDir, Some(("a/f.py", &t)), None);
        assert_eq!(attrs(&removed[0]).0, Some(r#"{"body":"x","ref":"a"}"#));
    }

    /// Keeps only `kept` under `t/`.
    struct KeepsUnderT;

    impl Extractor for KeepsUnderT {
        fn name(&self) -> &'static str {
            "keeps-under-t"
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
        fn keeps(&self, path: &str, symbol: &Symbol) -> bool {
            !path.starts_with("t/") || symbol.name == "kept"
        }
    }

    #[test]
    fn keeps_filters_each_side_by_its_path() {
        let t = table(&[("kept", 1, "x"), ("other", 2, "x")]);
        let d = one(&KeepsUnderT, Some(("s/f.js", &t)), Some(("t/f.js", &t)));
        assert_eq!(ops(&d), [("other", "removed"), ("kept", "moved")]);
        assert_eq!(ops(&one(&KeepsUnderT, None, Some(("t/f.js", &t)))), [("kept", "added")]);
    }

    #[test]
    fn missing_side_adds_or_removes_everything() {
        let t = table(&[("f", 1, "x")]);
        assert_eq!(ops(&one(&ByDir, None, Some(("a/f.py", &t)))), [("a:f", "added")]);
        assert_eq!(ops(&one(&ByDir, Some(("a/f.py", &t)), None)), [("a:f", "removed")]);
    }
}
