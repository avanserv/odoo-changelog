//! The hash of a function or class: its tokens without layout, comments, quote style, trailing
//! commas, semicolons or its own name, and with its locals numbered, so that reformatted code
//! and a consistent rename of its locals hash alike. Where each statement ends is hashed instead
//! of its semicolon, which JS may also infer.

use std::borrow::Cow;
use std::collections::BTreeMap;

use tree_sitter::Node;

use super::pattern_names;
use super::value::string_value;
use crate::hash::digest;
use crate::sketch::Sketcher;
use crate::syntax::{Tokens, Visit, Visitor, named, text, visit};

/// blake3 over the leaf tokens of `node`, each followed by a 0x00 byte, its strings and the
/// template strings without substitutions as `str:<decoded text>`, a lone arrow parameter in
/// parentheses and a `;` after each statement. The whole of `node` is one scope, as for a Python
/// def: a name bound anywhere in it reads as `$<n>` by first binding, and its own name as `$fn`.
pub(super) fn fn_hash(node: Node, src: &[u8]) -> String {
    hash_with(node, src, |_| {})
}

/// `fn_hash` and a sketch of the same tokens.
pub(super) fn fn_hash_sketched(node: Node, src: &[u8]) -> (String, Option<String>) {
    let mut sketcher = Sketcher::new();
    let hash = hash_with(node, src, |token| sketcher.push(token));
    (hash, sketcher.finish())
}

fn hash_with(node: Node, src: &[u8], mut also: impl FnMut(&str)) -> String {
    let mut hasher = blake3::Hasher::new();
    let own_node = node.child_by_field_name("name");
    let mut walk = Walk {
        src,
        bound: bound_names(node, own_node, src),
        // A method's name is a property, never a bare identifier in its body.
        own_name: own_node.filter(|n| n.kind() == "identifier").map(|n| text(n, src)),
        own_node,
        for_header: 0,
        tokens: Tokens::new(|token: &str| {
            hasher.update(token.as_bytes());
            hasher.update(&[0]);
            also(token);
        }),
    };
    visit(node, &mut walk);
    walk.tokens.finish();
    digest(&hasher)
}

struct Walk<'a, 't, F: FnMut(&str)> {
    src: &'a [u8],
    bound: BTreeMap<String, usize>,
    own_node: Option<Node<'t>>,
    own_name: Option<Cow<'a, str>>,
    /// How many nodes of a `for (...;...;...)` header the walk is in, where `;` is syntax.
    for_header: usize,
    tokens: Tokens<F>,
}

fn is_statement(kind: &str) -> bool {
    (kind.ends_with("_statement") && kind != "empty_statement")
        || matches!(
            kind,
            "lexical_declaration"
                | "variable_declaration"
                | "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
        )
}

fn in_for_header(v: &Visit) -> bool {
    v.parent.is_some_and(|p| p.kind() == "for_statement") && !v.fills("body")
}

impl<'t, F: FnMut(&str)> Visitor<'t> for Walk<'_, 't, F> {
    fn enter(&mut self, v: &Visit<'t>) -> bool {
        let n = v.node;
        if in_for_header(v) {
            self.for_header += 1;
        }
        if (n.is_extra() && !n.is_error()) || Some(n) == self.own_node || n.kind() == "decorator" {
            return false;
        }
        match n.kind() {
            ";" if self.for_header == 0 => {}
            "string" | "template_string" if let Some(s) = string_value(n, self.src) => {
                self.tokens.push(&format!("str:{s}"))
            }
            "identifier" if v.fills("parameter") => {
                self.tokens.push("(");
                self.tokens.push(&self.identifier(n));
                self.tokens.push(")");
            }
            "identifier" => self.tokens.push(&self.identifier(n)),
            // `{a}` names a key as well as a local: renaming the local alone changes the key.
            "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => {
                let name = text(n, self.src);
                self.tokens.push(&name);
                if let Some(i) = self.bound.get(name.as_ref()) {
                    self.tokens.push(":");
                    self.tokens.push(&format!("${i}"));
                }
            }
            "string_fragment" => self
                .tokens
                .push(&text(n, self.src).replace("\r\n", "\n").replace('\r', "\n")),
            _ if n.child_count() == 0 => self.tokens.push(&text(n, self.src)),
            _ => return true,
        }
        false
    }

    fn leave(&mut self, v: &Visit<'t>) {
        if in_for_header(v) {
            self.for_header -= 1;
        }
        if is_statement(v.node.kind()) {
            self.tokens.mark(";");
        }
    }
}

impl<'a, F: FnMut(&str)> Walk<'a, '_, F> {
    fn identifier(&self, n: Node) -> Cow<'a, str> {
        let name = text(n, self.src);
        match self.bound.get(name.as_ref()) {
            Some(i) => Cow::Owned(format!("${i}")),
            None if self.own_name.as_ref() == Some(&name) => Cow::Borrowed("$fn"),
            None => name,
        }
    }
}

/// Every name bound in `node`, numbered by first binding in source order, which puts its
/// parameters first: parameters, declarators, the names of nested functions and classes, `catch`
/// parameters and `for ... in`/`of` declarations. `node`'s own name is not bound.
fn bound_names(node: Node, own_node: Option<Node>, src: &[u8]) -> BTreeMap<String, usize> {
    let mut order = Vec::new();
    visit(node, &mut |v: &Visit| {
        let n = v.node;
        let field = |name: &str| n.child_by_field_name(name);
        match n.kind() {
            "variable_declarator" => pattern_names(field("name"), src, &mut order),
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "class_declaration"
            | "class"
            | "arrow_function"
            | "method_definition" => {
                let name = field("name").filter(|&name| Some(name) != own_node && name.kind() == "identifier");
                pattern_names(name, src, &mut order);
                pattern_names(field("parameter"), src, &mut order);
                for p in field("parameters").map(named).unwrap_or_default() {
                    pattern_names(Some(p), src, &mut order);
                }
            }
            "catch_clause" => pattern_names(field("parameter"), src, &mut order),
            "for_in_statement" if field("kind").is_some() => pattern_names(field("left"), src, &mut order),
            _ => {}
        }
        !n.is_extra()
    });
    let mut numbers = BTreeMap::new();
    for name in order {
        let next = numbers.len();
        numbers.entry(name).or_insert(next);
    }
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{named, parse};

    /// The hash of the first statement of `src`, a declaration, or the right side of an assignment.
    fn hash(src: &str) -> String {
        let tree = parse(src.as_bytes(), &tree_sitter_javascript::LANGUAGE.into()).unwrap();
        let statement = named(tree.root_node())[0];
        let node = match statement.kind() {
            "expression_statement" => named(statement)[0].child_by_field_name("right").unwrap(),
            _ => statement,
        };
        fn_hash(node, src.as_bytes())
    }

    #[test]
    fn layout_comments_quotes_commas_semicolons_and_name_hash_alike() {
        let base = "function f(a, b) {\n    const x = { k: \"v\", n: [1, 2] };\n    return g(x, a);\n}\n";
        for same in [
            "function f(a,b){const x={k:'v',n:[1,2]};return g(x,a)}",
            "function f(\n  a,\n  b,\n) {\n  // why\n  const x = {\n    k: `v`,\n    n: [1, 2,],\n  }\n  /* how */\n  return g(\n    x,\n    a,\n  )\n}",
            "function renamed(a, b) {\n    const x = { k: \"\\u0076\", n: [1, 2] };\n    return g(x, a);\n}\n",
        ] {
            assert_eq!(hash(base), hash(same), "{same}");
        }
        assert_eq!(hash("f = x => x + 1;"), hash("f = (x) => x + 1;"));
    }

    #[test]
    fn token_edits_change_the_hash() {
        let base = "function f(a, b) { for (let i = 0; i < a; i++) { b(i); } return a - b; }";
        for changed in [
            "function f(a, b) { for (let i = 0; i < a; i++) { b(i); } return b - a; }",
            "function f(a, b) { for (let i = 1; i < a; i++) { b(i); } return a - b; }",
            "function f(a, b) { for (let i = 0; i < a; i++) { b(i); } return 'a' - b; }",
            "async function f(a, b) { for (let i = 0; i < a; i++) { b(i); } return a - b; }",
            "function f(a, b) { for (let i = 0; i < a; i++) b(i); return a - b; }",
        ] {
            assert_ne!(hash(base), hash(changed), "{changed}");
        }
        assert_ne!(hash("f = function () { a; b; };"), hash("f = function () { a, b; };"));
        for (one, other) in [("a;(b)", "a(b)"), ("return; x", "return x"), ("a++; b", "a\n++b")] {
            let body = |s: &str| hash(&format!("f = function () {{ {s} }};"));
            assert_ne!(body(one), body(other), "{one} {other}");
        }
        let body = |s: &str| hash(&format!("f = function () {{ {s} }};"));
        assert_eq!(body("a(b)"), body("a\n(b)"));
        assert_eq!(hash("f = () => `a\r\n${b}`;"), hash("f = () => `a\n${b}`;"));
    }

    #[test]
    fn renamed_locals_hash_alike() {
        let base = "function f(a, { b, c: [d, ...e] } = {}) {
            let g = a + b;
            for (const h of d) { try { g(h, e); } catch (i) { g(i); } }
            class J { m(k) { return k; } }
            const l = (m) => m + g;
            const n = function o(p) { return o(p); };
            return [new J(), l, n, f(g), { g }];
        }";
        let renamed = "function f(x, { b: y, c: [z, ...w] } = {}) {
            let v = x + y;
            for (const u of z) { try { v(u, w); } catch (t) { v(t); } }
            class S { m(r) { return r; } }
            const q = (s) => s + v;
            const n2 = function o2(p2) { return o2(p2); };
            return [new S(), q, n2, f(v), { g: v }];
        }";
        assert_eq!(hash(base), hash(renamed));
        assert_eq!(hash("f = x => x + 1;"), hash("f = y => y + 1;"));
        assert_eq!(
            hash("class A { m(a) { return new A(a); } }"),
            hash("class B { m(b) { return new B(b); } }")
        );
    }

    #[test]
    fn free_names_properties_keys_and_order_still_count() {
        let base = "function f(a, b) { return g(a.x, { y: b }, a - b); }";
        for changed in [
            "function f(a, b) { return h(a.x, { y: b }, a - b); }",
            "function f(a, b) { return g(a.z, { y: b }, a - b); }",
            "function f(a, b) { return g(a.x, { z: b }, a - b); }",
            "function f(a, b) { return g(a.x, { y: b }, b - a); }",
            "function f(b, a) { return g(a.x, { y: b }, a - b); }",
        ] {
            assert_ne!(hash(base), hash(changed), "{changed}");
        }
        // Shorthand names a key: `{ b }` renamed alone becomes `{ c }`, another key.
        assert_ne!(
            hash("function f(b) { return { b }; }"),
            hash("function f(c) { return { c }; }")
        );
        assert_ne!(
            hash("function f(o) { const { b } = o; return b; }"),
            hash("function f(o) { const { c } = o; return c; }")
        );
        assert_eq!(
            hash("function f(o) { const { b } = o; return b; }"),
            hash("function f(o) { const { b: c } = o; return c; }")
        );
    }
}
