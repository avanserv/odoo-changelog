//! An Odoo XML data file as `odoo/tools/convert.py` loads it: every declaration by xmlid, with
//! the declarations of one id in the file merged as Odoo updates the record, and each view's
//! named nodes as symbols of their own.

pub(crate) mod arch;
pub(crate) mod dom;
pub(crate) mod refs;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::bound::{Budget, bounded};
use crate::data::{ACTION, DATA_FUNCTION, DELETE, MENU, RECORD, VIEW, VIEW_NODE, kind_of};
use crate::hash::digest;
use crate::{Parts, Span, Symbol, SymbolTable};
use dom::{Attr, Element, Node, elements};
use refs::Refs;

const ROOTS: [&str; 3] = ["odoo", "openerp", "data"];

/// The serialized symbols one file may give, per byte of it plus a constant, past which the
/// walker drops the symbols it has yet to give and warns once.
const OUTPUT_PER_BYTE: usize = 24;
const OUTPUT_SLACK: usize = 1 << 20;

pub(crate) fn extract(blob: &[u8]) -> SymbolTable {
    let doc = dom::parse(blob, |tag| ROOTS.contains(&tag));
    let mut walker = Walker {
        decls: BTreeMap::new(),
        calls: BTreeMap::new(),
        warnings: doc.warnings,
    };
    let empty = blob
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(blob)
        .trim_ascii()
        .is_empty();
    match &doc.root {
        Some(root) if ROOTS.contains(&root.tag.as_str()) => walker.root(root, noupdate_of(root, false)),
        // Neither a foreign document nor an empty file is a data file Odoo loads.
        Some(_) => {}
        None if empty => {}
        None => walker.warnings.push((1, "line 1: no root element".into())),
    }
    walker.finish(Budget::new(blob, OUTPUT_PER_BYTE, OUTPUT_SLACK))
}

/// Odoo's `nodeattr2bool` and `str2bool`, trimmed and in any case; a value they reject inherits.
fn noupdate_of(root: &Element, inherited: bool) -> bool {
    let value = root.attr("noupdate").map(|v| v.trim().to_ascii_lowercase());
    match value.as_deref() {
        Some("y" | "yes" | "1" | "true" | "t" | "on") => true,
        Some("n" | "no" | "0" | "false" | "f" | "off") => false,
        _ => inherited,
    }
}

/// Everything the declarations of one `(kind, name)` in the file say, merged by name.
struct Decl<'d> {
    /// The tag of the first declaration.
    declared: &'static str,
    record_model: Option<&'d str>,
    noupdate: bool,
    line: u32,
    span: Span,
    /// The declaring elements' attributes but the id, later ones winning, with the line of
    /// their element.
    attrs: BTreeMap<&'d str, (&'d str, u32)>,
    /// A record's fields by name, later ones winning.
    fields: BTreeMap<&'d str, &'d Element>,
    /// The content of a template, asset or call, the last declaration's, and its element's line.
    content: Option<(&'d [Node], u32)>,
}

impl<'d> Decl<'d> {
    fn new(declared: &'static str, at: &'d Attr, el: &'d Element, skip: &[&str], noupdate: bool) -> Self {
        let attrs = el
            .attrs
            .iter()
            .filter(|a| !skip.contains(&a.name.as_str()))
            .map(|a| (a.name.as_str(), (a.value.as_str(), el.line)))
            .collect();
        Decl {
            declared,
            record_model: None,
            noupdate,
            line: at.line,
            span: at.span,
            attrs,
            fields: BTreeMap::new(),
            content: None,
        }
    }
}

struct Walker<'d> {
    decls: BTreeMap<(&'static str, String), Decl<'d>>,
    /// Occurrences of each call and delete by search, for their `[n]` suffix.
    calls: BTreeMap<(&'static str, String), u32>,
    warnings: Vec<(u32, String)>,
}

impl<'d> Walker<'d> {
    fn warn(&mut self, line: u32, warning: impl std::fmt::Display) {
        self.warnings.push((line, format!("line {line}: {warning}")));
    }

    /// An element a syntax error cut short is read only when it is a root, whose complete
    /// children stand, or a menuitem, which its start tag declares in full.
    fn root(&mut self, root: &'d Element, noupdate: bool) {
        for el in root.elements() {
            let tag = el.tag.as_str();
            if !el.complete && !ROOTS.contains(&tag) && tag != "menuitem" {
                continue;
            }
            match tag {
                "odoo" | "openerp" | "data" => self.root(el, noupdate_of(el, noupdate)),
                "record" => self.record(el, noupdate),
                "template" => self.template(el, noupdate),
                "menuitem" => self.menuitem(el, None, noupdate),
                "report" | "act_window" => self.shortcut(el, ACTION, noupdate),
                "asset" => self.shortcut(el, RECORD, noupdate),
                "function" => self.function(el, noupdate),
                "delete" => self.delete(el),
                _ => {}
            }
        }
    }

    /// Records with an id inside a field's content (one2many lines from 18.0) are declared
    /// after their parent; anonymous ones only count in its values.
    fn record(&mut self, el: &'d Element, noupdate: bool) {
        let Some(model) = el.attr("model") else {
            return self.warn(el.line, "record without model ignored");
        };
        let mut fields = BTreeMap::new();
        let mut nested = Vec::new();
        for field in el.elements().filter(|f| f.tag == "field") {
            if let Some(name) = field.attr("name") {
                fields.insert(name, field);
            }
            nested.extend(field.elements().filter(|r| r.tag == "record"));
        }
        if let Some(id) = el.attr_node("id") {
            let mut decl = Decl::new("record", id, el, &["id", "model"], noupdate);
            decl.record_model = Some(model);
            decl.fields = fields;
            self.declare(kind_of(model), id, decl);
        }
        for record in nested {
            self.record(record, noupdate);
        }
    }

    fn template(&mut self, el: &'d Element, noupdate: bool) {
        let Some(id) = el.attr_node("id").or_else(|| el.attr_node("t-name")) else {
            return self.warn(el.line, "template without id ignored");
        };
        let mut decl = Decl::new("template", id, el, &[id.name.as_str()], noupdate);
        decl.content = Some((&el.children, el.line));
        self.declare(VIEW, id, decl);
    }

    /// A nested menuitem takes the enclosing one as parent, whatever its own attribute says.
    fn menuitem(&mut self, el: &'d Element, parent: Option<&'d str>, noupdate: bool) {
        let Some(id) = el.attr_node("id") else {
            return self.warn(el.line, "menuitem without id ignored");
        };
        let mut decl = Decl::new("menuitem", id, el, &["id"], noupdate);
        if let Some(parent) = parent {
            decl.attrs.insert("parent", (parent, el.line));
        }
        self.declare(MENU, id, decl);
        for child in el.elements().filter(|c| c.tag == "menuitem") {
            self.menuitem(child, Some(&id.value), noupdate);
        }
    }

    fn shortcut(&mut self, el: &'d Element, kind: &'static str, noupdate: bool) {
        let declared = match el.tag.as_str() {
            "report" => "report",
            "act_window" => "act_window",
            _ => "asset",
        };
        let Some(id) = el.attr_node("id") else {
            return self.warn(el.line, format!("{declared} without id ignored"));
        };
        let mut decl = Decl::new(declared, id, el, &["id"], noupdate);
        decl.content = Some((&el.children, el.line));
        self.declare(kind, id, decl);
    }

    fn function(&mut self, el: &'d Element, noupdate: bool) {
        let (Some(model), Some(name)) = (el.attr("model"), el.attr_node("name")) else {
            let missing = if el.attr("model").is_none() { "model" } else { "name" };
            return self.warn(el.line, format!("function without {missing} ignored"));
        };
        let mut decl = Decl::new("function", name, el, &[], noupdate);
        decl.content = Some((&el.children, el.line));
        let table_name = self.occurrence(DATA_FUNCTION, format!("{model}#{}", name.value));
        self.insert(DATA_FUNCTION, table_name, decl);
    }

    fn delete(&mut self, el: &'d Element) {
        let Some(model) = el.attr_node("model") else {
            return self.warn(el.line, "delete without model ignored");
        };
        match el.attr_node("id") {
            Some(id) => {
                let decl = Decl::new("delete", id, el, &["id"], false);
                self.declare(DELETE, id, decl);
            }
            None if el.attr("search").is_some() => {
                let decl = Decl::new("delete", model, el, &[], false);
                let name = self.occurrence(DELETE, format!("{}#search", model.value));
                self.insert(DELETE, name, decl);
            }
            None => self.warn(el.line, "delete without id or search ignored"),
        }
    }

    /// `name`, then `name[2]`, `name[3]`, ... for later occurrences in the file.
    fn occurrence(&mut self, kind: &'static str, name: String) -> String {
        let n = self.calls.entry((kind, name.clone())).or_default();
        *n += 1;
        if *n == 1 { name } else { format!("{name}[{n}]") }
    }

    fn declare(&mut self, kind: &'static str, id: &'d Attr, decl: Decl<'d>) {
        if id.value.matches('.').count() > 1 {
            self.warn(id.line, format!("id {} has more than one dot", id.value));
        }
        self.insert(kind, id.value.clone(), decl);
    }

    /// Merges a later declaration of the same `(kind, name)` into the first, as Odoo updates
    /// the record, unless both are records of different models, which Odoo refuses to load. A
    /// later menuitem resets what Odoo derives anew for each one: its parent, name and `active`.
    fn insert(&mut self, kind: &'static str, name: String, decl: Decl<'d>) {
        let key = (kind, name);
        let Some(first) = self.decls.get_mut(&key) else {
            self.decls.insert(key, decl);
            return;
        };
        match first.record_model.zip(decl.record_model) {
            Some((model, other)) if model != other => {
                self.warn(decl.line, format!("id {} is already a {model} record; ignored", key.1));
            }
            _ => {
                if decl.declared == "menuitem" {
                    first
                        .attrs
                        .retain(|key, _| !matches!(*key, "parent" | "name" | "active"));
                }
                first.attrs.extend(decl.attrs);
                first.fields.extend(decl.fields);
                first.content = decl.content.or(first.content);
            }
        }
    }

    /// Each declaration, then its view's nodes, named under its id as `bounded` keeps it since
    /// every node repeats it, in `(kind, name)` order within the budget.
    fn finish(self, mut budget: Budget) -> SymbolTable {
        let mut table = SymbolTable::default();
        let mut keep = |table: &mut SymbolTable, symbol: Symbol| {
            if budget.symbol_at(&symbol, symbol.line as usize) {
                table.insert(symbol);
            }
        };
        let mut spellings: BTreeMap<&str, Spellings> = BTreeMap::new();
        for (kind, name) in self.decls.keys() {
            spellings.entry(kind).or_default().add(name);
        }
        for ((kind, name), decl) in &self.decls {
            let (mut attrs, nodes, parts) = attrs(kind, decl);
            if decl.declared != "delete" {
                references(decl).put(&mut attrs);
            }
            let symbol = Symbol {
                kind: (*kind).into(),
                name: name.clone(),
                line: decl.line,
                span: decl.span,
                attrs,
                parts: parts.filter(|_| spellings[kind].both(name)),
                sketch: None,
            };
            keep(&mut table, symbol);
            let owner = if nodes.is_empty() {
                String::new()
            } else {
                bounded(name.clone())
            };
            for node in nodes {
                let mut node_attrs = BTreeMap::from([("count".to_string(), Value::from(node.count))]);
                if !node.path.is_empty() {
                    node_attrs.insert("path".into(), node.path.into());
                }
                let symbol = Symbol {
                    kind: VIEW_NODE.into(),
                    name: format!("{owner}#{}", node.key),
                    line: node.line,
                    span: node.span,
                    attrs: node_attrs,
                    parts: None,
                    sketch: None,
                };
                keep(&mut table, symbol);
            }
        }
        let mut warnings = self.warnings;
        warnings.extend(budget.warning("symbols").map(|(line, w)| (line as u32, w)));
        warnings.sort_by_key(|(line, _)| *line);
        for (_, warning) in warnings {
            table.warn(warning);
        }
        table
    }
}

/// The ids of one kind a file declares, bare and the local part of the dotted ones, to tell which
/// it writes both ways, `x` beside `<module>.x`, which `qualify` may name alike.
#[derive(Default)]
pub(crate) struct Spellings<'a> {
    bare: BTreeSet<&'a str>,
    dotted: BTreeSet<&'a str>,
}

impl<'a> Spellings<'a> {
    pub(crate) fn add(&mut self, name: &'a str) {
        match name.split_once('.') {
            Some((_, local)) => self.dotted.insert(local),
            None => self.bare.insert(name),
        };
    }

    pub(crate) fn both(&self, name: &str) -> bool {
        match name.split_once('.') {
            Some((_, local)) => self.bare.contains(local),
            None => self.dotted.contains(name),
        }
    }
}

/// The attrs of a declaration, the named nodes of a view, and the parts its `values` hashes.
fn attrs(kind: &str, decl: &Decl) -> (BTreeMap<String, Value>, Vec<arch::NamedNode>, Option<Parts>) {
    let mut out = BTreeMap::new();
    let a = |key: &str| decl.attrs.get(key).map(|(v, _)| Value::from(*v));
    let f = |key: &str| decl.fields.get(key).copied().map(clear);
    let mut nodes = Vec::new();
    match decl.declared {
        "function" => {
            put(&mut out, "model", a("model"));
            put(&mut out, "name", a("name"));
            let parts = parts(decl, &["model", "name"], &[], decl.content);
            put(&mut out, "values", values_of(&parts));
            if decl.noupdate {
                out.insert("noupdate".into(), true.into());
            }
            return (out, nodes, Some(parts));
        }
        "delete" => {
            put(&mut out, "model", a("model"));
            put(&mut out, "search", a("search"));
            return (out, nodes, None);
        }
        _ => {}
    }
    out.insert("declared".into(), decl.declared.into());
    put(&mut out, "record_model", decl.record_model.map(Value::from));
    if decl.noupdate {
        out.insert("noupdate".into(), true.into());
    }
    let parts = match (kind, decl.declared) {
        (VIEW, "template") => {
            let arch = decl.content.map(|(c, _)| c).unwrap_or_default();
            let inheriting = decl.attrs.contains_key("inherit_id");
            put(&mut out, "name", a("name"));
            put(&mut out, "inherit_id", a("inherit_id"));
            put(&mut out, "priority", a("priority"));
            out.insert("type".into(), "qweb".into());
            let mode = match decl.attrs.get("primary") {
                Some(("True", _)) => "primary",
                _ if inheriting => "extension",
                _ => "primary",
            };
            out.insert("mode".into(), mode.into());
            nodes = view_arch(&mut out, arch, inheriting);
            parts(decl, &["name", "inherit_id", "priority", "primary"], &[], None)
        }
        (VIEW, _) => {
            let arch = decl.fields.get("arch").map(|field| field.children.as_slice());
            let inheriting = decl.fields.contains_key("inherit_id");
            for key in ["name", "model", "inherit_id", "priority"] {
                put(&mut out, key, f(key));
            }
            let root = arch.and_then(|arch| elements(arch).next()).filter(|_| !inheriting);
            put(
                &mut out,
                "type",
                f("type").or_else(|| root.map(|el| el.tag.as_str().into())),
            );
            let mode = if inheriting { "extension" } else { "primary" };
            out.insert("mode".into(), f("mode").unwrap_or_else(|| mode.into()));
            if let Some(arch) = arch {
                nodes = view_arch(&mut out, arch, inheriting);
            }
            parts(decl, &[], &["arch"], None)
        }
        (MENU, "menuitem") => {
            for (key, (value, _)) in &decl.attrs {
                let value = match *key {
                    "groups" => value.split(',').map(|g| Value::from(g.trim())).collect(),
                    _ => Value::from(*value),
                };
                out.insert((*key).into(), value);
            }
            return (out, nodes, None);
        }
        (_, "report") => {
            put(&mut out, "name", a("string"));
            put(&mut out, "model", a("model"));
            parts(decl, &["string", "model"], &[], None)
        }
        (_, "act_window") => {
            put(&mut out, "name", a("name"));
            put(&mut out, "res_model", a("res_model"));
            parts(decl, &["name", "res_model"], &[], None)
        }
        (_, "asset") => {
            put(&mut out, "name", a("name"));
            parts(decl, &["name"], &[], decl.content)
        }
        _ => {
            for key in ["name", "model", "res_model"] {
                put(&mut out, key, f(key));
            }
            let model_id = decl.fields.get("model_id").and_then(|field| field.attr("ref"));
            put(&mut out, "model_id", model_id.map(Value::from));
            parts(decl, &[], &[], None)
        }
    };
    put(&mut out, "values", values_of(&parts));
    (out, nodes, Some(parts))
}

/// The xmlids a declaration names: in its own attributes, its fields' `ref`, `eval` and text,
/// and the arch or content it holds, of which an inheriting view's specs only put in their content.
fn references(decl: &Decl) -> Refs {
    let mut refs = Refs::default();
    for (key, (value, _)) in &decl.attrs {
        match *key {
            "groups" => refs.ids(value),
            "parent" | "action" | "inherit_id" => refs.id(value),
            _ => refs.text(value),
        }
    }
    let inheriting = decl.attrs.contains_key("inherit_id") || decl.fields.contains_key("inherit_id");
    let arch = |refs: &mut Refs, arch: &[Node], is_view: bool| {
        if is_view && inheriting {
            refs.specs(arch)
        } else {
            refs.arch(arch)
        }
    };
    for (name, field) in &decl.fields {
        if let Some(r) = field.attr("ref") {
            refs.id(r);
        }
        if let Some(e) = field.attr("eval") {
            refs.text(e);
        }
        if is_markup(field) {
            arch(&mut refs, &field.children, *name == "arch");
        } else {
            refs.text(&text(field));
        }
    }
    if let Some((content, _)) = decl.content {
        arch(&mut refs, content, decl.declared == "template");
    }
    refs
}

fn put(out: &mut BTreeMap<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        out.insert(key.into(), value);
    }
}

/// `arch` and, for an inheriting view, `specs`; the named nodes of the arch, or those its specs
/// add.
fn view_arch(out: &mut BTreeMap<String, Value>, arch: &[Node], inheriting: bool) -> Vec<arch::NamedNode> {
    out.insert("arch".into(), arch::hash(arch).into());
    if !inheriting {
        return arch::named_nodes(arch, arch::VIEW_KEYS);
    }
    let (specs, nodes) = arch::specs(arch, arch::VIEW_KEYS);
    out.insert("specs".into(), arch::specs_value(specs));
    nodes
}

/// A field's value in clear: a reference as written, an expression, or its text, trimmed; markup
/// is hashed like an arch.
fn clear(field: &Element) -> Value {
    if let Some(r) = field.attr("ref") {
        return r.into();
    }
    if let Some(e) = field.attr("eval") {
        return json!({"eval": e});
    }
    if let Some(s) = field.attr("search") {
        return json!({"search": s});
    }
    if is_markup(field) {
        return json!({"markup": arch::hash(&field.children)});
    }
    text(field).trim().into()
}

fn is_markup(field: &Element) -> bool {
    matches!(field.attr("type"), Some("xml" | "html")) || field.elements().next().is_some()
}

fn text(el: &Element) -> String {
    el.children
        .iter()
        .filter_map(|n| match n {
            Node::Text(t) => Some(t.as_str()),
            Node::Element(_) => None,
        })
        .collect()
}

/// What `values` hashes: each of the declaration's attributes but `skip` as `@<name>`, the hash
/// of its value; each field but `skip_fields` by name, those the attrs hold in clear included,
/// the hash of `0x07 name 0x00`, its other attributes as `name 0x02 value 0x00`, `0x03`, its
/// content and `0x04`; and `#content`, the hash of `content` when it has any. Plain field text is
/// hashed as written, since Odoo stores it verbatim (Python code is indentation-sensitive);
/// markup like an arch.
fn parts(decl: &Decl, skip: &[&str], skip_fields: &[&str], content: Option<(&[Node], u32)>) -> Parts {
    let mut out = Parts::new();
    for (name, (value, line)) in decl.attrs.iter().filter(|(n, _)| !skip.contains(n)) {
        let mut hasher = blake3::Hasher::new();
        hasher.update(value.as_bytes());
        out.insert(format!("@{name}"), (*line, digest(&hasher)));
    }
    for (name, field) in decl.fields.iter().filter(|(n, _)| !skip_fields.contains(n)) {
        let mut attrs: Vec<(&str, &str)> = field
            .attrs
            .iter()
            .filter(|a| a.name != "name")
            .map(|a| (a.name.as_str(), a.value.as_str()))
            .collect();
        attrs.sort();
        let mut hasher = blake3::Hasher::new();
        if is_markup(field) {
            feed_field_with(&mut hasher, name, &attrs, |h| arch::feed(h, &field.children));
        } else {
            feed_field(&mut hasher, name, &attrs, &text(field));
        }
        out.insert((*name).to_string(), (field.line, digest(&hasher)));
    }
    if let Some((content, line)) = content.filter(|(c, _)| elements(c).next().is_some()) {
        let mut hasher = blake3::Hasher::new();
        arch::feed(&mut hasher, content);
        out.insert("#content".into(), (line, digest(&hasher)));
    }
    out
}

/// A hash over each part as `key 0x00 hash 0x00`, in key order, or None when there is none.
pub(crate) fn values_of(parts: &Parts) -> Option<Value> {
    let mut hasher = blake3::Hasher::new();
    for (key, (_, hash)) in parts {
        hasher.update(key.as_bytes());
        hasher.update(&[0x00]);
        hasher.update(hash.as_bytes());
        hasher.update(&[0x00]);
    }
    (!parts.is_empty()).then(|| digest(&hasher).into())
}

/// A plain field as `values` hashes it, with `attrs` in name order and without `name`, so that
/// the CSV reader hashes a cell as the XML field that sets it.
pub(crate) fn feed_field(hasher: &mut blake3::Hasher, name: &str, attrs: &[(&str, &str)], text: &str) {
    feed_field_with(hasher, name, attrs, |h| {
        h.update(&[0x05]);
        h.update(text.as_bytes());
        h.update(&[0x00]);
    });
}

fn feed_field_with(
    hasher: &mut blake3::Hasher,
    name: &str,
    attrs: &[(&str, &str)],
    content: impl FnOnce(&mut blake3::Hasher),
) {
    hasher.update(&[0x07]);
    hasher.update(name.as_bytes());
    hasher.update(&[0x00]);
    for (name, value) in attrs {
        hasher.update(name.as_bytes());
        hasher.update(&[0x02]);
        hasher.update(value.as_bytes());
        hasher.update(&[0x00]);
    }
    hasher.update(&[0x03]);
    content(hasher);
    hasher.update(&[0x04]);
}
