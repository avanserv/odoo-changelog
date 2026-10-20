//! The hash of a function or class: its tokens without layout, comments, quote style, trailing
//! commas, statement semicolons or its own name, so that reformatted code hashes alike.

use tree_sitter::Node;

use super::value::string_value;
use crate::hash::digest;
use crate::syntax::{Tokens, Visit, Visitor, text, visit};

/// blake3 over the leaf tokens of `node`, each followed by a 0x00 byte, its strings and the
/// template strings without substitutions as `str:<decoded text>`, and a lone arrow parameter in
/// parentheses.
pub(super) fn fn_hash(node: Node, src: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut walk = Walk {
        src,
        own_name: node.child_by_field_name("name"),
        for_header: 0,
        tokens: Tokens::new(|token: &str| {
            hasher.update(token.as_bytes());
            hasher.update(&[0]);
        }),
    };
    visit(node, &mut walk);
    walk.tokens.finish();
    digest(&hasher)
}

struct Walk<'a, 't, F: FnMut(&str)> {
    src: &'a [u8],
    own_name: Option<Node<'t>>,
    /// How many nodes of a `for (...;...;...)` header the walk is in, where `;` is syntax.
    for_header: usize,
    tokens: Tokens<F>,
}

fn in_for_header(v: &Visit) -> bool {
    v.parent.is_some_and(|p| p.kind() == "for_statement") && v.field != Some("body")
}

impl<'t, F: FnMut(&str)> Visitor<'t> for Walk<'_, 't, F> {
    fn enter(&mut self, v: &Visit<'t>) -> bool {
        let n = v.node;
        if in_for_header(v) {
            self.for_header += 1;
        }
        if (n.is_extra() && !n.is_error()) || Some(n) == self.own_name || n.kind() == "decorator" {
            return false;
        }
        match n.kind() {
            ";" if self.for_header == 0 => {}
            "string" | "template_string" if let Some(s) = string_value(n, self.src) => {
                self.tokens.push(&format!("str:{s}"))
            }
            "identifier" if v.field == Some("parameter") => {
                self.tokens.push("(");
                self.tokens.push(&text(n, self.src));
                self.tokens.push(")");
            }
            _ if n.child_count() == 0 => self.tokens.push(&text(n, self.src)),
            _ => return true,
        }
        false
    }

    fn leave(&mut self, v: &Visit<'t>) {
        if in_for_header(v) {
            self.for_header -= 1;
        }
    }
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
            "function f(a, c) { for (let i = 0; i < a; i++) { c(i); } return a - c; }",
            "function f(a, b) { for (let i = 1; i < a; i++) { b(i); } return a - b; }",
            "function f(a, b) { for (let i = 0; i < a; i++) { b(i); } return 'a' - b; }",
            "async function f(a, b) { for (let i = 0; i < a; i++) { b(i); } return a - b; }",
            "function f(a, b) { for (let i = 0; i < a; i++) b(i); return a - b; }",
        ] {
            assert_ne!(hash(base), hash(changed), "{changed}");
        }
        assert_ne!(hash("f = function () { a; b; };"), hash("f = function () { a, b; };"));
    }
}
