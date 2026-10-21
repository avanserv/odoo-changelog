//! What a view's arch offers to others: its named nodes, the inheritance specs it applies in
//! order, and a hash that layout leaves alone.

use std::collections::{BTreeMap, VecDeque};

use super::dom::{Element, Node, elements};
use crate::Span;
use crate::hash::digest;

/// The attributes that name a node, in key order.
const NAMING: [&str; 3] = ["name", "id", "t-name"];

/// One distinct `tag[@attr='value']` of an arch.
pub(crate) struct NamedNode {
    pub key: String,
    /// The first key of each named ancestor of the first occurrence, joined with `/`.
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

/// Every element of a full arch named by `name`, `id` or `t-name`, one per distinct key, in
/// order of first occurrence.
pub(crate) fn named_nodes(arch: &[Node]) -> Vec<NamedNode> {
    let mut nodes = Nodes::default();
    for element in elements(arch) {
        nodes.walk(element, &mut Vec::new());
    }
    nodes.out
}

/// The specs of an inheriting arch in the order Odoo applies them, which puts the content of a
/// `data` spec after the specs already queued, and the named nodes their content adds. A
/// `position="move"` child of a spec is a spec of its own, listed after it.
pub(crate) fn specs(arch: &[Node]) -> (Vec<Spec>, Vec<NamedNode>) {
    let mut queue: VecDeque<&Element> = elements(arch).collect();
    let (mut specs, mut nodes) = (Vec::new(), Nodes::default());
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
                _ => nodes.walk(child, &mut Vec::new()),
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

#[derive(Default)]
struct Nodes {
    out: Vec<NamedNode>,
    index: BTreeMap<String, usize>,
}

impl Nodes {
    fn walk(&mut self, element: &Element, path: &mut Vec<String>) {
        let keys: Vec<_> = NAMING
            .iter()
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
                        path: path.join("/"),
                        count: 1,
                        span: attr.span,
                        line: attr.line,
                    });
                }
            }
        }
        if let Some((first, _)) = keys.first() {
            path.push(first.clone());
        }
        for child in element.elements() {
            self.walk(child, path);
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

/// Writes `nodes` as a stream that reindenting, reflowing, comments, attribute order and the
/// escaped form of a character leave alone. An element is `0x01 tag 0x00`, each attribute sorted
/// by name as `name 0x02 value 0x00`, `0x03`, its children and `0x04`; text that is not all
/// whitespace is `0x05 text 0x00`. Values and text have their whitespace runs collapsed to one
/// space, and those bytes cannot occur in XML 1.0 content, so the stream is unambiguous.
pub(crate) fn feed(hasher: &mut blake3::Hasher, nodes: &[Node]) {
    for node in nodes {
        match node {
            Node::Element(element) => {
                hasher.update(&[0x01]);
                hasher.update(element.tag.as_bytes());
                hasher.update(&[0x00]);
                let mut attrs: Vec<_> = element.attrs.iter().collect();
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
        let nodes = named_nodes(&arch(src));
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
        let (specs, nodes) = specs(&arch(src));
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
