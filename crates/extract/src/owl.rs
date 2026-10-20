//! Client templates under `static/src`, read as Odoo's asset bundles load them (15.0 to 17.0
//! `assetsbundle.py`, master's `templates.js`) and as the legacy QWeb engine of 12.0 to 16.0
//! extends them (`qweb2.js`): each template by name, each extension by the template it extends,
//! and the named nodes either gives a template.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;

use crate::bound::{Budget, ENVELOPE, bounded, json_len, warn_capped};
use crate::path::{static_addon, static_file};
use crate::xml::arch::{self, NamedNode, TEMPLATE_KEYS};
use crate::xml::dom::{self, Attr, Element};
use crate::{Extractor, Span, Symbol, SymbolTable};

const TEMPLATE: &str = "owl_template";
const EXTENSION: &str = "owl_extension";
const NODE: &str = "owl_node";

/// Roots whose element children are the templates; any other root is one template itself.
const ROOTS: [&str; 3] = ["templates", "template", "odoo"];

/// Attributes of a template element that other attrs already hold, left out of its `arch`, so
/// that writing the default mode explicitly is no change.
const OWN_ATTRS: [&str; 5] = ["t-name", "t-inherit", "t-inherit-mode", "t-extend", "owl"];

/// The serialized symbols one file may give, per byte of it plus a constant, past which the
/// reader stops collecting symbols and warns once.
const OUTPUT_PER_BYTE: usize = 24;
const OUTPUT_SLACK: usize = 1 << 20;

pub struct Owl;

impl Extractor for Owl {
    fn name(&self) -> &'static str {
        "owl"
    }

    /// Bump with any change to the output, including a quick-xml upgrade and changes to
    /// `xml::dom` or `xml::arch`, which `data` shares.
    fn version(&self) -> u32 {
        1
    }

    /// Client templates Odoo serves from `static/src`; tests, third-party files under
    /// `static/lib` and data files outside `static/` belong elsewhere.
    fn claims(&self, path: &str) -> bool {
        path.ends_with(".xml")
            && static_file(path).is_some_and(|(_, rest)| {
                rest.first() == Some(&"src") && !rest[1..].iter().any(|s| matches!(*s, "tests" | "test"))
            })
    }

    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        Ok(read(blob))
    }

    /// `<name>@<addon>`: template names are global, and each addon declaring or extending one
    /// keeps a history of its own. The addon follows the last `@`, the template precedes the
    /// first `#`.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        format!("{}@{}", symbol.name, static_addon(path).unwrap_or_default())
    }
}

fn read(blob: &[u8]) -> SymbolTable {
    let doc = dom::parse(blob, |_| true);
    let mut reader = Reader {
        budget: Budget::new(blob, OUTPUT_PER_BYTE, OUTPUT_SLACK),
        decls: Vec::new(),
        names: BTreeSet::new(),
        extensions: BTreeMap::new(),
        nodes: BTreeMap::new(),
        warnings: doc.warnings,
    };
    let empty = blob
        .strip_prefix(b"\xEF\xBB\xBF")
        .unwrap_or(blob)
        .trim_ascii()
        .is_empty();
    let template_attrs = ["t-name", "t-inherit", "t-extend"];
    match &doc.root {
        // A root a syntax error cut short still gives its complete children.
        Some(root) if ROOTS.contains(&root.tag.as_str()) => {
            for template in root.elements().filter(|t| t.complete) {
                reader.template(template);
            }
        }
        Some(root) if root.complete && template_attrs.iter().any(|a| root.attr(a).is_some()) => {
            reader.template(root);
        }
        // A foreign root, such as a mail plugin's translations, is no template file.
        Some(_) => {}
        None if empty => {}
        None => reader.warn(1, "no root element"),
    }
    reader.finish()
}

/// The parent a template inherits from, and whether it says so with the legacy `t-extend`.
struct Inherit<'d> {
    parent: &'d Attr,
    legacy: bool,
}

/// What one template element declares.
enum Form<'d> {
    /// A template of its own, possibly a primary inheritor or a `t-extend` clone.
    Template(&'d Attr, Option<Inherit<'d>>),
    /// Changes to its parent, with the `t-name` Odoo ignores.
    Extension(Inherit<'d>, Option<&'d Attr>),
}

/// A named node, merged over the templates of the file that share its name: counts add up, and
/// the first occurrence keeps the rest.
struct MergedNode {
    count: u32,
    path: String,
    line: u32,
    span: Span,
}

struct Reader {
    /// Spent as symbols are collected, in document order, so that memory stays bounded too.
    budget: Budget,
    /// Templates and extensions, in document order.
    decls: Vec<Symbol>,
    /// The templates read so far, for duplicates.
    names: BTreeSet<String>,
    /// How many extensions of each parent were read so far, for their `[n]` suffix.
    extensions: BTreeMap<String, u32>,
    nodes: BTreeMap<String, MergedNode>,
    /// `line N: ...`, with N.
    warnings: Vec<(u32, String)>,
}

impl Reader {
    fn warn(&mut self, line: u32, warning: impl std::fmt::Display) {
        self.warnings.push((line, format!("line {line}: {warning}")));
    }

    /// Classifies one template element as Odoo does, `t-inherit` before `t-extend`, with
    /// `primary` as the default mode on every branch.
    fn template(&mut self, el: &Element) {
        let given = |attr: &str| el.attr_node(attr).filter(|a| !a.value.is_empty());
        let name = given("t-name");
        let form = if let Some(parent) = given("t-inherit") {
            let inherit = Inherit { parent, legacy: false };
            match (el.attr("t-inherit-mode").unwrap_or("primary"), name) {
                ("extension", _) => Form::Extension(inherit, name),
                ("primary", Some(name)) => Form::Template(name, Some(inherit)),
                ("primary", None) => return self.warn(el.line, "primary template without t-name ignored"),
                (mode, _) => {
                    let warning = format!("t-inherit-mode {mode} is not primary or extension; ignored");
                    return self.warn(el.line, warning);
                }
            }
        } else if let Some(parent) = given("t-extend") {
            let inherit = Inherit { parent, legacy: true };
            match name {
                Some(name) => Form::Template(name, Some(inherit)),
                None => Form::Extension(inherit, None),
            }
        } else if let Some(name) = name {
            Form::Template(name, None)
        } else {
            return self.warn(el.line, format!("<{}> without t-name ignored", el.tag));
        };

        let mut attrs = BTreeMap::new();
        let (kind, at, symbol_name, owner, nodes) = match form {
            Form::Template(name, inherit) => {
                if !self.names.insert(name.value.clone()) {
                    let warning = format!("duplicate {TEMPLATE} {} dropped", name.value);
                    return self.warn(name.line, warning);
                }
                let nodes = match &inherit {
                    // Its own element is rendered, so its keys but `t-name` name nodes too.
                    None => arch::named_nodes_of(el, TEMPLATE_KEYS, &["t-name"]),
                    Some(inherit) => content(el, inherit, "primary", &mut attrs),
                };
                (TEMPLATE, name, name.value.clone(), &name.value, nodes)
            }
            Form::Extension(inherit, name) => {
                if let Some(name) = name {
                    attrs.insert("name".into(), name.value.as_str().into());
                }
                let parent = &inherit.parent.value;
                let n = self.extensions.entry(parent.clone()).or_default();
                *n += 1;
                let symbol_name = if *n == 1 {
                    parent.clone()
                } else {
                    format!("{parent}[{n}]")
                };
                let nodes = content(el, &inherit, "extension", &mut attrs);
                // An extension's nodes live in its parent, so they are named under it.
                (EXTENSION, inherit.parent, symbol_name, parent, nodes)
            }
        };
        if let Some(owl) = el.attr("owl") {
            attrs.insert("owl".into(), owl.into());
        }
        attrs.insert("arch".into(), arch::hash_element(el, &OWN_ATTRS).into());

        let owner = bounded(owner.clone());
        let symbol = Symbol {
            kind: kind.into(),
            name: symbol_name,
            line: at.line,
            span: at.span,
            attrs,
        };
        if self.budget.symbol_at(&symbol, symbol.line as usize) {
            self.decls.push(symbol);
        }
        for node in nodes {
            let name = format!("{owner}#{}", node.key);
            if let Some(merged) = self.nodes.get_mut(&name) {
                merged.count += node.count;
            } else if self
                .budget
                .afford(json_len(&name) + json_len(&node.path) + ENVELOPE, node.line as usize)
            {
                let merged = MergedNode {
                    count: node.count,
                    path: node.path,
                    line: node.line,
                    span: node.span,
                };
                self.nodes.insert(name, merged);
            }
        }
    }

    fn finish(self) -> SymbolTable {
        let mut table = SymbolTable::default();
        let nodes = self.nodes.into_iter().map(|(name, node)| {
            let mut attrs = BTreeMap::from([("count".to_string(), Value::from(node.count))]);
            if !node.path.is_empty() {
                attrs.insert("path".into(), node.path.into());
            }
            Symbol {
                kind: NODE.into(),
                name,
                line: node.line,
                span: node.span,
                attrs,
            }
        });
        for symbol in self.decls.into_iter().chain(nodes) {
            table.insert(symbol);
        }
        let warnings = self.warnings.into_iter().map(|(line, w)| (line as usize, w)).collect();
        warn_capped(&mut table, warnings, &self.budget, "symbols");
        table
    }
}

/// The inheritance attrs and specs of `el`, given its parent and mode, and the named nodes its
/// specs add.
fn content(el: &Element, inherit: &Inherit, mode: &str, attrs: &mut BTreeMap<String, Value>) -> Vec<NamedNode> {
    attrs.insert("inherit".into(), inherit.parent.value.as_str().into());
    attrs.insert("mode".into(), mode.into());
    if !inherit.legacy {
        let (specs, nodes) = arch::specs(&el.children, TEMPLATE_KEYS);
        attrs.insert("specs".into(), arch::specs_value(specs));
        return nodes;
    }
    attrs.insert("declared".into(), "t-extend".into());
    let (mut specs, mut nodes) = (Vec::new(), Vec::new());
    for child in el.elements() {
        let operation = child.attr("t-operation");
        let spec = [("jquery", child.attr("t-jquery")), ("operation", operation)];
        specs.push(Value::from_iter(spec.into_iter().filter_map(|(k, v)| Some((k, v?)))));
        // Without an operation, the child's text is code run on the target.
        if operation.is_some_and(|o| o != "attributes") {
            nodes.extend(arch::named_nodes(&child.children, TEMPLATE_KEYS));
        }
    }
    attrs.insert("specs".into(), specs.into());
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_templates_under_static_src_only() {
        for path in [
            "addons/web/static/src/views/form/form_controller.xml",
            "theme_x/static/src/xml/a.xml",
            "odoo/addons/base/static/src/a.xml",
        ] {
            assert!(Owl.claims(path), "{path}");
        }
        for path in [
            "addons/web/static/tests/a.xml",
            "addons/web/static/lib/qweb/qweb-test-call.xml",
            "addons/web/static/src/tests/a.xml",
            "addons/mail_bot/views/discuss.xml",
            "addons/web/static/snippets/a.xml",
            "addons/web/static/src/a.js",
        ] {
            assert!(!Owl.claims(path), "{path}");
        }
    }

    #[test]
    fn qualnames_end_with_the_addon() {
        let table = read(
            br#"<templates><t t-name="web.X"><div t-ref="r"/></t><t t-inherit="web.X" t-inherit-mode="extension"/></templates>"#,
        );
        let path = "addons/mail/static/src/x.xml";
        let rows: Vec<(&str, String)> = table.iter().map(|s| (s.kind.as_str(), Owl.qualify(path, s))).collect();
        assert_eq!(
            rows,
            [
                (EXTENSION, "web.X@mail".to_string()),
                (NODE, "web.X#div[@t-ref='r']@mail".to_string()),
                (TEMPLATE, "web.X@mail".to_string()),
            ]
        );
    }
}
