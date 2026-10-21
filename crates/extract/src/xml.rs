//! An Odoo XML data file as `odoo/tools/convert.py` loads it: every declaration by xmlid, with
//! the declarations of one id in the file merged as Odoo updates the record, and each view's
//! named nodes as symbols of their own.

pub(crate) mod arch;
pub(crate) mod dom;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::bound::{Budget, bounded};
use crate::data::{ACTION, DATA_FUNCTION, DELETE, MENU, RECORD, VIEW, VIEW_NODE, kind_of};
use crate::hash::digest;
use crate::{Span, Symbol, SymbolTable};
use dom::{Attr, Element, Node, elements};

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
    /// The declaring elements' attributes but the id, later ones winning.
    attrs: BTreeMap<&'d str, &'d str>,
    /// A record's fields by name, later ones winning.
    fields: BTreeMap<&'d str, &'d Element>,
    /// The content of a template, asset or call, the last declaration's.
    content: Option<&'d [Node]>,
}

impl<'d> Decl<'d> {
    fn new(declared: &'static str, at: &'d Attr, el: &'d Element, skip: &[&str], noupdate: bool) -> Self {
        let attrs = el
            .attrs
            .iter()
            .filter(|a| !skip.contains(&a.name.as_str()))
            .map(|a| (a.name.as_str(), a.value.as_str()))
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
        decl.content = Some(&el.children);
        self.declare(VIEW, id, decl);
    }

    /// A nested menuitem takes the enclosing one as parent, whatever its own attribute says.
    fn menuitem(&mut self, el: &'d Element, parent: Option<&'d str>, noupdate: bool) {
        let Some(id) = el.attr_node("id") else {
            return self.warn(el.line, "menuitem without id ignored");
        };
        let mut decl = Decl::new("menuitem", id, el, &["id"], noupdate);
        if let Some(parent) = parent {
            decl.attrs.insert("parent", parent);
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
        decl.content = Some(&el.children);
        self.declare(kind, id, decl);
    }

    fn function(&mut self, el: &'d Element, noupdate: bool) {
        let (Some(model), Some(name)) = (el.attr("model"), el.attr_node("name")) else {
            let missing = if el.attr("model").is_none() { "model" } else { "name" };
            return self.warn(el.line, format!("function without {missing} ignored"));
        };
        let mut decl = Decl::new("function", name, el, &[], noupdate);
        decl.content = Some(&el.children);
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
        for ((kind, name), decl) in &self.decls {
            let (attrs, nodes) = attrs(kind, decl);
            let symbol = Symbol {
                kind: (*kind).into(),
                name: name.clone(),
                line: decl.line,
                span: decl.span,
                attrs,
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

/// The attrs of a declaration, and the named nodes of a view.
fn attrs(kind: &str, decl: &Decl) -> (BTreeMap<String, Value>, Vec<arch::NamedNode>) {
    let mut out = BTreeMap::new();
    let a = |key: &str| decl.attrs.get(key).map(|v| Value::from(*v));
    let f = |key: &str| decl.fields.get(key).copied().map(clear);
    let mut nodes = Vec::new();
    match decl.declared {
        "function" => {
            put(&mut out, "model", a("model"));
            put(&mut out, "name", a("name"));
            put(&mut out, "values", values(decl, &["model", "name"], &[], decl.content));
            if decl.noupdate {
                out.insert("noupdate".into(), true.into());
            }
            return (out, nodes);
        }
        "delete" => {
            put(&mut out, "model", a("model"));
            put(&mut out, "search", a("search"));
            return (out, nodes);
        }
        _ => {}
    }
    out.insert("declared".into(), decl.declared.into());
    put(&mut out, "record_model", decl.record_model.map(Value::from));
    if decl.noupdate {
        out.insert("noupdate".into(), true.into());
    }
    match (kind, decl.declared) {
        (VIEW, "template") => {
            let arch = decl.content.unwrap_or_default();
            let inheriting = decl.attrs.contains_key("inherit_id");
            put(&mut out, "name", a("name"));
            put(&mut out, "inherit_id", a("inherit_id"));
            put(&mut out, "priority", a("priority"));
            out.insert("type".into(), "qweb".into());
            let mode = match decl.attrs.get("primary") {
                Some(&"True") => "primary",
                _ if inheriting => "extension",
                _ => "primary",
            };
            out.insert("mode".into(), mode.into());
            nodes = view_arch(&mut out, arch, inheriting);
            let skip = ["name", "inherit_id", "priority", "primary"];
            put(&mut out, "values", values(decl, &skip, &[], None));
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
            put(&mut out, "values", values(decl, &[], &["arch"], None));
        }
        (MENU, "menuitem") => {
            for (key, value) in &decl.attrs {
                let value = match *key {
                    "groups" => value.split(',').map(Value::from).collect(),
                    _ => Value::from(*value),
                };
                out.insert((*key).into(), value);
            }
        }
        (_, "report") => {
            put(&mut out, "name", a("string"));
            put(&mut out, "model", a("model"));
            put(&mut out, "values", values(decl, &["string", "model"], &[], None));
        }
        (_, "act_window") => {
            put(&mut out, "name", a("name"));
            put(&mut out, "res_model", a("res_model"));
            put(&mut out, "values", values(decl, &["name", "res_model"], &[], None));
        }
        (_, "asset") => {
            put(&mut out, "name", a("name"));
            put(&mut out, "values", values(decl, &["name"], &[], decl.content));
        }
        _ => {
            for key in ["name", "model", "res_model"] {
                put(&mut out, key, f(key));
            }
            let model_id = decl.fields.get("model_id").and_then(|field| field.attr("ref"));
            put(&mut out, "model_id", model_id.map(Value::from));
            put(&mut out, "values", values(decl, &[], &[], None));
        }
    }
    (out, nodes)
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

/// A hash, or None when there is nothing to hash: the declaration's attributes but `skip`, as
/// `name 0x02 value 0x00` in name order, then every field but `skip_fields`, those the attrs hold
/// in clear included, as `0x07 name 0x00`, its other attributes likewise, `0x03`, its content and
/// `0x04`, then `0x06` and `content` when it has any. Plain field text is hashed as written, since
/// Odoo stores it verbatim (Python code is indentation-sensitive); markup like an arch.
fn values(decl: &Decl, skip: &[&str], skip_fields: &[&str], content: Option<&[Node]>) -> Option<Value> {
    let mut hasher = blake3::Hasher::new();
    let mut empty = true;
    for (name, value) in decl.attrs.iter().filter(|(n, _)| !skip.contains(n)) {
        hasher.update(name.as_bytes());
        hasher.update(&[0x02]);
        hasher.update(value.as_bytes());
        hasher.update(&[0x00]);
        empty = false;
    }
    for (name, field) in decl.fields.iter().filter(|(n, _)| !skip_fields.contains(n)) {
        let mut attrs: Vec<(&str, &str)> = field
            .attrs
            .iter()
            .filter(|a| a.name != "name")
            .map(|a| (a.name.as_str(), a.value.as_str()))
            .collect();
        attrs.sort();
        if is_markup(field) {
            feed_field_with(&mut hasher, name, &attrs, |h| arch::feed(h, &field.children));
        } else {
            feed_field(&mut hasher, name, &attrs, &text(field));
        }
        empty = false;
    }
    if let Some(content) = content.filter(|c| elements(c).next().is_some()) {
        hasher.update(&[0x06]);
        arch::feed(&mut hasher, content);
        empty = false;
    }
    (!empty).then(|| digest(&hasher).into())
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
