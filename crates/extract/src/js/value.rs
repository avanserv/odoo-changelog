//! Literal values and the canonical text of expressions, so that attrs compare equal across
//! formatting: quote style, escapes, spacing, comments, semicolons and trailing commas.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};
use tree_sitter::Node;

use super::hash::fn_hash;
use super::scope::{Resolved, Scope};
use crate::syntax::{Tokens, Visit, named, quoted, text, visit};

/// Literals nested deeper than this read as expressions, which keeps `value` off the stack limit.
const MAX_DEPTH: usize = 32;

/// Whether `node` is a function or a class, which values hold as a hash of their tokens.
pub(super) fn is_function(node: Node) -> bool {
    node.is_named()
        && matches!(
            node.kind(),
            "function_expression"
                | "function_declaration"
                | "generator_function"
                | "generator_function_declaration"
                | "arrow_function"
                | "method_definition"
                | "class"
                | "class_declaration"
        )
}

/// The JSON value of a literal, `{"ref": ...}` for a name that resolves, `{"fn": <hash>}` for a
/// function or class, or `{"expr": <canonical text>}` for anything else.
pub(super) fn value(node: Node, src: &[u8], scope: &Scope) -> Value {
    value_at(node, src, scope, 0)
}

fn value_at(node: Node, src: &[u8], scope: &Scope, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return expr(node, src);
    }
    let literal = match node.kind() {
        "string" | "template_string" => string_value(node, src).map(Value::String),
        "number" => number(&text(node, src)),
        "unary_expression" => negative(node, src),
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        "null" => Some(Value::Null),
        "array" => Some(Value::Array(
            named(node)
                .into_iter()
                .map(|n| value_at(n, src, scope, depth + 1))
                .collect(),
        )),
        "object" => Some(Value::Object(object(node, src, scope, depth).into_iter().collect())),
        "call_expression" => merged(node, src, scope, depth),
        "identifier" | "shorthand_property_identifier" | "member_expression" => {
            scope.resolve(node, src).map(|r| match r {
                Resolved::Ref(r) => single("ref", r),
                Resolved::Expr(e) => single("expr", e),
            })
        }
        "parenthesized_expression" => named(node).first().map(|&n| value_at(n, src, scope, depth + 1)),
        _ if is_function(node) => Some(single("fn", fn_hash(node, src))),
        _ => None,
    };
    literal.unwrap_or_else(|| expr(node, src))
}

pub(super) fn single(key: &str, value: impl Into<Value>) -> Value {
    Value::Object(Map::from_iter([(key.to_string(), value.into())]))
}

pub(super) fn expr(node: Node, src: &[u8]) -> Value {
    single("expr", canonical(node, src))
}

/// An object literal's entries, the last of a key winning as in JS. Shorthand entries resolve
/// like names, methods hash like functions and a spread reads as `...<canonical>`.
fn object(node: Node, src: &[u8], scope: &Scope, depth: usize) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for entry in named(node) {
        let (key, value) = match entry.kind() {
            "pair" => {
                let (Some(key), Some(v)) = (entry.child_by_field_name("key"), entry.child_by_field_name("value"))
                else {
                    continue;
                };
                (key_text(key, src), v)
            }
            "method_definition" => (method_key(entry, src), entry),
            "shorthand_property_identifier" => (text(entry, src).into_owned(), entry),
            "spread_element" => match named(entry).first() {
                Some(&inner) => (format!("...{}", canonical(inner, src)), inner),
                None => continue,
            },
            _ => continue,
        };
        out.insert(key, value_at(value, src, scope, depth + 1));
    }
    out
}

/// `_.extend({}, a, {...})`, `$.extend`, `jQuery.extend` and `Object.assign` on a fresh object,
/// as the object they build, which is how legacy widgets extend their parent's `events`.
fn merged(call: Node, src: &[u8], scope: &Scope, depth: usize) -> Option<Value> {
    let callee = canonical(call.child_by_field_name("function")?, src);
    if !matches!(
        callee.as_str(),
        "_.extend" | "$.extend" | "jQuery.extend" | "Object.assign"
    ) {
        return None;
    }
    let args = named(call.child_by_field_name("arguments")?);
    let (first, rest) = args.split_first()?;
    if !is_empty_object(*first) {
        return None;
    }
    let mut out = BTreeMap::new();
    for &arg in rest {
        if arg.kind() == "object" {
            out.extend(object(arg, src, scope, depth));
        } else {
            out.insert(
                format!("...{}", canonical(arg, src)),
                value_at(arg, src, scope, depth + 1),
            );
        }
    }
    Some(Value::Object(out.into_iter().collect()))
}

pub(super) fn is_empty_object(node: Node) -> bool {
    node.kind() == "object" && named(node).is_empty()
}

/// A property name as text: an identifier, a decoded string, a number, or `[<canonical>]` for a
/// computed one.
pub(super) fn key_text(key: Node, src: &[u8]) -> String {
    match key.kind() {
        "string" => string_value(key, src).unwrap_or_default(),
        "computed_property_name" => format!("[{}]", named(key).first().map_or(String::new(), |&k| canonical(k, src))),
        _ => text(key, src).into_owned(),
    }
}

/// A method's name, `get x` and `set x` for accessors.
pub(super) fn method_key(method: Node, src: &[u8]) -> String {
    let name = method
        .child_by_field_name("name")
        .map_or(String::new(), |n| key_text(n, src));
    match accessor(method) {
        Some(kind) => format!("{kind} {name}"),
        None => name,
    }
}

fn accessor(method: Node) -> Option<&'static str> {
    let name = method.child_by_field_name("name")?;
    let mut cursor = method.walk();
    method
        .children(&mut cursor)
        .take_while(|c| c.start_byte() < name.start_byte())
        .find_map(|c| match c.kind() {
            "get" => Some("get"),
            "set" => Some("set"),
            _ => None,
        })
}

/// The decoded text of a string, or of a template string without substitutions.
pub(super) fn string_value(node: Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "template_string") {
        return None;
    }
    let mut out = String::new();
    let mut cursor = node.walk();
    for part in node.children(&mut cursor) {
        match part.kind() {
            "template_substitution" => return None,
            "escape_sequence" => unescape(&text(part, src), &mut out),
            _ if part.is_named() && !part.is_extra() => out.push_str(&text(part, src)),
            _ => {}
        }
    }
    Some(out)
}

/// Decodes one escape sequence: a backslash before a line break is dropped, and an escaped
/// character with no meaning of its own stands for itself.
fn unescape(sequence: &str, out: &mut String) {
    let body = &sequence[1.min(sequence.len())..];
    let mut chars = body.chars();
    let decoded = match chars.next() {
        Some('n') => Some('\n'),
        Some('t') => Some('\t'),
        Some('r') => Some('\r'),
        Some('b') => Some('\x08'),
        Some('f') => Some('\x0c'),
        Some('v') => Some('\x0b'),
        Some('0') if body.len() == 1 => Some('\0'),
        Some('x' | 'u') => code_point(&body[1..]),
        Some('\r' | '\n' | '\u{2028}' | '\u{2029}') => return,
        Some(c) if body.chars().count() == 1 => Some(c),
        _ => None,
    };
    match decoded {
        Some(c) => out.push(c),
        None => out.push_str(sequence),
    }
}

fn code_point(hex: &str) -> Option<char> {
    let hex = hex.strip_prefix('{').and_then(|h| h.strip_suffix('}')).unwrap_or(hex);
    char::from_u32(u32::from_str_radix(hex, 16).ok()?)
}

/// Legacy octals (`017`) and BigInts (`1n`) are no JSON number here.
fn number(text: &str) -> Option<Value> {
    let digits = text.replace('_', "").to_ascii_lowercase();
    let radix = match digits.get(..2) {
        Some("0x") => 16,
        Some("0o") => 8,
        Some("0b") => 2,
        _ => 10,
    };
    if radix != 10 {
        return i64::from_str_radix(&digits[2..], radix).ok().map(Value::from);
    }
    if digits.ends_with('n')
        || (digits.len() > 1 && digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    match digits.parse::<i64>() {
        Ok(i) => Some(Value::from(i)),
        Err(_) => Number::from_f64(digits.parse().ok()?).map(Value::Number),
    }
}

fn negative(node: Node, src: &[u8]) -> Option<Value> {
    let operator = node.child_by_field_name("operator")?;
    let argument = node.child_by_field_name("argument").filter(|a| a.kind() == "number")?;
    if operator.kind() != "-" {
        return None;
    }
    match number(&text(argument, src))? {
        Value::Number(n) if n.is_i64() => n.as_i64().map(|i| Value::from(-i)),
        Value::Number(n) => Number::from_f64(-n.as_f64()?).map(Value::Number),
        _ => None,
    }
}

/// The leaf tokens of `node` without comments, semicolons or trailing commas, with one space
/// between two word characters and after each comma. Strings and template strings without
/// substitutions read as their quoted value, other template strings as written, and functions
/// and classes as `fn:<hash>`, which keeps the text short.
pub(super) fn canonical(node: Node, src: &[u8]) -> String {
    let mut out = String::new();
    let mut tokens = Tokens::new(|token: &str| {
        if token.is_empty() {
            return;
        }
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
        let after_comma = out.ends_with(',') && !matches!(token, ")" | "]" | "}");
        if after_comma || (out.ends_with(word) && token.starts_with(word)) {
            out.push(' ');
        }
        out.push_str(token);
    });
    visit(node, &mut |v: &Visit| {
        let n = v.node;
        if n.is_extra() && !n.is_error() {
            return false;
        }
        match n.kind() {
            "string" | "template_string" if let Some(s) = string_value(n, src) => tokens.push(&quoted(&s)),
            "template_string" => tokens.push(&text(n, src)),
            ";" => {}
            _ if is_function(n) => tokens.push(&format!("fn:{}", fn_hash(n, src))),
            _ if n.child_count() == 0 => tokens.push(&text(n, src)),
            _ => return true,
        }
        false
    });
    tokens.finish();
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::js::scope::Binding;
    use crate::syntax::parse;

    /// The value and canonical text of the expression `source`, with `Foo` bound to `./foo#Foo`.
    fn read(source: &str) -> (Value, String) {
        let src = format!("x = {source};\n");
        let tree = parse(src.as_bytes(), &tree_sitter_javascript::LANGUAGE.into()).unwrap();
        let statement = named(tree.root_node())[0];
        let right = named(statement)[0].child_by_field_name("right").unwrap();
        let mut scope = Scope::new(String::new());
        scope.bind(
            "Foo",
            Binding::Member {
                module: "./foo".into(),
                path: "Foo".into(),
            },
        );
        scope.bind("Local", Binding::Local);
        (value(right, src.as_bytes(), &scope), canonical(right, src.as_bytes()))
    }

    fn value_of(source: &str) -> Value {
        read(source).0
    }

    #[test]
    fn literals_become_json() {
        for (source, expected) in [
            (r#""a\tb\x41éA\u{1F600}\q\0""#, json!("a\tbAéA\u{1F600}q\0")),
            ("'it\\'s'", json!("it's")),
            ("'a\\\nb'", json!("ab")),
            ("`multi\nline`", json!("multi\nline")),
            ("1_000", json!(1000)),
            ("0x1F", json!(31)),
            ("0o17", json!(15)),
            ("0b101", json!(5)),
            ("-3", json!(-3)),
            ("2.5", json!(2.5)),
            ("-0.5", json!(-0.5)),
            ("1e3", json!(1000.0)),
            (".5", json!(0.5)),
            ("true", json!(true)),
            ("null", json!(null)),
            ("[1, [2, 'x'],]", json!([1, [2, "x"]])),
            (
                "{b: 1, 'a': [false], 2: null}",
                json!({"2": null, "a": [false], "b": 1}),
            ),
            ("{a: 1, a: 2}", json!({"a": 2})),
            ("((1))", json!(1)),
        ] {
            assert_eq!(value_of(source), expected, "{source}");
        }
    }

    #[test]
    fn names_resolve_and_functions_hash() {
        assert_eq!(value_of("Foo"), json!({"ref": "./foo#Foo"}));
        assert_eq!(value_of("Foo.bar"), json!({"ref": "./foo#Foo.bar"}));
        assert_eq!(value_of("Local"), json!({"ref": "#Local"}));
        assert_eq!(
            value_of("{Foo, [k]: 1, ...base, get x() {}, m() {}}")
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["...base", "Foo", "[k]", "get x", "m"]
        );
        assert_eq!(value_of("{Foo}")["Foo"], json!({"ref": "./foo#Foo"}));
        for function in [
            "function () { return 1; }",
            "() => 1",
            "class extends Foo {}",
            "async function* g() {}",
        ] {
            let hash = value_of(function)["fn"].as_str().unwrap().to_string();
            assert_eq!(hash.len(), 32, "{function}");
        }
    }

    #[test]
    fn extend_on_a_fresh_object_merges() {
        assert_eq!(
            value_of("_.extend({}, Foo.prototype.events, {'click .a': '_onClick'}, {'click .a': '_onA'})"),
            json!({"...Foo.prototype.events": {"ref": "./foo#Foo.prototype.events"}, "click .a": "_onA"})
        );
        assert_eq!(value_of("Object.assign({}, {a: 1})"), json!({"a": 1}));
        assert_eq!(
            value_of("_.extend(target, {a: 1})"),
            json!({"expr": "_.extend(target, {a:1})"})
        );
    }

    #[test]
    fn anything_else_is_an_expression() {
        for (source, expected) in [
            ("_t('Name')", "_t('Name')"),
            ("`a${b}`", "`a${b}`"),
            ("10n", "10n"),
            ("017", "017"),
            ("a ? b : c", "a?b:c"),
            ("new Foo(1, 2)", "new Foo(1, 2)"),
            ("unbound.x", "unbound.x"),
            ("this.x", "this.x"),
            ("typeof x", "typeof x"),
            ("-x", "-x"),
        ] {
            assert_eq!(value_of(source), json!({"expr": expected}), "{source}");
        }
        let deep = format!("{}1{}", "[".repeat(40), "]".repeat(40));
        let mut v = &value_of(&deep);
        for _ in 0..=MAX_DEPTH {
            v = &v[0];
        }
        assert_eq!(v["expr"].as_str().unwrap().len(), 2 * (40 - MAX_DEPTH - 1) + 1);
    }

    #[test]
    fn canonical_text_ignores_layout() {
        let (_, a) = read("f(\n    'x',  // why\n    \"y\" /* and */,\n    k = 1,\n)");
        let (_, b) = read("f('x', 'y', k=1)");
        assert_eq!(a, b);
        assert_eq!(a, "f('x', 'y', k=1)");
        assert_eq!(read(r#"g("it's\n")"#).1, r"g('it\'s\n')");
        assert_eq!(read("[a,]").1, "[a]");
        let (_, with_fn) = read("f(function () { return 1; })");
        assert!(
            with_fn.starts_with("f(fn:") && with_fn.len() == "f(fn:)".len() + 32,
            "{with_fn}"
        );
    }
}
