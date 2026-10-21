//! Literal values and the canonical text of expressions, so that attrs compare equal across
//! formatting: quote style, string escapes and splits, spacing and trailing commas.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};
use tree_sitter::Node;

use super::closes_single_tuple;
use crate::syntax::{Tokens, Visit, named, quoted, text, visit};

/// Literals nested deeper than this read as expressions, which keeps `value` off the stack limit.
const MAX_DEPTH: usize = 32;

/// The JSON value of a literal, or `{"expr": <canonical text>}` for anything else. The manifest
/// extractor shares it: a change to its output bumps both extractors' `version()`.
pub(crate) fn value(node: Node, src: &[u8]) -> Value {
    value_at(node, src, 0)
}

fn value_at(node: Node, src: &[u8], depth: usize) -> Value {
    let literal = if depth > MAX_DEPTH {
        None
    } else {
        match node.kind() {
            "string" => plain_string(node, src).map(Value::String),
            "concatenated_string" => concatenated(node, src).map(Value::String),
            "integer" => integer(&text(node, src)).map(Value::from),
            "float" => float(&text(node, src)),
            "unary_operator" => negative(node, src),
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            "none" => Some(Value::Null),
            "list" | "tuple" | "expression_list" => Some(Value::Array(
                named(node).into_iter().map(|n| value_at(n, src, depth + 1)).collect(),
            )),
            "dictionary" => dictionary(node, src, depth),
            "parenthesized_expression" => named(node).first().map(|&n| value_at(n, src, depth + 1)),
            _ => None,
        }
    };
    literal.unwrap_or_else(|| expr(node, src))
}

pub(super) fn expr(node: Node, src: &[u8]) -> Value {
    Value::Object(Map::from_iter([("expr".to_string(), canonical(node, src).into())]))
}

/// Python 3 rejects leading zeros, so `0777` (a Python 2 octal) and `10L` are no integer here.
fn integer(text: &str) -> Option<i64> {
    let digits = text.replace('_', "").to_ascii_lowercase();
    let (radix, digits) = match digits.get(..2) {
        Some("0x") => (16, &digits[2..]),
        Some("0o") => (8, &digits[2..]),
        Some("0b") => (2, &digits[2..]),
        _ if digits.len() > 1 && digits.starts_with('0') && digits.bytes().any(|b| b != b'0') => return None,
        _ => (10, digits.as_str()),
    };
    i64::from_str_radix(digits, radix).ok()
}

fn float(text: &str) -> Option<Value> {
    let f: f64 = text.replace('_', "").parse().ok()?;
    Number::from_f64(f).map(Value::Number)
}

fn negative(node: Node, src: &[u8]) -> Option<Value> {
    let operator = node.child_by_field_name("operator")?;
    let argument = node.child_by_field_name("argument")?;
    if text(operator, src) != "-" {
        return None;
    }
    match argument.kind() {
        "integer" => integer(&text(argument, src)).map(|i| Value::from(-i)),
        "float" => float(&format!("-{}", text(argument, src))),
        _ => None,
    }
}

fn dictionary(node: Node, src: &[u8], depth: usize) -> Option<Value> {
    let mut out = BTreeMap::new();
    for pair in named(node) {
        let key = pair.child_by_field_name("key").filter(|_| pair.kind() == "pair")?;
        let key = match key.kind() {
            "string" => plain_string(key, src)?,
            "integer" => integer(&text(key, src))?.to_string(),
            _ => return None,
        };
        out.insert(key, value_at(pair.child_by_field_name("value")?, src, depth + 1));
    }
    Some(Value::Object(out.into_iter().collect()))
}

/// The decoded text of a string literal that has no `f` or `b` prefix and no interpolation.
pub(super) fn plain_string(node: Node, src: &[u8]) -> Option<String> {
    let (start, end) = delimiters(node)?;
    let delimiter = text(start, src);
    if delimiter.ends_with('`') {
        return None;
    }
    let flags = prefix(&delimiter);
    if flags.chars().any(|c| c != 'r' && c != 'u') {
        return None;
    }
    let mut cursor = node.walk();
    if node.children(&mut cursor).any(|c| c.kind() == "interpolation") {
        return None;
    }
    let raw = String::from_utf8_lossy(&src[start.end_byte()..end.start_byte()]);
    Some(if flags.contains('r') {
        raw.into_owned()
    } else {
        unescape(&raw)
    })
}

/// `string_start` and `string_end`, the quotes with the prefix on the first.
pub(super) fn delimiters(node: Node) -> Option<(Node, Node)> {
    let start = node.child(0).filter(|n| n.kind() == "string_start")?;
    let end = node
        .child(node.child_count().checked_sub(1)?)
        .filter(|n| n.kind() == "string_end")?;
    Some((start, end))
}

/// The lowercased prefix flags of a string's opening delimiter, such as `rb` or `f`.
pub(super) fn prefix(delimiter: &str) -> String {
    delimiter.trim_end_matches(['\'', '"', '`']).to_ascii_lowercase()
}

fn concatenated(node: Node, src: &[u8]) -> Option<String> {
    named(node).into_iter().map(|part| plain_string(part, src)).collect()
}

/// Decodes Python's escapes; `\N{...}` and unknown escapes stay as written.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let escaped = &rest[i + 1..];
        match escape(escaped) {
            Some((decoded, used)) => {
                out.extend(decoded);
                rest = &escaped[used..];
            }
            None => {
                out.push('\\');
                rest = escaped;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character an escape stands for (none for a line continuation) and the bytes it takes
/// after the backslash.
fn escape(s: &str) -> Option<(Option<char>, usize)> {
    let c = match s.as_bytes().first()? {
        b'\n' => return Some((None, 1)),
        b'\r' => return Some((None, if s.as_bytes().get(1) == Some(&b'\n') { 2 } else { 1 })),
        b'x' => return code_point(s, 16, 2),
        b'u' => return code_point(s, 16, 4),
        b'U' => return code_point(s, 16, 8),
        b'0'..=b'7' => {
            let n = s.bytes().take(3).take_while(|b| (b'0'..=b'7').contains(b)).count();
            let c = char::from_u32(u32::from_str_radix(&s[..n], 8).ok()?)?;
            return Some((Some(c), n));
        }
        b'\\' => '\\',
        b'\'' => '\'',
        b'"' => '"',
        b'a' => '\x07',
        b'b' => '\x08',
        b'f' => '\x0c',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'v' => '\x0b',
        _ => return None,
    };
    Some((Some(c), 1))
}

fn code_point(s: &str, radix: u32, digits: usize) -> Option<(Option<char>, usize)> {
    let hex = s.get(1..1 + digits)?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let c = char::from_u32(u32::from_str_radix(hex, radix).ok()?)?;
    Some((Some(c), 1 + digits))
}

/// The leaf tokens of `node` without comments or trailing commas, with one space between two
/// word characters and after each comma. Plain strings read as their quoted value; bytes and
/// f-strings keep their source text.
pub(crate) fn canonical(node: Node, src: &[u8]) -> String {
    let mut out = String::new();
    let mut tokens = Tokens::new(|token: &str| {
        if token.is_empty() {
            return;
        }
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let after_comma = out.ends_with(',') && !matches!(token, ")" | "]" | "}");
        if after_comma || (out.ends_with(word) && token.starts_with(word)) {
            out.push(' ');
        }
        out.push_str(token);
    });
    visit(node, &mut |v: &Visit| {
        let n = v.node;
        if n.is_extra() {
            return false;
        }
        match n.kind() {
            "string" => {
                match plain_string(n, src) {
                    Some(s) => tokens.push(&quoted(&s)),
                    None => tokens.push(&text(n, src)),
                }
                false
            }
            "concatenated_string" => match concatenated(n, src) {
                Some(s) => {
                    tokens.push(&quoted(&s));
                    false
                }
                None => true,
            },
            "," if closes_single_tuple(v) => {
                tokens.mark(",");
                false
            }
            _ if n.child_count() == 0 => {
                tokens.push(&text(n, src));
                false
            }
            _ => true,
        }
    });
    tokens.finish();
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tree_sitter::Parser;

    use super::*;

    /// The value and canonical text of the expression `source`.
    fn read(source: &str) -> (Value, String) {
        let src = format!("x = {source}\n");
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
        let tree = parser.parse(&src, None).unwrap();
        let statement = tree.root_node().named_child(0).unwrap();
        let right = statement.named_child(0).unwrap().child_by_field_name("right").unwrap();
        (value(right, src.as_bytes()), canonical(right, src.as_bytes()))
    }

    fn value_of(source: &str) -> Value {
        read(source).0
    }

    #[test]
    fn literals_become_json() {
        for (source, expected) in [
            (r#""a\tb\x41é\101\N{DASH}\q""#, json!("a\tbAé\u{41}\\N{DASH}\\q")),
            (r"r'\d+'", json!(r"\d+")),
            (r"u'd\xe9j\xe0'", json!("déjà")),
            ("'''multi\nline'''", json!("multi\nline")),
            ("('a' \"b\"\n    'c')", json!("abc")),
            ("1_000", json!(1000)),
            ("0x1F", json!(31)),
            ("0o17", json!(15)),
            ("0b101", json!(5)),
            ("-3", json!(-3)),
            ("2.5", json!(2.5)),
            ("-0.5", json!(-0.5)),
            ("True", json!(true)),
            ("None", json!(null)),
            ("[1, (2, 'x'),]", json!([1, [2, "x"]])),
            ("{'b': 1, 2: [False]}", json!({"2": [false], "b": 1})),
            ("((1))", json!(1)),
        ] {
            assert_eq!(value_of(source), expected, "{source}");
        }
    }

    #[test]
    fn anything_else_is_an_expression() {
        for (source, expected) in [
            ("lambda self: self.env.company", "lambda self:self.env.company"),
            ("_('Name')", "_('Name')"),
            ("f'{a}'", "f'{a}'"),
            ("b'raw'", "b'raw'"),
            ("{'a', 'b'}", "{'a', 'b'}"),
            ("{**base, 'a': 1}", "{**base, 'a':1}"),
            ("0777", "0777"),
            ("99999999999999999999", "99999999999999999999"),
            ("a if not b else c", "a if not b else c"),
            ("fields.Date.today", "fields.Date.today"),
        ] {
            assert_eq!(value_of(source), json!({"expr": expected}), "{source}");
        }
    }

    #[test]
    fn canonical_text_ignores_layout() {
        let (_, a) = read("f(\n    'x',  # why\n    \"y\" 'z',\n    k = 1,\n)");
        let (_, b) = read("f('x', 'yz', k=1)");
        assert_eq!(a, b);
        assert_eq!(a, "f('x', 'yz', k=1)");
        assert_eq!(read(r#"g("it's\n")"#).1, r"g('it\'s\n')");
        assert_eq!(read("(a,)").1, "(a,)");
        assert_eq!(read("[a,]").1, "[a]");
    }
}
