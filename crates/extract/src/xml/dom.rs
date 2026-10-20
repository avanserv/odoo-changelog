//! A small owned tree of an XML blob, as lxml would give Odoo: decoded per the declaration,
//! references resolved, comments and processing instructions dropped. Spans are byte offsets
//! into the blob, exact for UTF-8 and approximate after transcoding or a lossy decode.

use std::borrow::Cow;
use std::collections::BTreeSet;

use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::Span;

const BOM: &[u8] = b"\xEF\xBB\xBF";

pub(crate) struct Element {
    pub tag: String,
    pub attrs: Vec<Attr>,
    pub children: Vec<Node>,
    pub line: u32,
    /// False for an element a syntax error left open: its children stop at the error.
    pub complete: bool,
}

pub(crate) struct Attr {
    pub name: String,
    /// Normalized and unescaped.
    pub value: String,
    /// The raw value between the quotes.
    pub span: Span,
    pub line: u32,
}

pub(crate) enum Node {
    Element(Element),
    /// Adjacent text, CDATA and references, as one string.
    Text(String),
}

pub(crate) struct Document {
    pub root: Option<Element>,
    /// `line N: ...`, with N.
    pub warnings: Vec<(u32, String)>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attr_node(name).map(|a| a.value.as_str())
    }

    pub fn attr_node(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }

    /// Element children; text is skipped.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        elements(&self.children)
    }
}

pub(crate) fn elements(nodes: &[Node]) -> impl Iterator<Item = &Element> {
    nodes.iter().filter_map(|n| match n {
        Node::Element(e) => Some(e),
        Node::Text(_) => None,
    })
}

/// Reads `blob` up to its first syntax error, which becomes a warning. Stops after the root's
/// start tag when `keep` rejects its tag, so that a foreign document is never read in full.
pub(crate) fn parse(blob: &[u8], keep: impl Fn(&str) -> bool) -> Document {
    let offset = if blob.starts_with(BOM) { BOM.len() } else { 0 };
    let body = &blob[offset..];
    let text = match declared_encoding(body).and_then(encoding_rs::Encoding::for_label) {
        Some(encoding) if encoding != encoding_rs::UTF_8 => encoding.decode_without_bom_handling(body).0,
        _ => String::from_utf8_lossy(body),
    };
    let mut parser = Parser {
        text: &text,
        offset: offset as u32,
        newlines: text.match_indices('\n').map(|(i, _)| i).collect(),
        open: Vec::new(),
        root: None,
        pending: None,
        warnings: Vec::new(),
        unknown: BTreeSet::new(),
    };
    parser.run(keep);
    parser.close();
    Document {
        root: parser.root,
        warnings: parser.warnings,
    }
}

/// The `encoding` of a leading `<?xml ...?>` declaration.
fn declared_encoding(body: &[u8]) -> Option<&[u8]> {
    let decl = body.strip_prefix(b"<?xml")?;
    let decl = &decl[..decl.windows(2).position(|w| w == b"?>")?];
    let at = decl.windows(8).position(|w| w == b"encoding")?;
    let rest = decl[at + 8..].trim_ascii_start().strip_prefix(b"=")?.trim_ascii_start();
    let quote = *rest.first()?;
    let rest = &rest[1..];
    Some(&rest[..rest.iter().position(|&b| b == quote)?])
}

struct Parser<'t> {
    text: &'t str,
    /// Bytes before `text` in the blob: a BOM.
    offset: u32,
    newlines: Vec<usize>,
    open: Vec<Element>,
    root: Option<Element>,
    pending: Option<String>,
    warnings: Vec<(u32, String)>,
    unknown: BTreeSet<String>,
}

impl<'t> Parser<'t> {
    fn run(&mut self, keep: impl Fn(&str) -> bool) {
        let mut reader = Reader::from_str(self.text);
        reader.config_mut().check_end_names = true;
        loop {
            let at = reader.buffer_position() as usize;
            let event = match reader.read_event() {
                Ok(event) => event,
                Err(e) => {
                    let line = self.line(reader.error_position() as usize);
                    self.warn(line, e);
                    return;
                }
            };
            if matches!(event, Event::Start(_) | Event::Empty(_)) && self.open.is_empty() && self.root.is_some() {
                let line = self.line(at);
                self.warn(line, "content after the root element");
                return;
            }
            match event {
                Event::Start(start) => {
                    let Some(element) = self.element(&start, at) else {
                        return;
                    };
                    let foreign = self.open.is_empty() && !keep(&element.tag);
                    self.flush();
                    self.open.push(element);
                    if foreign {
                        return;
                    }
                }
                Event::Empty(start) => {
                    let Some(mut element) = self.element(&start, at) else {
                        return;
                    };
                    element.complete = true;
                    self.flush();
                    self.append(element);
                }
                Event::End(_) => {
                    self.flush();
                    let mut element = self.open.pop().expect("the reader checks end names");
                    element.complete = true;
                    self.append(element);
                }
                Event::Text(t) => self.push(&t.xml10_content()),
                Event::CData(c) => self.push(&c.xml10_content()),
                Event::GeneralRef(r) => {
                    let resolved = if r.is_char_ref() {
                        match r.resolve_char_ref() {
                            Ok(Some(c)) => c.to_string(),
                            _ => {
                                let line = self.line(at);
                                self.warn(line, "invalid character reference");
                                return;
                            }
                        }
                    } else {
                        let name = r.into_inner();
                        match resolve_predefined_entity(&name) {
                            Some(s) => s.to_string(),
                            None => {
                                if self.unknown.insert(name.to_string()) {
                                    let line = self.line(at);
                                    self.warn(line, format!("unknown entity &{name};"));
                                }
                                format!("&{name};")
                            }
                        }
                    };
                    self.push(&resolved);
                }
                Event::Comment(_) | Event::PI(_) | Event::Decl(_) | Event::DocType(_) => {}
                Event::Eof => {
                    if let Some(open) = self.open.last() {
                        let (line, warning) = (open.line, format!("<{}> is not closed", open.tag));
                        self.warn(line, warning);
                    }
                    return;
                }
            }
        }
    }

    /// Closes what a syntax error or a foreign root left open, as incomplete elements.
    fn close(&mut self) {
        self.flush();
        while let Some(element) = self.open.pop() {
            self.append(element);
        }
    }

    fn element(&mut self, start: &BytesStart, at: usize) -> Option<Element> {
        let line = self.line(at);
        let mut attrs = Vec::new();
        for attr in start.attributes() {
            let attr = match attr {
                Ok(attr) => attr,
                Err(e) => {
                    self.warn(line, e);
                    return None;
                }
            };
            let span = match &attr.value {
                Cow::Borrowed(raw) => self.span_of(raw),
                Cow::Owned(_) => self.span(at, at),
            };
            let value = match attr.normalized_value(XmlVersion::Implicit1_0) {
                Ok(value) => value.into_owned(),
                Err(e) => {
                    self.warn(line, e);
                    attr.value.into_owned()
                }
            };
            attrs.push(Attr {
                name: attr.key.as_ref().to_string(),
                value,
                line: self.line(span.start as usize - self.offset as usize),
                span,
            });
        }
        Some(Element {
            tag: start.name().as_ref().to_string(),
            attrs,
            children: Vec::new(),
            line,
            complete: false,
        })
    }

    /// Keeps the first root only: the reader does not reject a second one.
    fn append(&mut self, element: Element) {
        match self.open.last_mut() {
            Some(parent) => parent.children.push(Node::Element(element)),
            None if self.root.is_none() => self.root = Some(element),
            None => {}
        }
    }

    fn push(&mut self, text: &str) {
        if !self.open.is_empty() {
            self.pending.get_or_insert_default().push_str(text);
        }
    }

    fn flush(&mut self) {
        if let (Some(text), Some(parent)) = (self.pending.take(), self.open.last_mut()) {
            parent.children.push(Node::Text(text));
        }
    }

    fn warn(&mut self, line: u32, warning: impl std::fmt::Display) {
        self.warnings.push((line, format!("line {line}: {warning}")));
    }

    fn line(&self, at: usize) -> u32 {
        self.newlines.partition_point(|&nl| nl < at) as u32 + 1
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span {
            start: start as u32 + self.offset,
            end: end as u32 + self.offset,
        }
    }

    /// The span of `raw`, a slice of `text`.
    fn span_of(&self, raw: &str) -> Span {
        let start = raw.as_ptr() as usize - self.text.as_ptr() as usize;
        self.span(start, start + raw.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(blob: &[u8]) -> (Element, Vec<String>) {
        let doc = parse(blob, |_| true);
        (doc.root.unwrap(), doc.warnings.into_iter().map(|(_, w)| w).collect())
    }

    fn text(element: &Element) -> String {
        element
            .children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect()
    }

    #[test]
    fn latin1_is_transcoded() {
        let blob = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><odoo><record id=\"x\" model=\"m\"><field name=\"name\">\xe9t\xe9</field></record></odoo>";
        let (odoo, warnings) = root(blob);
        let field = odoo.elements().next().unwrap().elements().next().unwrap();
        assert_eq!(text(field), "été");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn spans_slice_the_raw_value_after_a_bom() {
        let blob = b"\xEF\xBB\xBF<odoo>\n  <record id=\"a&gt;b\" model='m'/></odoo>";
        let (odoo, _) = root(blob);
        let record = odoo.elements().next().unwrap();
        let id = record.attr_node("id").unwrap();
        assert_eq!(id.value, "a>b");
        assert_eq!(&blob[id.span.range()], b"a&gt;b");
        assert_eq!((record.line, id.line), (2, 2));
        assert_eq!(&blob[record.attr_node("model").unwrap().span.range()], b"m");
    }

    #[test]
    fn references_resolve_into_one_text_node() {
        let (root, warnings) = root(b"<a>x &gt; y<![CDATA[ <z> ]]>&#233;<!-- c --> w &amp;</a>");
        assert_eq!(root.children.len(), 1);
        assert_eq!(text(&root), "x > y <z> é w &");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn unknown_entities_are_kept_and_warned_once() {
        let (root, warnings) = root(b"<a>\n&nbsp;x&nbsp;</a>");
        assert_eq!(text(&root), "\n&nbsp;x&nbsp;");
        assert_eq!(warnings, ["line 2: unknown entity &nbsp;"]);
    }

    #[test]
    fn a_syntax_error_leaves_open_elements_incomplete() {
        let (odoo, warnings) = root(b"<odoo>\n<a/>\n<b>\n<c></d>\n</b></odoo>");
        assert!(!odoo.complete);
        let children: Vec<(&str, bool)> = odoo.elements().map(|e| (e.tag.as_str(), e.complete)).collect();
        assert_eq!(children, [("a", true), ("b", false)]);
        let b = odoo.elements().nth(1).unwrap();
        assert_eq!(b.elements().map(|e| e.complete).collect::<Vec<_>>(), [false]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("line 4: "), "{warnings:?}");
    }

    #[test]
    fn a_truncated_file_warns_at_its_innermost_open_element() {
        let (odoo, warnings) = root(b"<odoo>\n<a/>\n<b>\n<c/>");
        assert!(!odoo.complete);
        assert_eq!(odoo.elements().count(), 2);
        assert_eq!(warnings, ["line 3: <b> is not closed"]);
    }

    #[test]
    fn a_foreign_root_is_not_read() {
        let doc = parse(b"<Invoice><a></b></Invoice>", |tag| tag == "odoo");
        let root = doc.root.unwrap();
        assert_eq!(
            (root.tag.as_str(), root.children.len(), root.complete),
            ("Invoice", 0, false)
        );
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    #[test]
    fn no_root_and_a_second_root() {
        let doc = parse(b"  <!-- only -->\n", |_| true);
        assert!(doc.root.is_none() && doc.warnings.is_empty());
        let doc = parse(b"<a/>\n<b/>", |_| true);
        assert_eq!(doc.root.unwrap().tag, "a");
        assert_eq!(
            doc.warnings,
            [(2, "line 2: content after the root element".to_string())]
        );
    }
}
