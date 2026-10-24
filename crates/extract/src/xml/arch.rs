//! What a view's arch or a client template offers to others: its named nodes, the inheritance
//! specs it applies in order, and a hash that layout leaves alone. `data` and `owl` share it.

use std::collections::{BTreeMap, VecDeque};

use serde_json::Value;

use super::dom::{Element, Node, elements};
use crate::Span;
use crate::bound::bounded;
use crate::hash::digest;

/// The attributes that name an element of a data view's arch, in key order.
pub(crate) const VIEW_KEYS: &[&str] = &["name", "id", "t-name"];
/// Of a client template: component code also reads `t-ref` (`useRef`), and xpaths target
/// `t-set-slot` like names.
pub(crate) const TEMPLATE_KEYS: &[&str] = &["name", "id", "t-name", "t-ref", "t-set-slot"];

/// One distinct `tag[@attr='value']` of an arch.
pub(crate) struct NamedNode {
    pub key: String,
    /// The first key of each named ancestor of the first occurrence, joined with `/`. Each level
    /// goes through `bounded`, since every descendant repeats it.
    pub path: String,
    /// Above 1, Odoo's shorthand and `//tag[@name=...]` match the first.
    pub count: u32,
    /// The first occurrence's attribute value.
    pub span: Span,
    pub line: u32,
}

/// One inheritance spec: an `xpath` gives `expr`, any other tag a `target` in node-key form.
pub(crate) struct Spec {
    pub expr: Option<String>,
    pub target: Option<String>,
    pub position: String,
    pub mode: Option<String>,
}

/// `tag[@attr='value']`, quoted with `"` when the value holds `'` but no `"`.
pub(crate) fn node_key(tag: &str, attr: &str, value: &str) -> String {
    format!("{tag}{}", predicate(attr, value))
}

fn predicate(attr: &str, value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    format!("[@{attr}={quote}{value}{quote}]")
}

/// Every element of a full arch named by one of `keys`, one per distinct key, in order of first
/// occurrence.
pub(crate) fn named_nodes(arch: &[Node], keys: &[&str]) -> Vec<NamedNode> {
    let mut nodes = Nodes::new(keys);
    for element in elements(arch) {
        nodes.walk(element, &mut Vec::new(), &[]);
    }
    nodes.out
}

/// `named_nodes` of the arch made of `element`, its own attributes in `skip` aside, such as the
/// `t-name` that makes it a template rather than a node.
pub(crate) fn named_nodes_of(element: &Element, keys: &[&str], skip: &[&str]) -> Vec<NamedNode> {
    let mut nodes = Nodes::new(keys);
    nodes.walk(element, &mut Vec::new(), skip);
    nodes.out
}

/// The specs of an inheriting arch in the order Odoo applies them, which puts the content of a
/// `data` spec after the specs already queued, and the named nodes their content adds. A
/// `position="move"` child of a spec is a spec of its own, listed after it.
pub(crate) fn specs(arch: &[Node], keys: &[&str]) -> (Vec<Spec>, Vec<NamedNode>) {
    let mut queue: VecDeque<&Element> = elements(arch).collect();
    let (mut specs, mut nodes) = (Vec::new(), Nodes::new(keys));
    while let Some(element) = queue.pop_front() {
        if element.tag == "data" {
            queue.extend(element.elements());
            continue;
        }
        let applied = spec(element);
        let attributes = applied.position == "attributes";
        specs.push(applied);
        if attributes {
            continue;
        }
        for child in element.elements() {
            match child.attr("position") {
                Some("move") => specs.push(spec(child)),
                _ => nodes.walk(child, &mut Vec::new(), &[]),
            }
        }
    }
    (specs, nodes.out)
}

/// A shorthand spec matches on every attribute but `position`, except `field`, which matches on
/// `name` alone; so a target reads like the node key it resolves to.
fn spec(element: &Element) -> Spec {
    let position = element.attr("position").unwrap_or("inside").to_string();
    let mode = element.attr("mode").map(String::from);
    if element.tag == "xpath" {
        let expr = element.attr("expr").map(String::from);
        return Spec {
            expr,
            target: None,
            position,
            mode,
        };
    }
    let mut target = element.tag.clone();
    if element.tag == "field" {
        target += &element.attr("name").map_or_else(String::new, |v| predicate("name", v));
    } else {
        let mut attrs: Vec<_> = element.attrs.iter().filter(|a| a.name != "position").collect();
        attrs.sort_by(|a, b| a.name.cmp(&b.name));
        attrs.iter().for_each(|a| target += &predicate(&a.name, &a.value));
    }
    Spec {
        expr: None,
        target: Some(target),
        position,
        mode,
    }
}

/// `specs` as attrs hold them: `{"expr"|"target", "position", "mode"?}` each, in order.
pub(crate) fn specs_value(specs: Vec<Spec>) -> Value {
    let specs = specs.into_iter().map(|s| {
        let mut spec = BTreeMap::new();
        spec.insert("position", Value::from(s.position));
        for (key, value) in [("expr", s.expr), ("target", s.target), ("mode", s.mode)] {
            if let Some(value) = value {
                spec.insert(key, value.into());
            }
        }
        Value::from_iter(spec)
    });
    specs.collect()
}

struct Nodes<'k> {
    keys: &'k [&'k str],
    out: Vec<NamedNode>,
    index: BTreeMap<String, usize>,
}

impl<'k> Nodes<'k> {
    fn new(keys: &'k [&'k str]) -> Self {
        Nodes {
            keys,
            out: Vec::new(),
            index: BTreeMap::new(),
        }
    }

    /// `path` holds the node path of each named ancestor, so a node copies its parent's. `skip`
    /// applies to `element` only.
    fn walk(&mut self, element: &Element, path: &mut Vec<String>, skip: &[&str]) {
        let keys: Vec<_> = self
            .keys
            .iter()
            .filter(|key| !skip.contains(key))
            .filter_map(|name| element.attr_node(name))
            .map(|a| (node_key(&element.tag, &a.name, &a.value), a))
            .collect();
        for (key, attr) in &keys {
            match self.index.get(key) {
                Some(&i) => self.out[i].count += 1,
                None => {
                    self.index.insert(key.clone(), self.out.len());
                    self.out.push(NamedNode {
                        key: key.clone(),
                        path: path.last().cloned().unwrap_or_default(),
                        count: 1,
                        span: attr.span,
                        line: attr.line,
                    });
                }
            }
        }
        if let Some((first, _)) = keys.first() {
            let joined = match path.last() {
                Some(parent) => format!("{parent}/{first}"),
                None => first.clone(),
            };
            path.push(bounded(joined));
        }
        for child in element.elements() {
            self.walk(child, path, &[]);
        }
        if !keys.is_empty() {
            path.pop();
        }
    }
}

/// The hash of `arch`'s canonical stream (`feed`).
pub(crate) fn hash(arch: &[Node]) -> String {
    let mut hasher = blake3::Hasher::new();
    feed(&mut hasher, arch);
    digest(&hasher)
}

/// `hash` of the arch made of `element` alone, without its attributes named in `skip`.
pub(crate) fn hash_element(element: &Element, skip: &[&str]) -> String {
    let mut hasher = blake3::Hasher::new();
    feed_element(&mut hasher, element, skip);
    digest(&hasher)
}

/// Writes `nodes` as a stream that reindenting, reflowing, comments, attribute order and the
/// escaped form of a character leave alone. An element is `0x01 tag 0x00`, each attribute sorted
/// by name as `name 0x02 value 0x00`, `0x03`, its children and `0x04`; text that is not all
/// whitespace is `0x05 text 0x00`. Values and text have their whitespace runs collapsed to one
/// space, and those bytes cannot occur in XML 1.0 content, so the stream is unambiguous.
pub(crate) fn feed(hasher: &mut blake3::Hasher, nodes: &[Node]) {
    for node in nodes {
        match node {
            Node::Element(element) => feed_element(hasher, element, &[]),
            Node::Text(text) => {
                let text = collapse(text);
                if !text.is_empty() {
                    hasher.update(&[0x05]);
                    hasher.update(text.as_bytes());
                    hasher.update(&[0x00]);
                }
            }
        }
    }
}

fn feed_element(hasher: &mut blake3::Hasher, element: &Element, skip: &[&str]) {
    hasher.update(&[0x01]);
    hasher.update(element.tag.as_bytes());
    hasher.update(&[0x00]);
    let mut attrs: Vec<_> = element
        .attrs
        .iter()
        .filter(|a| !skip.contains(&a.name.as_str()))
        .collect();
    attrs.sort_by(|a, b| a.name.cmp(&b.name));
    for attr in attrs {
        hasher.update(attr.name.as_bytes());
        hasher.update(&[0x02]);
        hasher.update(collapse(&attr.value).as_bytes());
        hasher.update(&[0x00]);
    }
    hasher.update(&[0x03]);
    feed(hasher, &element.children);
    hasher.update(&[0x04]);
}

/// Each element of `nodes`, descendants included, as its tag and its attributes sorted by name, and
/// each run of text, as `feed` writes them but without children: the items a view's sketch is made
/// of, so that one edited element leaves the others alike.
pub(crate) fn items(nodes: &[Node], out: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::Element(element) => {
                let mut attrs: Vec<_> = element.attrs.iter().collect();
                attrs.sort_by(|a, b| a.name.cmp(&b.name));
                let mut item = format!("\x01{}\x00", element.tag);
                for attr in attrs {
                    item.push_str(&format!("{}\x02{}\x00", attr.name, collapse(&attr.value)));
                }
                out.push(item);
                items(&element.children, out);
            }
            Node::Text(text) => {
                let text = collapse(text);
                if !text.is_empty() {
                    out.push(format!("\x05{text}\x00"));
                }
            }
        }
    }
}

/// XML whitespace runs as one space, trimmed.
fn collapse(text: &str) -> String {
    text.split([' ', '\t', '\r', '\n'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::dom::parse;

    fn arch(src: &str) -> Vec<Node> {
        let doc = parse(src.as_bytes(), |_| true);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        vec![Node::Element(doc.root.unwrap())]
    }

    fn keys(nodes: &[NamedNode]) -> Vec<(&str, &str, u32)> {
        nodes
            .iter()
            .map(|n| (n.key.as_str(), n.path.as_str(), n.count))
            .collect()
    }

    fn spec_list(specs: &[Spec]) -> Vec<String> {
        specs
            .iter()
            .map(|s| {
                let at = s.expr.as_deref().or(s.target.as_deref()).unwrap_or_default();
                match &s.mode {
                    Some(mode) => format!("{at} {} {mode}", s.position),
                    None => format!("{at} {}", s.position),
                }
            })
            .collect()
    }

    #[test]
    fn keys_quote_with_whichever_quote_the_value_lacks() {
        assert_eq!(node_key("field", "name", "partner_id"), "field[@name='partner_id']");
        assert_eq!(node_key("t", "t-name", "it's"), "t[@t-name=\"it's\"]");
        assert_eq!(node_key("div", "id", "a'b\"c"), "div[@id='a'b\"c']");
    }

    #[test]
    fn named_nodes_count_and_path_past_unnamed_wrappers() {
        let src = r#"<form>
            <header><button name="confirm" id="confirm_btn"/><button id="confirm"/></header>
            <sheet>
                <group name="main"><div><field name="partner_id"/></div><filter name="f"/></group>
                <notebook><page name="lines"><field name="line_ids"><tree><field name="partner_id"/></tree></field></page></notebook>
                <t t-name="tpl"><div id="x"/></t>
            </sheet>
        </form>"#;
        let nodes = named_nodes(&arch(src), VIEW_KEYS);
        assert_eq!(
            keys(&nodes),
            [
                ("button[@name='confirm']", "", 1),
                ("button[@id='confirm_btn']", "", 1),
                ("button[@id='confirm']", "", 1),
                ("group[@name='main']", "", 1),
                ("field[@name='partner_id']", "group[@name='main']", 2),
                ("filter[@name='f']", "group[@name='main']", 1),
                ("page[@name='lines']", "", 1),
                ("field[@name='line_ids']", "page[@name='lines']", 1),
                ("t[@t-name='tpl']", "", 1),
                ("div[@id='x']", "t[@t-name='tpl']", 1),
            ]
        );
        let partner = &nodes[4];
        assert_eq!(&src.as_bytes()[partner.span.range()], b"partner_id");
        assert_eq!(partner.line, 4);
    }

    /// Every descendant repeats a path, so a long one is bounded once, where it is built.
    #[test]
    fn long_paths_are_bounded_where_they_are_built() {
        let long = "a".repeat(300);
        let leaves: String = (0..1000).map(|i| format!("<f name=\"{i}\"/>")).collect();
        let src = format!(
            "<form>{}{leaves}{}</form>",
            format!("<g name=\"{long}\">").repeat(200),
            "</g>".repeat(200)
        );
        let nodes = named_nodes(&arch(&src), VIEW_KEYS);
        assert_eq!(nodes.len(), 1001);
        assert!(nodes.iter().all(|n| n.path.len() <= 256), "{}", nodes[1].path);
        assert_eq!(nodes[1].path, nodes[1000].path);
        assert!(nodes[1].path.starts_with('~'), "{}", nodes[1].path);

        let short = arch(r#"<form><g name="a"><g name="b"><f name="c"/></g></g></form>"#);
        assert_eq!(named_nodes(&short, VIEW_KEYS)[2].path, "g[@name='a']/g[@name='b']");
    }

    #[test]
    fn specs_in_application_order_with_the_nodes_they_add() {
        let src = r#"<data>
            <xpath expr="//field[@name='a']" position="after"><field name="b"/></xpath>
            <data><field name="c" string="ignored" position="before"/></data>
            <div id="box" class="o" position="inside">
                <field name="d" position="move"/>
                <group name="g"><field name="e"/></group>
            </div>
            <sheet position="attributes"><attribute name="class">x</attribute></sheet>
            <xpath expr="//group" position="replace" mode="inner"/>
            <page name="p"><field name="f"/></page>
        </data>"#;
        let (specs, nodes) = specs(&arch(src), VIEW_KEYS);
        assert_eq!(
            spec_list(&specs),
            [
                "//field[@name='a'] after",
                "div[@class='o'][@id='box'] inside",
                "field[@name='d'] move",
                "sheet attributes",
                "//group replace inner",
                "page[@name='p'] inside",
                "field[@name='c'] before",
            ]
        );
        assert_eq!(
            keys(&nodes),
            [
                ("field[@name='b']", "", 1),
                ("group[@name='g']", "", 1),
                ("field[@name='e']", "group[@name='g']", 1),
                ("field[@name='f']", "", 1),
            ]
        );
    }

    #[test]
    fn template_keys_also_name_refs_and_slots() {
        let src = r#"<t><div t-ref="root"><t t-set-slot="body"><span name="x"/></t></div></t>"#;
        let nodes = named_nodes(&arch(src), TEMPLATE_KEYS);
        assert_eq!(
            keys(&nodes),
            [
                ("div[@t-ref='root']", "", 1),
                ("t[@t-set-slot='body']", "div[@t-ref='root']", 1),
                ("span[@name='x']", "div[@t-ref='root']/t[@t-set-slot='body']", 1),
            ]
        );
        assert_eq!(keys(&named_nodes(&arch(src), VIEW_KEYS)), [("span[@name='x']", "", 1)]);
    }

    #[test]
    fn hash_element_hashes_the_element_as_an_arch() {
        let root = parse(br#"<div t-name="x" class="a"><span/></div>"#, |_| true)
            .root
            .unwrap();
        let unnamed = parse(br#"<div class="a"><span/></div>"#, |_| true).root.unwrap();
        assert_eq!(hash_element(&root, &["t-name"]), hash(&[Node::Element(unnamed)]));
        assert_eq!(hash_element(&root, &[]), hash(&[Node::Element(root)]));
    }

    #[test]
    fn hash_ignores_layout_and_sees_edits() {
        let base = hash(&arch(r#"<form><field name="a" string="A &gt; B"/>Some  text</form>"#));
        for same in [
            "<form>\n  <!-- note -->\n  <field string=\"A > B\" name=\"a\"/>\n  Some\n  text\n</form>",
            "<form><field name='a' string='A &#62; B'></field>Some text</form>",
        ] {
            assert_eq!(hash(&arch(same)), base, "{same}");
        }
        for edited in [
            r#"<form><field name="b" string="A &gt; B"/>Some  text</form>"#,
            r#"<form><field name="a" string="A &gt; C"/>Some  text</form>"#,
            r#"<form><field name="a" string="A &gt; B"/>Other text</form>"#,
            r#"<tree><field name="a" string="A &gt; B"/>Some  text</tree>"#,
        ] {
            assert_ne!(hash(&arch(edited)), base, "{edited}");
        }
        assert_eq!(base.len(), 32);
    }
}
