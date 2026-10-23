//! The xmlids and templates a declaration names literally. They are read syntactically, so
//! nothing is evaluated, and a `t-call` whose target is computed at render time is only counted.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::Value;

use super::dom::{Element, Node, elements};

#[derive(Default)]
pub(crate) struct Refs {
    names: BTreeSet<String>,
    dynamic_calls: u32,
    /// A client template names other templates, never xmlids: only its `t-call`s count.
    calls_only: bool,
}

impl Refs {
    pub(crate) fn templates() -> Self {
        Refs {
            calls_only: true,
            ..Refs::default()
        }
    }

    /// `references`, sorted, and `dynamic_calls`, each when there is any.
    pub(crate) fn put(self, out: &mut BTreeMap<String, Value>) {
        if !self.names.is_empty() {
            out.insert("references".into(), self.names.into_iter().collect());
        }
        if self.dynamic_calls > 0 {
            out.insert("dynamic_calls".into(), self.dynamic_calls.into());
        }
    }

    /// An xmlid as an attribute holds it, with the `!` or `-` that negates a group dropped.
    pub(crate) fn id(&mut self, value: &str) {
        let id = value.trim();
        let id = id.strip_prefix(['!', '-']).unwrap_or(id);
        if is_xml_id(id) {
            self.names.insert(id.to_string());
        }
    }

    /// A comma-separated list, as `groups` and a CSV `/id` cell hold one.
    pub(crate) fn ids(&mut self, value: &str) {
        value.split(',').for_each(|g| self.id(g));
    }

    /// `ref('x')` and `%(x)d` or `%(x)s` in an expression, a field's text or an attribute value.
    pub(crate) fn text(&mut self, text: &str) {
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let bytes = text.as_bytes();
        for (at, _) in text.match_indices("ref(") {
            if at > 0 && word(bytes[at - 1]) {
                continue;
            }
            let rest = text[at + 4..].trim_start();
            let Some(quote) = rest.chars().next().filter(|q| matches!(q, '\'' | '"')) else {
                continue;
            };
            if let Some((id, _)) = rest[1..].split_once(quote) {
                self.id(id);
            }
        }
        for (at, _) in text.match_indices("%(") {
            if let Some((id, rest)) = text[at + 2..].split_once(')')
                && rest.starts_with(['d', 's'])
            {
                self.id(id);
            }
        }
    }

    /// The template a `t-call` names, or one more dynamic call.
    pub(crate) fn call(&mut self, target: &str) {
        let target = target.trim();
        let literal = !target.is_empty()
            && target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
        if literal {
            self.names.insert(target.to_string());
        } else {
            self.dynamic_calls += 1;
        }
    }

    /// Every attribute of `nodes` and of their descendants.
    pub(crate) fn arch(&mut self, nodes: &[Node]) {
        self.walk(elements(nodes));
    }

    pub(crate) fn walk<'e>(&mut self, roots: impl IntoIterator<Item = &'e Element>) {
        let mut todo: Vec<&Element> = roots.into_iter().collect();
        while let Some(element) = todo.pop() {
            self.element(element);
            todo.extend(element.elements());
        }
    }

    /// What the specs of an inheriting arch put in, as `arch::specs` walks it: a spec's own
    /// attributes locate its target and name nothing, but the values an `attributes` spec sets do.
    pub(crate) fn specs(&mut self, arch: &[Node]) {
        let mut queue: VecDeque<&Element> = elements(arch).collect();
        while let Some(spec) = queue.pop_front() {
            if spec.tag == "data" {
                queue.extend(spec.elements());
                continue;
            }
            for child in spec.elements() {
                match (spec.attr("position"), child.attr("position")) {
                    (Some("attributes"), _) if child.tag == "attribute" => self.set_attribute(child),
                    (Some("attributes"), _) | (_, Some("move")) => {}
                    _ => self.walk([child]),
                }
            }
        }
    }

    fn element(&mut self, element: &Element) {
        for attr in &element.attrs {
            self.attr(&element.tag, &attr.name, &attr.value);
        }
    }

    fn attr(&mut self, tag: &str, name: &str, value: &str) {
        match name {
            "t-call" => self.call(value),
            _ if self.calls_only => {}
            "groups" => self.ids(value),
            "ref" if matches!(tag, "field" | "value") => self.id(value),
            _ => self.text(value),
        }
    }

    /// `<attribute name="groups">a,b</attribute>`, or with `add` and `remove` lists.
    fn set_attribute(&mut self, attribute: &Element) {
        let Some(name) = attribute.attr("name") else {
            return;
        };
        let text: String = attribute
            .children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect();
        for value in [Some(text.as_str()), attribute.attr("add"), attribute.attr("remove")]
            .into_iter()
            .flatten()
            .filter(|v| !v.trim().is_empty())
        {
            self.attr("", name, value);
        }
    }
}

/// `x` or `module.x`, as Odoo's `ref` accepts them.
fn is_xml_id(id: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    match id.split_once('.') {
        Some((module, name)) => part(module) && part(name),
        None => part(id),
    }
}
