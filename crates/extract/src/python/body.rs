//! The `body` attr of a def: a hash of its tokens that reformatting, comments, the docstring and
//! a consistent rename of its locals leave alone, so that moved or renamed code hashes alike.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use tree_sitter::Node;

use super::value::{delimiters, prefix};
use super::{Tokens, Visit, Visitor, named, text, visit};
use crate::hash::digest;

/// The first 16 bytes of blake3 over the body's tokens, each followed by a 0x00 byte, as hex.
/// The whole def is one scope: a name bound anywhere in it, nested defs and lambdas included,
/// reads as `$<n>` (parameters first, in signature order, then by first binding), and the def's
/// own name reads as `$fn`, so that an override hashes alike under a new name.
pub(super) fn body_hash(def: Node, src: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    if let Some(body) = def.child_by_field_name("body") {
        let own_name = def.child_by_field_name("name").map(|n| text(n, src));
        let docstring = named(body).first().copied().filter(|&s| is_docstring(s));
        let mut walk = Walk {
            src,
            bound: bound_names(def, body, src),
            own_name: own_name.as_deref(),
            docstring,
            tokens: Tokens::new(|token: &str| {
                hasher.update(token.as_bytes());
                hasher.update(&[0]);
            }),
        };
        visit(body, &mut walk);
        walk.tokens.finish();
    }
    digest(&hasher)
}

fn is_docstring(statement: Node) -> bool {
    let inner = named(statement);
    statement.kind() == "expression_statement"
        && inner.len() == 1
        && matches!(inner[0].kind(), "string" | "concatenated_string")
}

struct Walk<'a, 't, F: FnMut(&str)> {
    src: &'a [u8],
    bound: BTreeMap<String, usize>,
    own_name: Option<&'a str>,
    docstring: Option<Node<'t>>,
    tokens: Tokens<F>,
}

impl<'t, F: FnMut(&str)> Visitor<'t> for Walk<'_, 't, F> {
    fn enter(&mut self, v: &Visit<'t>) -> bool {
        let n = v.node;
        if n.is_extra() || Some(n) == self.docstring {
            return false;
        }
        match n.kind() {
            // Indentation is syntax: without these, `if a: b` then `c` would read as `if a: b; c`.
            "block" => self.tokens.mark("{"),
            "string" => {
                let flags = delimiters(n).map(|(start, _)| prefix(&text(start, self.src)).replace('u', ""));
                self.tokens.push(&format!("str:{}", flags.unwrap_or_default()));
            }
            "string_start" | "string_end" => return false,
            "string_content" => {
                self.tokens.push(&text(n, self.src));
                return false;
            }
            "," if v.closes_single_tuple() => {
                self.tokens.mark(",");
                return false;
            }
            "identifier" => {
                let token = self.identifier(v);
                self.tokens.push(&token);
                return false;
            }
            _ if n.child_count() == 0 => {
                self.tokens.push(&text(n, self.src));
                return false;
            }
            _ => {}
        }
        true
    }

    fn leave(&mut self, v: &Visit<'t>) {
        if v.node.kind() == "block" {
            self.tokens.mark("}");
        }
    }
}

impl<'a, F: FnMut(&str)> Walk<'a, '_, F> {
    /// Attribute and keyword names refer to fields, methods and parameters elsewhere, so a local
    /// of the same name never replaces them.
    fn identifier(&self, v: &Visit) -> Cow<'a, str> {
        let name = text(v.node, self.src);
        let parent = v.parent.map(|p| p.kind());
        let elsewhere = matches!(
            (parent, v.field),
            (Some("attribute"), Some("attribute")) | (Some("keyword_argument"), Some("name"))
        );
        match self.bound.get(name.as_ref()) {
            Some(i) if !elsewhere => Cow::Owned(format!("${i}")),
            _ if Some(name.as_ref()) == self.own_name => Cow::Borrowed("$fn"),
            _ => name,
        }
    }
}

/// Every name bound in the def, numbered: parameters, assignment and loop targets, `as` and
/// walrus names, imports, nested defs and classes and lambda parameters, minus `global` and
/// `nonlocal` names.
fn bound_names(def: Node, body: Node, src: &[u8]) -> BTreeMap<String, usize> {
    let mut order = Vec::new();
    if let Some(params) = def.child_by_field_name("parameters") {
        parameters(params, src, &mut order);
    }
    let mut global = BTreeSet::new();
    visit(body, &mut |v: &Visit| {
        let n = v.node;
        let field = |name: &str| n.child_by_field_name(name);
        match n.kind() {
            "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
                targets(field("left"), src, &mut order);
            }
            "as_pattern" | "except_clause" => targets(field("alias"), src, &mut order),
            "named_expression" => targets(field("name"), src, &mut order),
            "function_definition" | "class_definition" | "lambda" => {
                targets(field("name"), src, &mut order);
                if let Some(params) = field("parameters") {
                    parameters(params, src, &mut order);
                }
            }
            "import_statement" | "import_from_statement" => {
                for name in n.children_by_field_name("name", &mut n.walk()) {
                    let bound = match name.kind() {
                        "aliased_import" => name.child_by_field_name("alias"),
                        // `import a.b` binds `a`; `from m import b` binds `b`.
                        _ if n.kind() == "import_statement" => name.named_child(0),
                        _ => named(name).last().copied(),
                    };
                    targets(bound, src, &mut order);
                }
            }
            "global_statement" | "nonlocal_statement" => {
                global.extend(named(n).into_iter().map(|i| text(i, src).into_owned()));
            }
            _ => {}
        }
        !n.is_extra()
    });
    let mut numbers = BTreeMap::new();
    for name in order {
        if !global.contains(&name) && !numbers.contains_key(&name) {
            numbers.insert(name, numbers.len());
        }
    }
    numbers
}

fn parameters(params: Node, src: &[u8], out: &mut Vec<String>) {
    for p in named(params) {
        let target = match p.kind() {
            "default_parameter" | "typed_default_parameter" => p.child_by_field_name("name"),
            "typed_parameter" => named(p).first().copied(),
            _ => Some(p),
        };
        targets(target, src, out);
    }
}

/// The names a binding target binds, through tuples, lists and splats, but not through an
/// attribute or a subscript, which bind nothing new.
fn targets(target: Option<Node>, src: &[u8], out: &mut Vec<String>) {
    let mut todo: Vec<Node> = target.into_iter().collect();
    while let Some(n) = todo.pop() {
        match n.kind() {
            "identifier" => out.push(text(n, src).into_owned()),
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "tuple"
            | "list"
            | "expression_list"
            | "list_splat_pattern"
            | "dictionary_splat_pattern"
            | "list_splat"
            | "parenthesized_expression"
            | "as_pattern_target" => todo.extend(named(n).into_iter().rev()),
            _ => {}
        }
    }
}
