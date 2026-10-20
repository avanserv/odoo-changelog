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

/// Keys that make a one-key object stand for something else than a JS object.
const TAGS: [&str; 4] = ["expr", "fn", "inline", "ref"];

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
        "object" => Some(tagless(object(node, src, scope, depth))),
        "call_expression" => merged(node, src, scope, depth).or_else(|| resolved(node, src, scope)),
        "identifier" | "shorthand_property_identifier" | "member_expression" => resolved(node, src, scope),
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

/// `{"ref"}` or `{"expr"}` for what a name, a member chain or a `require` names.
fn resolved(node: Node, src: &[u8], scope: &Scope) -> Option<Value> {
    scope.resolve(node, src).map(|r| match r {
        Resolved::Ref(r) => single("ref", r),
        Resolved::Expr(e) => single("expr", e),
    })
}

/// Object entries as JSON. The key of a one-key object is prefixed with `=` when it is a tag or
/// already starts with `=`, so that a literal `{ref: "#x"}` never reads as a reference.
pub(super) fn tagless(mut entries: BTreeMap<String, Value>) -> Value {
    if let Some(key) = entries.keys().next().filter(|_| entries.len() == 1).cloned()
        && (TAGS.contains(&key.as_str()) || key.starts_with('='))
    {
        let value = entries.remove(&key).expect("the only key");
        entries.insert(format!("={key}"), value);
    }
    Value::Object(entries.into_iter().collect())
}

/// An object literal's entries, the last of a key winning as in JS. Shorthand entries resolve
/// like names and methods hash like functions. A spread reads as `...<canonical>`, holding what
/// it resolves to or `null`: its value is not read, so nested spreads cost no more than once.
pub(super) fn object(node: Node, src: &[u8], scope: &Scope, depth: usize) -> BTreeMap<String, Value> {
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
            "spread_element" => {
                if let Some(&inner) = named(entry).first() {
                    out.insert(spread(inner, src), resolved(inner, src, scope).unwrap_or(Value::Null));
                }
                continue;
            }
            _ => continue,
        };
        out.insert(key, value_at(value, src, scope, depth + 1));
    }
    out
}

fn spread(node: Node, src: &[u8]) -> String {
    format!("...{}", canonical(node, src))
}

/// `_.extend({}, a, {...})`, `$.extend`, `jQuery.extend` and `Object.assign` on a fresh object,
/// as the object they build, which is how legacy widgets extend their parent's `events`. An
/// argument that is no object literal reads like a spread.
fn merged(call: Node, src: &[u8], scope: &Scope, depth: usize) -> Option<Value> {
    let callee = call
        .child_by_field_name("function")
        .filter(|c| c.kind() == "member_expression")?;
    let (object, property) = (
        callee.child_by_field_name("object")?,
        callee.child_by_field_name("property")?,
    );
    let merge = matches!(
        (text(object, src).as_ref(), text(property, src).as_ref()),
        ("_" | "$" | "jQuery", "extend") | ("Object", "assign")
    );
    let args = named(call.child_by_field_name("arguments")?);
    let (first, rest) = args.split_first()?;
    if !merge || !is_empty_object(*first) {
        return None;
    }
    let mut out = BTreeMap::new();
    for &arg in rest {
        if arg.kind() == "object" {
            out.extend(self::object(arg, src, scope, depth));
        } else {
            out.insert(spread(arg, src), resolved(arg, src, scope).unwrap_or(Value::Null));
        }
    }
    Some(tagless(out))
}

pub(super) fn is_empty_object(node: Node) -> bool {
    node.kind() == "object" && named(node).is_empty()
}

/// A property name as text: an identifier, a decoded string, a number as JS reads it, or
/// `[<canonical>]` for a computed one.
pub(super) fn key_text(key: Node, src: &[u8]) -> String {
    match key.kind() {
        "string" => string_value(key, src).unwrap_or_default(),
        "number" => number(&text(key, src)).map_or_else(|| text(key, src).into_owned(), |n| n.to_string()),
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

/// The decoded text of a string, or of a template string without substitutions, whose line
/// breaks read as `\n` as JS reads them.
pub(super) fn string_value(node: Node, src: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "template_string") {
        return None;
    }
    let mut out = String::new();
    // A `\uD8xx` escape waiting for the low surrogate that completes it, and its text.
    let mut high: Option<(u32, String)> = None;
    let mut cursor = node.walk();
    for part in node.children(&mut cursor) {
        let decoded = match part.kind() {
            "template_substitution" => return None,
            "escape_sequence" => unescape(&text(part, src)),
            "string_fragment" if node.kind() == "template_string" => {
                Err(text(part, src).replace("\r\n", "\n").replace('\r', "\n"))
            }
            _ if part.is_named() && !part.is_extra() => Err(text(part, src).into_owned()),
            _ => continue,
        };
        let pending = high.take();
        match decoded {
            Ok(Some(low @ 0xDC00..0xE000)) if pending.is_some() => {
                let (h, _) = pending.expect("a pending high surrogate");
                out.extend(char::from_u32(0x10000 + ((h - 0xD800) << 10) + (low - 0xDC00)));
                continue;
            }
            _ => out.push_str(&pending.map(|(_, raw)| raw).unwrap_or_default()),
        }
        match decoded {
            Ok(Some(h @ 0xD800..0xDC00)) => high = Some((h, text(part, src).into_owned())),
            Ok(Some(code)) => match char::from_u32(code) {
                Some(c) => out.push(c),
                None => out.push_str(&text(part, src)),
            },
            Ok(None) => {}
            Err(raw) => out.push_str(&raw),
        }
    }
    out.push_str(&high.map(|(_, raw)| raw).unwrap_or_default());
    Some(out)
}

/// The code unit an escape sequence stands for, `None` for a line continuation, or the sequence
/// itself when it means nothing. An escaped character with no meaning of its own stands for
/// itself, and `\0` to `\377` are legacy octal escapes.
fn unescape(sequence: &str) -> Result<Option<u32>, String> {
    let raw = || Err(sequence.to_string());
    let Some(body) = sequence.get(1..).filter(|b| !b.is_empty()) else {
        return raw();
    };
    let mut chars = body.chars();
    let first = chars.next().expect("a non-empty body");
    let code = match first {
        'n' => 0x0a,
        't' => 0x09,
        'r' => 0x0d,
        'b' => 0x08,
        'f' => 0x0c,
        'v' => 0x0b,
        '0'..='7' if body.len() <= 3 && body.bytes().all(|b| (b'0'..=b'7').contains(&b)) => {
            u32::from_str_radix(body, 8).expect("octal digits")
        }
        'x' | 'u' => {
            let hex = &body[1..];
            let hex = hex.strip_prefix('{').and_then(|h| h.strip_suffix('}')).unwrap_or(hex);
            match u32::from_str_radix(hex, 16) {
                Ok(code) if !hex.is_empty() => code,
                _ => return raw(),
            }
        }
        '\r' | '\n' | '\u{2028}' | '\u{2029}' => return Ok(None),
        c if chars.next().is_none() => c as u32,
        _ => return raw(),
    };
    Ok(Some(code))
}

/// A JS number, an integer when it is one. Legacy octals (`017`) and BigInts (`1n`) are no JSON
/// number here.
fn number(text: &str) -> Option<Value> {
    let digits = text.replace('_', "").to_ascii_lowercase();
    let radix = match digits.get(..2) {
        Some("0x") => 16,
        Some("0o") => 8,
        Some("0b") => 2,
        _ => 10,
    };
    let legacy_octal = digits.len() > 1 && digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit());
    if digits.ends_with('n') || (radix == 10 && legacy_octal) {
        return None;
    }
    let value = match radix {
        10 => digits.parse::<f64>().ok()?,
        _ => u64::from_str_radix(&digits[2..], radix).ok()? as f64,
    };
    json_number(value)
}

fn json_number(value: f64) -> Option<Value> {
    // Integers past 2^53 are not exact in JS either, so they keep their float form.
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        Some(Value::from(value as i64))
    } else {
        Number::from_f64(value).map(Value::Number)
    }
}

fn negative(node: Node, src: &[u8]) -> Option<Value> {
    let operator = node.child_by_field_name("operator")?;
    let argument = node.child_by_field_name("argument").filter(|a| a.kind() == "number")?;
    if operator.kind() != "-" {
        return None;
    }
    json_number(-number(&text(argument, src))?.as_f64()?)
}

/// The leaf tokens of `node` without comments, semicolons or trailing commas, with one space
/// between two word characters, between two tokens that would run into another one, and after
/// each comma. Strings and template strings without substitutions read as their quoted value,
/// and functions and classes as `fn:<hash>`, which keeps the text short. Another template
/// string reads as written up to 256 bytes when it holds no function, else as `tpl:<hash>`.
pub(super) fn canonical(node: Node, src: &[u8]) -> String {
    let mut out = String::new();
    let mut tokens = Tokens::new(|token: &str| {
        let (Some(last), Some(next)) = (out.chars().last(), token.chars().next()) else {
            out.push_str(token);
            return;
        };
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
        let after_comma = last == ',' && !matches!(token, ")" | "]" | "}");
        let run_together = matches!((last, next), ('+', '+') | ('-', '-') | ('/', '/' | '*'));
        if after_comma || run_together || (word(last) && word(next)) {
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
            "template_string" => tokens.push(&template(n, src)),
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

/// A template string with substitutions, for `canonical`.
fn template(node: Node, src: &[u8]) -> String {
    if node.byte_range().len() <= 256 {
        let mut function = false;
        visit(node, &mut |v: &Visit| {
            function |= is_function(v.node);
            !function
        });
        if !function {
            return text(node, src).replace("\r\n", "\n").replace('\r', "\n");
        }
    }
    format!("tpl:{}", fn_hash(node, src))
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
            ("1e3", json!(1000)),
            ("1.0", json!(1)),
            ("1.5e1", json!(15)),
            ("2e53", json!(2e53)),
            (r#""\uD83D\uDE00 \uD83D \101\0""#, json!("\u{1F600} \\uD83D A\0")),
            ("`a\r\nb\rc`", json!("a\nb\nc")),
            ("{1.0: a, 0x10: b}", json!({"1": {"expr": "a"}, "16": {"expr": "b"}})),
            ("{ref: '#x'}", json!({"=ref": "#x"})),
            ("{'=a': 1}", json!({"==a": 1})),
            ("{ref: 1, b: 2}", json!({"b": 2, "ref": 1})),
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
        assert_eq!(
            value_of("{...f({a: 1}), ...Foo}"),
            json!({"...Foo": {"ref": "./foo#Foo"}, "...f({a:1})": null})
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
            ("a - --b", "a- --b"),
            ("a-- - b", "a-- -b"),
            ("a / /x/", "a/ /x/"),
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
