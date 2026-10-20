//! What every tree-sitter extractor shares: parsing, the walk, syntax warnings, the token feed
//! and the quoting their canonical texts use. A change to what these return bumps the version of
//! every extractor built on them.

use std::borrow::Cow;

use anyhow::{Result, anyhow};
use tree_sitter::{Language, Node, Parser, Tree};

/// Syntax warnings past this many are summed up in one.
const MAX_SYNTAX_WARNINGS: usize = 10;

/// A tree whenever tree-sitter gives one, with ERROR and MISSING nodes where the source is broken.
pub(crate) fn parse(blob: &[u8], language: &Language) -> Result<Tree> {
    let mut parser = Parser::new();
    parser.set_language(language)?;
    parser
        .parse(blob, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no tree"))
}

/// One step of `visit`: the node and its parent.
pub(crate) struct Visit<'t> {
    pub node: Node<'t>,
    pub parent: Option<Node<'t>>,
}

impl Visit<'_> {
    /// Whether the node fills `field` of its parent. Asked only where it matters: the cursor's
    /// own field lookup on every node makes long chains slow.
    pub(crate) fn fills(&self, field: &str) -> bool {
        self.parent.and_then(|p| p.child_by_field_name(field)) == Some(self.node)
    }
}

pub(crate) trait Visitor<'t> {
    /// Returns whether to walk the node's children.
    fn enter(&mut self, v: &Visit<'t>) -> bool;
    fn leave(&mut self, _: &Visit<'t>) {}
}

impl<'t, F: FnMut(&Visit<'t>) -> bool> Visitor<'t> for F {
    fn enter(&mut self, v: &Visit<'t>) -> bool {
        self(v)
    }
}

/// Walks `root`'s subtree in source order without recursion, which long operator chains would
/// overflow.
pub(crate) fn visit<'t>(root: Node<'t>, visitor: &mut impl Visitor<'t>) {
    let mut cursor = root.walk();
    let mut ancestors: Vec<Visit<'t>> = Vec::new();
    loop {
        let here = Visit {
            node: cursor.node(),
            parent: ancestors.last().map(|a| a.node),
        };
        if visitor.enter(&here) && cursor.goto_first_child() {
            ancestors.push(here);
            continue;
        }
        visitor.leave(&here);
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
            visitor.leave(&ancestors.pop().expect("the cursor's parent was entered"));
        }
    }
}

/// Feeds tokens on, holding each comma back until the next token shows whether it is a trailing
/// one, which is dropped. Comments never reach it, so a comma before one is still trailing.
pub(crate) struct Tokens<F: FnMut(&str)> {
    emit: F,
    comma: bool,
}

impl<F: FnMut(&str)> Tokens<F> {
    pub(crate) fn new(emit: F) -> Self {
        Tokens { emit, comma: false }
    }

    pub(crate) fn push(&mut self, token: &str) {
        if std::mem::take(&mut self.comma) && !matches!(token, ")" | "]" | "}") {
            (self.emit)(",");
        }
        if token == "," {
            self.comma = true;
        } else {
            (self.emit)(token);
        }
    }

    /// A token that no comma before it can be trailing for.
    pub(crate) fn mark(&mut self, token: &str) {
        self.finish();
        (self.emit)(token);
    }

    pub(crate) fn finish(&mut self) {
        if std::mem::take(&mut self.comma) {
            (self.emit)(",");
        }
    }
}

pub(crate) fn text<'s>(node: Node, src: &'s [u8]) -> Cow<'s, str> {
    String::from_utf8_lossy(&src[node.byte_range()])
}

/// Named children without comments.
pub(crate) fn named(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|n| !n.is_extra()).collect()
}

/// Named children without comments, keeping the ERROR nodes tree-sitter marks extra, as it does
/// most ERROR nodes between statements and class members.
pub(crate) fn named_with_errors(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| !n.is_extra() || n.is_error())
        .collect()
}

/// The statements of a module or block in order, those inside ERROR nodes included, so that the
/// definitions around a syntax error survive it.
pub(crate) fn statements(block: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut todo = named_with_errors(block);
    todo.reverse();
    while let Some(n) = todo.pop() {
        if n.is_error() {
            todo.extend(named_with_errors(n).into_iter().rev());
        } else {
            out.push(n);
        }
    }
    out
}

/// `line N: ...` for ERROR and MISSING nodes, the outermost ERROR only, and for the nodes `flag`
/// describes, keyed by byte offset. A warning equal to the one before it is dropped, so that a
/// line holding several flagged forms warns once.
pub(crate) fn syntax_warnings(root: Node, mut flag: impl FnMut(Node) -> Option<String>) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    visit(root, &mut |v: &Visit| {
        let n = v.node;
        let line = n.start_position().row + 1;
        let (warning, descend) = if n.is_error() {
            (Some(format!("line {line}: syntax error")), false)
        } else if n.is_missing() {
            (Some(format!("line {line}: missing {}", n.kind())), false)
        } else {
            (flag(n).map(|what| format!("line {line}: {what}")), true)
        };
        if let Some(w) = warning.filter(|w| found.last().is_none_or(|(_, last)| last != w)) {
            found.push((n.start_byte(), w));
        }
        descend
    });
    if found.len() > MAX_SYNTAX_WARNINGS {
        let more = found.len() - MAX_SYNTAX_WARNINGS;
        let at = found[MAX_SYNTAX_WARNINGS].0;
        found.truncate(MAX_SYNTAX_WARNINGS);
        found.push((at, format!("{more} more syntax warnings")));
    }
    found
}

/// A decoded string in single quotes, the form canonical texts give every plain string, so that
/// quote style and escapes compare equal.
pub(crate) fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}
