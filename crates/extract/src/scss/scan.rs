//! One pass over a stylesheet's bytes: tokens, then the statements and blocks they make. Not a
//! parser: it knows brackets, strings, comments, `url(...)` and interpolation, which is all that
//! finding where a statement ends takes. Every loop is flat, so nesting costs no stack.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A quoted string, quotes included.
    Str,
    /// An unquoted `url(...)`, opaque so that `//` inside it is no comment.
    Url,
    /// `#{...}`.
    Interp,
    /// Whitespace or a comment: a separator.
    Space,
    /// One of `( ) [ ] { } , ; : !`.
    Punct(u8),
    /// Any other run, such as `$name`, `@mixin`, `%ph`, `#fff` or `62/16`.
    Word,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Token {
    pub kind: Kind,
    /// Byte offsets into the blob.
    pub start: usize,
    pub end: usize,
}

/// Line numbers of byte offsets.
pub(super) struct Lines(Vec<usize>);

impl Lines {
    pub(super) fn new(src: &[u8]) -> Self {
        Lines(
            src.iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(i, _)| i)
                .collect(),
        )
    }

    pub(super) fn line(&self, at: usize) -> u32 {
        self.0.partition_point(|&nl| nl < at) as u32 + 1
    }
}

/// Warnings as `(offset, "line N: ...")`.
pub(super) type Warnings = Vec<(usize, String)>;

fn warn(warnings: &mut Warnings, lines: &Lines, at: usize, what: &str) {
    warnings.push((at, format!("line {}: {what}", lines.line(at))));
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

fn is_punct(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b';' | b':' | b'!')
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b >= 0x80
}

/// Where a string scan stands: in a string with its quote, or in an interpolation inside one
/// with its brace depth.
enum Nested {
    Str(u8),
    Interp(usize),
}

/// The end of the string quoted at `at`, and whether it closed before the end of its line. An
/// interpolation inside it may hold strings of either quote; a stack, not recursion, tracks them.
fn string_end(src: &[u8], at: usize) -> (usize, bool) {
    let mut stack = vec![Nested::Str(src[at])];
    let mut j = at + 1;
    while j < src.len() {
        let b = src[j];
        match stack.last_mut().expect("the scan stops when the stack empties") {
            Nested::Str(quote) => match b {
                b'\\' if src[j + 1..].starts_with(b"\r\n") => j += 2,
                b'\\' => j += 1,
                b'\n' => return (j, false),
                b'#' if src.get(j + 1) == Some(&b'{') => {
                    stack.push(Nested::Interp(1));
                    j += 1;
                }
                b if b == *quote => {
                    stack.pop();
                    if stack.is_empty() {
                        return (j + 1, true);
                    }
                }
                _ => {}
            },
            Nested::Interp(depth) => match b {
                b'{' => *depth += 1,
                b'}' if *depth == 1 => {
                    stack.pop();
                }
                b'}' => *depth -= 1,
                b'"' | b'\'' => stack.push(Nested::Str(b)),
                _ => {}
            },
        }
        j += 1;
    }
    (src.len(), false)
}

/// The end of an unquoted `url(...)` starting at `at`, if there is one there: the first `)`
/// outside interpolation on its line, an interpolation ending where the lexer ends it. A scan
/// that finds none moves `fail` to where it stopped, and a later `url(` before that point is no
/// url either, so each byte is scanned once.
fn url_end(src: &[u8], at: usize, fail: &mut usize) -> Option<usize> {
    let head = src.get(at..at + 4)?;
    if !head.eq_ignore_ascii_case(b"url(") || (at > 0 && is_word_byte(src[at - 1])) {
        return None;
    }
    let inner = at + 4 + src[at + 4..].iter().take_while(|b| is_space(**b)).count();
    if matches!(src.get(inner), Some(b'"' | b'\'')) || inner < *fail {
        return None;
    }
    let mut j = inner;
    while j < src.len() {
        match src[j] {
            b'#' if src.get(j + 1) == Some(&b'{') => match interpolation_close(src, j) {
                Some(end) => j = end,
                None => {
                    j = src.len();
                    break;
                }
            },
            b')' => return Some(j + 1),
            b'\n' => break,
            _ => j += 1,
        }
    }
    *fail = j;
    None
}

/// The tokens of `src` from `start`, past a BOM.
pub(super) fn lex(src: &[u8], start: usize, lines: &Lines, warnings: &mut Warnings) -> Vec<Token> {
    let mut tokens = Vec::new();
    let (mut i, mut url_fail) = (start, 0);
    while i < src.len() {
        let b = src[i];
        let next = src.get(i + 1).copied();
        let (kind, end) = if is_space(b) {
            let run = src[i..].iter().take_while(|b| is_space(**b)).count();
            (Kind::Space, i + run)
        } else if b == b'/' && next == Some(b'*') {
            match src[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(at) => (Kind::Space, i + 2 + at + 2),
                None => {
                    warn(warnings, lines, i, "unclosed comment");
                    (Kind::Space, src.len())
                }
            }
        } else if b == b'/' && next == Some(b'/') {
            let run = src[i..].iter().take_while(|b| **b != b'\n').count();
            (Kind::Space, i + run)
        } else if b == b'"' || b == b'\'' {
            let (end, closed) = string_end(src, i);
            if !closed {
                warn(warnings, lines, i, "unclosed string");
            }
            (Kind::Str, end)
        } else if b == b'#' && next == Some(b'{') {
            (Kind::Interp, interpolation_end(src, i, lines, warnings))
        } else if is_punct(b) {
            (Kind::Punct(b), i + 1)
        } else if let Some(end) = url_end(src, i, &mut url_fail) {
            (Kind::Url, end)
        } else {
            let mut j = i + 1;
            while j < src.len() && !ends_word(src, j) {
                j += 1;
            }
            (Kind::Word, j)
        };
        tokens.push(Token { kind, start: i, end });
        i = end;
    }
    tokens
}

/// Whether a word stops before `at`.
fn ends_word(src: &[u8], at: usize) -> bool {
    let b = src[at];
    let next = src.get(at + 1).copied();
    is_space(b)
        || is_punct(b)
        || b == b'"'
        || b == b'\''
        || (b == b'/' && matches!(next, Some(b'/' | b'*')))
        || (b == b'#' && next == Some(b'{'))
}

/// The end of the interpolation at `at`, its nested braces counted and its strings skipped.
fn interpolation_end(src: &[u8], at: usize, lines: &Lines, warnings: &mut Warnings) -> usize {
    interpolation_close(src, at).unwrap_or_else(|| {
        warn(warnings, lines, at, "unclosed interpolation");
        src.len()
    })
}

/// `interpolation_end`, or `None` when the interpolation never closes.
fn interpolation_close(src: &[u8], at: usize) -> Option<usize> {
    let (mut depth, mut j) = (1usize, at + 2);
    while j < src.len() {
        match src[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j + 1);
                }
            }
            b'"' | b'\'' => {
                j = string_end(src, j).0;
                continue;
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// What the tokens make, as ranges of token indices.
#[derive(Debug, PartialEq)]
pub(super) enum Item {
    /// A statement without its `;`.
    Statement(Range<usize>),
    /// The header of a block, the tokens before its `{`.
    Open(Range<usize>),
    /// The body of the block opened last, between its braces.
    Close(Range<usize>),
}

/// Splits tokens into statements and blocks: `;` ends a statement outside brackets, `{` and `}`
/// always act on blocks, and a last declaration before `}` needs no `;`.
pub(super) fn split(tokens: &[Token], lines: &Lines, warnings: &mut Warnings) -> Vec<Item> {
    let mut items = Vec::new();
    let (mut depth, mut paren_at, mut pending) = (0usize, 0usize, 0usize);
    // The offset of each open block's header, and the index its body starts at.
    let mut open: Vec<(usize, usize)> = Vec::new();
    let solid = |range: &Range<usize>| tokens[range.clone()].iter().any(|t| t.kind != Kind::Space);
    let statement = |items: &mut Vec<Item>, range: Range<usize>| {
        if solid(&range) {
            items.push(Item::Statement(range));
        }
    };
    for (idx, token) in tokens.iter().enumerate() {
        let Kind::Punct(p) = token.kind else { continue };
        match p {
            b'(' | b'[' => {
                if depth == 0 {
                    paren_at = token.start;
                }
                depth += 1;
            }
            b')' | b']' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => {
                statement(&mut items, pending..idx);
                pending = idx + 1;
            }
            b'{' | b'}' => {
                if depth > 0 {
                    warn(warnings, lines, paren_at, "unclosed (");
                    depth = 0;
                }
                if p == b'{' {
                    let header = pending..idx;
                    let at = tokens[header.clone()]
                        .iter()
                        .find(|t| t.kind != Kind::Space)
                        .map_or(token.start, |t| t.start);
                    items.push(Item::Open(header));
                    open.push((at, idx + 1));
                } else {
                    statement(&mut items, pending..idx);
                    match open.pop() {
                        Some((_, body)) => items.push(Item::Close(body..idx)),
                        None => warn(warnings, lines, token.start, "unmatched }"),
                    }
                }
                pending = idx + 1;
            }
            _ => {}
        }
    }
    if depth > 0 {
        warn(warnings, lines, paren_at, "unclosed (");
    }
    statement(&mut items, pending..tokens.len());
    for (at, _) in open {
        warn(warnings, lines, at, "unclosed {");
    }
    items
}

/// Tokens as one canonical text, so that layout, comments and trailing commas are no change:
/// spaces become one, none after `(` and `[`, before `)`, `]` and `,`, or around `{`, `}` and
/// `;`; one after `,`; a `,` before `)` or `]` and a `;` before `}` are dropped. Spaces before
/// `(` and around `:` stay, since `a (b)` is a list and `a(b)` a call.
pub(super) fn render(tokens: &[Token], src: &[u8]) -> String {
    let mut solid: Vec<(Token, bool)> = Vec::new();
    let mut space = false;
    for token in tokens {
        if token.kind == Kind::Space {
            space = true;
        } else {
            solid.push((*token, space));
            space = false;
        }
    }
    let mut out = String::new();
    let mut prev: Option<Kind> = None;
    for (i, (token, space)) in solid.iter().enumerate() {
        let next = solid.get(i + 1).map(|(t, _)| t.kind);
        match (token.kind, next) {
            (Kind::Punct(b','), Some(Kind::Punct(b')' | b']'))) | (Kind::Punct(b';'), Some(Kind::Punct(b'}'))) => {
                continue;
            }
            _ => {}
        }
        let gap = match (prev, token.kind) {
            (None, _) => false,
            (_, Kind::Punct(b')' | b']' | b',')) => false,
            (Some(Kind::Punct(b',')), _) => true,
            (Some(Kind::Punct(b'(' | b'[')), _) => false,
            (Some(Kind::Punct(b'{' | b'}' | b';')), _) | (_, Kind::Punct(b'{' | b'}' | b';')) => false,
            _ => *space,
        };
        if gap {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(&src[token.start..token.end]));
        prev = Some(token.kind);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lexed(src: &str) -> (Vec<(Kind, String)>, Vec<String>) {
        let mut warnings = Vec::new();
        let tokens = lex(src.as_bytes(), 0, &Lines::new(src.as_bytes()), &mut warnings);
        let tokens = tokens
            .iter()
            .filter(|t| t.kind != Kind::Space)
            .map(|t| (t.kind, src[t.start..t.end].to_string()))
            .collect();
        (tokens, warnings.into_iter().map(|(_, w)| w).collect())
    }

    fn rendered(src: &str) -> String {
        let mut warnings = Vec::new();
        let tokens = lex(src.as_bytes(), 0, &Lines::new(src.as_bytes()), &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        render(&tokens, src.as_bytes())
    }

    fn items(src: &str) -> (Vec<String>, Vec<String>) {
        let lines = Lines::new(src.as_bytes());
        let mut warnings = Vec::new();
        let tokens = lex(src.as_bytes(), 0, &lines, &mut warnings);
        let items = split(&tokens, &lines, &mut warnings)
            .into_iter()
            .map(|item| match item {
                Item::Statement(r) => format!("stmt {}", render(&tokens[r], src.as_bytes())),
                Item::Open(r) => format!("open {}", render(&tokens[r], src.as_bytes())),
                Item::Close(r) => format!("close {}", render(&tokens[r], src.as_bytes())),
            })
            .collect();
        (items, warnings.into_iter().map(|(_, w)| w).collect())
    }

    #[test]
    fn strings_urls_interpolation_and_comments_lex_whole() {
        let (tokens, warnings) =
            lexed("$a: \"x // y\" url(http://x/y) url(\"a//b\") #{$a + {b: 1}} /* c\n d */ 62/16 Url(a);");
        assert!(warnings.is_empty(), "{warnings:?}");
        let kinds: Vec<(Kind, &str)> = tokens.iter().map(|(k, t)| (*k, t.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (Kind::Word, "$a"),
                (Kind::Punct(b':'), ":"),
                (Kind::Str, "\"x // y\""),
                (Kind::Url, "url(http://x/y)"),
                (Kind::Word, "url"),
                (Kind::Punct(b'('), "("),
                (Kind::Str, "\"a//b\""),
                (Kind::Punct(b')'), ")"),
                (Kind::Interp, "#{$a + {b: 1}}"),
                (Kind::Word, "62/16"),
                (Kind::Url, "Url(a)"),
                (Kind::Punct(b';'), ";"),
            ]
        );
    }

    #[test]
    fn urls_skip_interpolation_and_strings_nest_it() {
        let (tokens, warnings) = lexed("url(#{map-get($m, k)}/x.png) \"a #{\"b\"} c\" 'd\\\r\ne' x");
        assert!(warnings.is_empty(), "{warnings:?}");
        let kinds: Vec<(Kind, &str)> = tokens.iter().map(|(k, t)| (*k, t.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (Kind::Url, "url(#{map-get($m, k)}/x.png)"),
                (Kind::Str, "\"a #{\"b\"} c\""),
                (Kind::Str, "'d\\\r\ne'"),
                (Kind::Word, "x"),
            ]
        );
        let (tokens, warnings) = lexed("$a: url(#{\"{\"} url(http://x));");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(tokens[2], (Kind::Url, "url(#{\"{\"} url(http://x)".to_string()));
        let (tokens, _) = lexed("url(a\nb) url(c)");
        assert_eq!(tokens[0], (Kind::Word, "url".to_string()));
        assert_eq!(tokens.last().unwrap(), &(Kind::Url, "url(c)".to_string()));
    }

    #[test]
    fn a_bom_is_skipped_and_offsets_stay_in_the_blob() {
        let src = b"\xEF\xBB\xBF$a: 1;";
        let mut warnings = Vec::new();
        let tokens = lex(src, 3, &Lines::new(src), &mut warnings);
        assert_eq!(&src[tokens[0].start..tokens[0].end], b"$a");
    }

    #[test]
    fn unclosed_forms_warn_at_their_line() {
        assert_eq!(lexed("$a: 1;\n/* open").1, ["line 2: unclosed comment"]);
        assert_eq!(lexed("$a: 'open\n;").1, ["line 1: unclosed string"]);
        assert_eq!(lexed("\n\n$a: #{open").1, ["line 3: unclosed interpolation"]);
        let (tokens, _) = lexed("$a: 'open\n$b: 2;");
        assert_eq!(tokens[2], (Kind::Str, "'open".to_string()));
        assert_eq!(tokens[3], (Kind::Word, "$b".to_string()));
    }

    #[test]
    fn rendering_drops_layout_comments_and_trailing_commas() {
        assert_eq!(
            rendered("(\n  x: 1, // one\n  y: (2,3),\n  /* last */\n  z: 3,\n)"),
            "(x: 1, y: (2, 3), z: 3)"
        );
        assert_eq!(rendered("rgba(0,0,0,.25)"), "rgba(0, 0, 0, .25)");
        assert_eq!(rendered("map-get( $a , b )!default"), "map-get($a, b)!default");
        assert_eq!(rendered("a (b)"), "a (b)");
        assert_eq!(rendered("a(b)"), "a(b)");
        assert_eq!(rendered("a :hover { b : c ; }"), "a :hover{b : c}");
        assert_eq!(rendered("[a ,b]"), "[a, b]");
    }

    #[test]
    fn splitting_statements_and_blocks() {
        assert_eq!(
            items("$a: 1;\n.x { b: c; d: (e; f) }\n@if a { $b: 2 }"),
            (
                vec![
                    "stmt $a: 1".to_string(),
                    "open .x".into(),
                    "stmt b: c".into(),
                    "stmt d: (e;f)".into(),
                    "close b: c;d: (e;f)".into(),
                    "open @if a".into(),
                    "stmt $b: 2".into(),
                    "close $b: 2".into(),
                ],
                vec![]
            )
        );
        assert_eq!(items("$a: 1;\n}\n$b: 2;").1, ["line 2: unmatched }"],);
        assert_eq!(items("$a: 1;\n@mixin m {\n  a: b;\n").1, ["line 2: unclosed {"]);
        let (found, warnings) = items("$a: (1,\n$b: 2;\n.x { c: d }");
        assert_eq!(warnings, ["line 1: unclosed ("]);
        assert_eq!(found[0], "open $a: (1, $b: 2;.x");
        assert_eq!(items("$a: (1").1, ["line 1: unclosed ("]);
    }
}
