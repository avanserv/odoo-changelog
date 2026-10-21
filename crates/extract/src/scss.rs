//! Stylesheets an addon serves: the variables, map entries, mixins, functions, placeholders and
//! `:root` custom properties a file declares at global scope, which themes and other addons
//! override or use, and what the file includes, extends and imports. Odoo compiles with libsass,
//! so there is no `@use` or `@forward` to follow.

mod scan;

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use anyhow::Result;
use serde_json::Value;

use crate::bound::{Budget, ENVELOPE, bounded, json_len, warn_capped};
use crate::hash::digest;
use crate::path::{static_addon, static_file};
use crate::{Extractor, Span, Symbol, SymbolTable};
use scan::{Item, Kind, Lines, Token, Warnings};

const VARIABLE: &str = "scss_variable";
const MIXIN: &str = "scss_mixin";
const FUNCTION: &str = "scss_function";
const PLACEHOLDER: &str = "scss_placeholder";
const MAP_ENTRY: &str = "scss_map_entry";
const STYLESHEET: &str = "scss_stylesheet";
const PROPERTY: &str = "css_property";

/// Blocks that run in the scope around them, so that a theme's `@if` assigns its globals.
const CONTROL: [&str; 6] = ["@if", "@else", "@elseif", "@each", "@for", "@while"];

/// The most control headers a `within` lists; deeper ones are folded into the last.
const MAX_WITHIN: usize = 8;

/// The serialized output one file may give, per byte of it plus a constant, past which the
/// reader drops what it has yet to give and warns once.
const OUTPUT_PER_BYTE: usize = 24;
const OUTPUT_SLACK: usize = 1 << 20;

pub struct Scss;

impl Extractor for Scss {
    fn name(&self) -> &'static str {
        "scss"
    }

    /// Bump with any change to the output.
    fn version(&self) -> u32 {
        2
    }

    /// Every stylesheet an addon serves, vendored Bootstrap included: themes override its
    /// `!default` variables, so its defaults are half of every override.
    fn claims(&self, path: &str) -> bool {
        (path.ends_with(".scss") || path.ends_with(".css"))
            && static_file(path).is_some_and(|(_, rest)| !rest.iter().any(|s| matches!(*s, "tests" | "test")))
    }

    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        Ok(read(blob))
    }

    /// `<name>@<addon>`, names keeping their `$` or `%`: a theme's override of `$spacer` is
    /// `$spacer@theme_x` beside `$spacer@web`. A stylesheet is its path as manifests list it,
    /// `<addon>/static/...`.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        if symbol.kind == STYLESHEET {
            return static_file(path).map_or_else(String::new, |(addon, rest)| {
                format!("{addon}/static/{}", rest.join("/"))
            });
        }
        format!("{}@{}", symbol.name, static_addon(path).unwrap_or_default())
    }
}

fn read(blob: &[u8]) -> SymbolTable {
    let start = if blob.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let lines = Lines::new(blob);
    let mut warnings = Warnings::new();
    let tokens = scan::lex(blob, start, &lines, &mut warnings);
    let items = scan::split(&tokens, &lines, &mut warnings);
    let mut reader = Reader {
        src: blob,
        tokens: &tokens,
        lines: &lines,
        open: Vec::new(),
        top: Rc::new(Within::default()),
        variables: BTreeMap::new(),
        entries: BTreeMap::new(),
        properties: BTreeMap::new(),
        imports: Vec::new(),
        includes: BTreeSet::new(),
        extends: BTreeSet::new(),
        table: SymbolTable::default(),
        budget: Budget::new(blob, OUTPUT_PER_BYTE, OUTPUT_SLACK),
        warnings,
    };
    for item in items {
        match item {
            Item::Statement(range) => reader.statement(range),
            Item::Open(range) => reader.open(range),
            Item::Close(range) => reader.close(range),
        }
    }
    reader.finish()
}

/// The headers of the control blocks around a statement, outermost first, each `bounded`.
/// Past `MAX_WITHIN` levels, the last entry is `~<hash>` of the headers from that level on, so
/// that no statement repeats more than a bounded text, and a level costs a bounded size too.
#[derive(Default)]
struct Within {
    /// The first headers, up to `MAX_WITHIN - 1`, shared by every deeper level.
    lead: Rc<Vec<String>>,
    /// The `MAX_WITHIN`th entry: that level's header, or `~<hash>` below it.
    last: Option<String>,
    /// From the `MAX_WITHIN`th level on, the digest of the headers, each chained to the last.
    fold: Option<[u8; 32]>,
}

impl Within {
    fn nest(&self, header: String) -> Within {
        let header = bounded(header);
        if self.last.is_none() && self.lead.len() + 1 < MAX_WITHIN {
            let mut lead = (*self.lead).clone();
            lead.push(header);
            return Within {
                lead: Rc::new(lead),
                last: None,
                fold: None,
            };
        }
        let mut hasher = blake3::Hasher::new();
        if let Some(fold) = &self.fold {
            hasher.update(fold);
            hasher.update(&[0]);
        }
        hasher.update(header.as_bytes());
        let last = match self.last {
            None => header,
            Some(_) => format!("~{}", digest(&hasher)),
        };
        Within {
            lead: self.lead.clone(),
            last: Some(last),
            fold: Some(*hasher.finalize().as_bytes()),
        }
    }

    fn entries(&self) -> Vec<&str> {
        self.lead.iter().chain(&self.last).map(String::as_str).collect()
    }

    fn value(&self) -> Option<Value> {
        let entries = self.entries();
        (!entries.is_empty()).then(|| entries.into_iter().map(Value::from).collect())
    }
}

/// A mixin, function or placeholder whose body is still being read.
struct Def {
    kind: &'static str,
    name: String,
    span: Span,
    params: Option<Vec<String>>,
    within: Rc<Within>,
}

struct Block {
    /// Whether statements directly inside it are at global scope.
    global: bool,
    /// Whether it is a global `:root` rule, whose custom properties are symbols.
    root: bool,
    within: Rc<Within>,
    def: Option<Def>,
}

/// The first global assignment of a variable, a map entry or a custom property, and the later
/// ones.
struct Variable {
    span: Span,
    first: BTreeMap<String, Value>,
    reassigned: Vec<Value>,
}

/// Records `entry` as the first assignment of `name` in `store`, or as a later one, within the
/// budget.
fn assign(
    store: &mut BTreeMap<String, Variable>,
    budget: &mut Budget,
    name: String,
    span: Span,
    entry: BTreeMap<String, Value>,
) {
    let entry_size = serde_json::to_vec(&entry).map_or(usize::MAX, |b| b.len());
    let at = span.start as usize;
    match store.get_mut(&name) {
        Some(variable) => {
            if budget.afford(entry_size, at) {
                variable.reassigned.push(Value::from_iter(entry));
            }
        }
        None => {
            if budget.afford(entry_size + json_len(&name) + ENVELOPE, at) {
                let variable = Variable {
                    span,
                    first: entry,
                    reassigned: Vec::new(),
                };
                store.insert(name, variable);
            }
        }
    }
}

/// Sass reads `_` and `-` in a name as one, so `$o_brand_primary` is `$o-brand-primary`.
fn fold(name: &str) -> String {
    name.replace('_', "-")
}

/// A string's content without its quotes, or the text of any other key.
fn unquoted(text: &str) -> &str {
    let quoted = text.len() >= 2 && matches!(text.as_bytes()[0], b'"' | b'\'') && text.ends_with(&text[..1]);
    if quoted { &text[1..text.len() - 1] } else { text }
}

struct Reader<'t> {
    src: &'t [u8],
    tokens: &'t [Token],
    lines: &'t Lines,
    open: Vec<Block>,
    /// The `within` of the top level: none.
    top: Rc<Within>,
    variables: BTreeMap<String, Variable>,
    entries: BTreeMap<String, Variable>,
    properties: BTreeMap<String, Variable>,
    /// What `@import`, `@include` and `@extend` name, anywhere in the file.
    imports: Vec<String>,
    includes: BTreeSet<String>,
    extends: BTreeSet<String>,
    table: SymbolTable,
    /// Spent in document order, by assignments and definitions.
    budget: Budget,
    warnings: Warnings,
}

impl Reader<'_> {
    fn text(&self, token: Token) -> String {
        String::from_utf8_lossy(&self.src[token.start..token.end]).into_owned()
    }

    fn render(&self, range: std::ops::Range<usize>) -> String {
        scan::render(&self.tokens[range], self.src)
    }

    /// The indices of the tokens of `range` that are not space.
    fn solid(&self, range: std::ops::Range<usize>) -> Vec<usize> {
        range.filter(|&i| self.tokens[i].kind != Kind::Space).collect()
    }

    fn warn(&mut self, at: usize, what: impl std::fmt::Display) {
        let line = self.lines.line(at);
        self.warnings.push((at, format!("line {line}: {what}")));
    }

    fn span(token: Token) -> Span {
        Span {
            start: token.start as u32,
            end: token.end as u32,
        }
    }

    /// The scope a statement or block at this point sits in: whether it is global, and its
    /// control headers.
    fn scope(&self) -> (bool, Rc<Within>) {
        match self.open.last() {
            Some(block) => (block.global, block.within.clone()),
            None => (true, self.top.clone()),
        }
    }

    /// Reads a statement by its head: `@include`, `@extend` and `@import` anywhere, `--name:` in
    /// a global `:root` rule, and a variable assignment, `$name:` then a value with trailing
    /// `!default` and `!global` flags in any order. One with `!global` assigns the global at any
    /// depth. A global map literal, or a `map-merge` of literals, also assigns its entries, which
    /// keep the `default` flag.
    fn statement(&mut self, range: std::ops::Range<usize>) {
        let solid = self.solid(range.clone());
        let Some(&head) = solid.first() else { return };
        match &self.src[self.tokens[head].start..self.tokens[head].end] {
            b"@include" => return self.include(&solid[1..]),
            b"@extend" => return self.extend(&solid[1..]),
            b"@import" => return self.import(&solid[1..]),
            _ => {}
        }
        if self.open.last().is_some_and(|b| b.root) && self.src[self.tokens[head].start..].starts_with(b"--") {
            return self.property(&solid);
        }
        let [name, colon, ..] = solid[..] else { return };
        let name_token = self.tokens[name];
        if name_token.kind != Kind::Word
            || self.src[name_token.start] != b'$'
            || self.tokens[colon].kind != Kind::Punct(b':')
        {
            return;
        }
        let mut value = &solid[2..];
        let mut flags = BTreeMap::new();
        while let [rest @ .., bang, flag] = value {
            let flag_token = self.tokens[*flag];
            let flag_text = &self.src[flag_token.start..flag_token.end];
            if self.tokens[*bang].kind != Kind::Punct(b'!') || !matches!(flag_text, b"default" | b"global") {
                break;
            }
            flags.insert(String::from_utf8_lossy(flag_text).into_owned(), Value::from(true));
            value = rest;
        }
        let (global, within) = self.scope();
        if !global && !flags.contains_key("global") {
            return;
        }
        let name = fold(&self.text(name_token));
        let Some((&first, &last)) = value.first().zip(value.last()) else {
            return self.warn(name_token.start, format!("{name} without a value ignored"));
        };
        let within = within.value();
        for (key, at, entry) in self.map_entries(value) {
            let mut attrs = BTreeMap::from([("value".to_string(), Value::from(self.render(entry)))]);
            if let Some(default) = flags.get("default") {
                attrs.insert("default".into(), default.clone());
            }
            if let Some(within) = &within {
                attrs.insert("within".into(), within.clone());
            }
            let key = format!("{name}#{}", bounded(key));
            assign(&mut self.entries, &mut self.budget, key, Self::span(at), attrs);
        }
        let mut entry = flags;
        entry.insert("value".into(), self.render(first..last + 1).into());
        if let Some(within) = within {
            entry.insert("within".into(), within);
        }
        assign(
            &mut self.variables,
            &mut self.budget,
            name,
            Self::span(name_token),
            entry,
        );
    }

    /// The index in `solid` of the bracket closing the one at `open`, if any.
    fn closing(&self, solid: &[usize], open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, &t) in solid.iter().enumerate().skip(open) {
            match self.tokens[t].kind {
                Kind::Punct(b'(' | b'[') => depth += 1,
                Kind::Punct(b')' | b']') => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The parts of `solid` between top-level commas, empty ones dropped.
    fn commas<'s>(&self, solid: &'s [usize]) -> Vec<&'s [usize]> {
        let (mut parts, mut depth, mut from) = (Vec::new(), 0usize, 0);
        for (i, &t) in solid.iter().enumerate() {
            match self.tokens[t].kind {
                Kind::Punct(b'(' | b'[') => depth += 1,
                Kind::Punct(b')' | b']') => depth = depth.saturating_sub(1),
                Kind::Punct(b',') if depth == 0 => {
                    parts.push(&solid[from..i]);
                    from = i + 1;
                }
                _ => {}
            }
        }
        parts.push(&solid[from..]);
        parts.retain(|p| !p.is_empty());
        parts
    }

    /// The tokens inside the brackets `solid` is wholly wrapped in.
    fn wrapped<'s>(&self, solid: &'s [usize]) -> Option<&'s [usize]> {
        let open = *solid.first()?;
        (self.tokens[open].kind == Kind::Punct(b'(') && self.closing(solid, 0) == Some(solid.len() - 1))
            .then(|| &solid[1..solid.len() - 1])
    }

    /// The top-level entries of a map literal `(k: v, ...)`, or of the literal maps a
    /// `map-merge(a, b)` merges, in order: each key, unquoted, its token, and the token range of
    /// its value. A list gives none, and neither does a key that is no literal text, such as the
    /// `$k` of a loop.
    fn map_entries(&self, value: &[usize]) -> Vec<(String, Token, std::ops::Range<usize>)> {
        let maps = match value {
            [call, rest @ ..] if &self.src[self.tokens[*call].start..self.tokens[*call].end] == b"map-merge" => {
                match self.wrapped(rest).map(|args| self.commas(args)).as_deref() {
                    Some([a, b]) => [self.wrapped(a), self.wrapped(b)].into_iter().flatten().collect(),
                    _ => Vec::new(),
                }
            }
            _ => self.wrapped(value).into_iter().collect(),
        };
        let mut out = Vec::new();
        for map in maps {
            let mut entries = Vec::new();
            for entry in self.commas(map) {
                let [key, colon, ref value @ ..] = entry[..] else {
                    entries.clear();
                    break;
                };
                let (Some(&first), Some(&last)) = (value.first(), value.last()) else {
                    entries.clear();
                    break;
                };
                if self.tokens[colon].kind != Kind::Punct(b':') {
                    entries.clear();
                    break;
                }
                let token = self.tokens[key];
                let text = self.text(token);
                let literal = match token.kind {
                    Kind::Str => !text.contains("#{"),
                    Kind::Word => !text.starts_with('$'),
                    _ => false,
                };
                if literal {
                    entries.push((unquoted(&text).to_string(), token, first..last + 1));
                }
            }
            out.extend(entries);
        }
        out
    }

    /// `@include <mixin>`, the name folded like a mixin's.
    fn include(&mut self, rest: &[usize]) {
        if let Some(&name) = rest.first().filter(|&&t| self.tokens[t].kind == Kind::Word) {
            self.includes.insert(bounded(fold(&self.text(self.tokens[name]))));
        }
    }

    /// `@extend <selector>`, without `!optional`.
    fn extend(&mut self, mut rest: &[usize]) {
        if let [head @ .., bang, flag] = rest
            && self.tokens[*bang].kind == Kind::Punct(b'!')
            && self.text(self.tokens[*flag]) == "optional"
        {
            rest = head;
        }
        if let Some((&first, &last)) = rest.first().zip(rest.last()) {
            self.extends.insert(bounded(self.render(first..last + 1)));
        }
    }

    /// `@import a, b`: each a string's content, or the text of a `url(...)`.
    fn import(&mut self, rest: &[usize]) {
        for part in self.commas(rest) {
            let import = match part {
                [one] if self.tokens[*one].kind == Kind::Str => unquoted(&self.text(self.tokens[*one])).to_string(),
                _ => self.render(part[0]..part[part.len() - 1] + 1),
            };
            self.imports.push(bounded(import));
        }
    }

    /// `--name: value` in a global `:root` rule.
    fn property(&mut self, solid: &[usize]) {
        let [name, colon, ref value @ ..] = solid[..] else {
            return;
        };
        let (Some(&first), Some(&last)) = (value.first(), value.last()) else {
            return;
        };
        let name_token = self.tokens[name];
        if name_token.kind != Kind::Word
            || !self.src[name_token.start..].starts_with(b"--")
            || self.tokens[colon].kind != Kind::Punct(b':')
        {
            return;
        }
        let mut entry = BTreeMap::from([("value".to_string(), Value::from(self.render(first..last + 1)))]);
        if let Some(within) = self.open.last().and_then(|b| b.within.value()) {
            entry.insert("within".into(), within);
        }
        let name = self.text(name_token);
        assign(
            &mut self.properties,
            &mut self.budget,
            name,
            Self::span(name_token),
            entry,
        );
    }

    /// A block: control flow keeps the scope global, a mixin, function or placeholder at
    /// global scope starts a definition, and anything else opens a local scope.
    fn open(&mut self, header: std::ops::Range<usize>) {
        let (global, within) = self.scope();
        let solid = self.solid(header.clone());
        if solid.first().is_some_and(|&t| self.text(self.tokens[t]) == "@include") {
            self.include(&solid[1..]);
        }
        let word = |i: usize| -> Option<String> {
            let token = self.tokens[*solid.get(i)?];
            (token.kind == Kind::Word).then(|| self.text(token))
        };
        let first = word(0).unwrap_or_default();
        if CONTROL.contains(&first.as_str()) {
            let within = Rc::new(within.nest(self.render(header)));
            let root = self.open.last().is_some_and(|b| b.root);
            return self.open.push(Block {
                global,
                root,
                within,
                def: None,
            });
        }
        let root = global && self.render(header) == ":root";
        let def = match (global, first.as_str()) {
            (true, "@mixin" | "@function") => word(1).map(|name| {
                let kind = if first == "@mixin" { MIXIN } else { FUNCTION };
                let params = self.params(&solid[2..]);
                Def {
                    kind,
                    name: fold(&name),
                    span: Self::span(self.tokens[solid[1]]),
                    params,
                    within: within.clone(),
                }
            }),
            (true, placeholder) if placeholder.starts_with('%') && solid.len() == 1 => Some(Def {
                kind: PLACEHOLDER,
                name: placeholder.to_string(),
                span: Self::span(self.tokens[solid[0]]),
                params: None,
                within: within.clone(),
            }),
            _ => None,
        };
        self.open.push(Block {
            global: false,
            root,
            within,
            def,
        });
    }

    /// The top-level comma-separated parameters in the parentheses `rest` opens with, each
    /// rendered, or `None` when there are none.
    fn params(&self, rest: &[usize]) -> Option<Vec<String>> {
        let (&open, rest) = rest.split_first()?;
        if self.tokens[open].kind != Kind::Punct(b'(') {
            return None;
        }
        let (mut params, mut depth, mut start) = (Vec::new(), 0usize, open + 1);
        for &i in rest {
            match self.tokens[i].kind {
                Kind::Punct(b'(' | b'[') => depth += 1,
                Kind::Punct(b')' | b']') if depth > 0 => depth -= 1,
                Kind::Punct(b',' | b')') if depth == 0 => {
                    let param = self.render(start..i);
                    if !param.is_empty() {
                        params.push(param);
                    }
                    start = i + 1;
                    if self.tokens[i].kind == Kind::Punct(b')') {
                        break;
                    }
                }
                _ => {}
            }
        }
        (!params.is_empty()).then_some(params)
    }

    fn close(&mut self, body: std::ops::Range<usize>) {
        let Some(Block { def: Some(def), .. }) = self.open.pop() else {
            return;
        };
        if self.table.contains(def.kind, &def.name) {
            return self.warn(
                def.span.start as usize,
                format!("duplicate {} {} dropped", def.kind, def.name),
            );
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.render(body).as_bytes());
        let mut attrs = BTreeMap::from([("body".to_string(), Value::from(digest(&hasher)))]);
        if let Some(params) = def.params {
            attrs.insert("params".into(), params.into());
        }
        if let Some(within) = def.within.value() {
            attrs.insert("within".into(), within);
        }
        let symbol = Symbol {
            kind: def.kind.into(),
            name: def.name,
            line: self.lines.line(def.span.start as usize),
            span: def.span,
            attrs,
        };
        if self.budget.symbol(&symbol) {
            self.table.insert(symbol);
        }
    }

    fn finish(mut self) -> SymbolTable {
        let stores = [
            (VARIABLE, std::mem::take(&mut self.variables)),
            (MAP_ENTRY, std::mem::take(&mut self.entries)),
            (PROPERTY, std::mem::take(&mut self.properties)),
        ];
        for (kind, store) in stores {
            for (name, variable) in store {
                let mut attrs = variable.first;
                if !variable.reassigned.is_empty() {
                    attrs.insert("reassigned".into(), variable.reassigned.into());
                }
                self.table.insert(Symbol {
                    kind: kind.into(),
                    name,
                    line: self.lines.line(variable.span.start as usize),
                    span: variable.span,
                    attrs,
                });
            }
        }
        let mut attrs = BTreeMap::new();
        if !self.imports.is_empty() {
            attrs.insert(
                "imports".to_string(),
                Value::from_iter(std::mem::take(&mut self.imports)),
            );
        }
        for (key, set) in [("includes", &mut self.includes), ("extends", &mut self.extends)] {
            if !set.is_empty() {
                attrs.insert(key.to_string(), Value::from_iter(std::mem::take(set)));
            }
        }
        let stylesheet = Symbol {
            kind: STYLESHEET.into(),
            name: String::new(),
            line: 1,
            span: Span::default(),
            attrs,
        };
        if self.budget.symbol(&stylesheet) {
            self.table.insert(stylesheet);
        }
        warn_capped(&mut self.table, self.warnings, &self.budget, "declarations");
        self.table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_every_stylesheet_an_addon_serves() {
        for path in [
            "addons/web/static/src/scss/primary_variables.scss",
            "addons/web/static/lib/bootstrap/scss/_variables.scss",
            "theme_bistro/static/src/scss/bootstrap_overridden.scss",
            "addons/web/static/fonts/fonts.scss",
            "addons/web/static/src/x.css",
            "addons/web/static/lib/pdfjs/web/viewer.css",
        ] {
            assert!(Scss.claims(path), "{path}");
        }
        for path in [
            "addons/point_of_sale/static/tests/unit/qunit_tests.scss",
            "addons/web/static/tests/x.css",
            "scss/x.scss",
        ] {
            assert!(!Scss.claims(path), "{path}");
        }
    }

    #[test]
    fn qualnames_keep_their_sigil_and_end_with_the_addon() {
        let table = read(b"$a: 1;\n@mixin m { a: b; }\n@function f($x) { @return $x; }\n%p { a: b; }\n");
        let path = "theme_x/static/src/scss/a.scss";
        let rows: Vec<(&str, String)> = table.iter().map(|s| (s.kind.as_str(), Scss.qualify(path, s))).collect();
        assert_eq!(
            rows,
            [
                (FUNCTION, "f@theme_x".to_string()),
                (MIXIN, "m@theme_x".to_string()),
                (PLACEHOLDER, "%p@theme_x".to_string()),
                (STYLESHEET, "theme_x/static/src/scss/a.scss".to_string()),
                (VARIABLE, "$a@theme_x".to_string()),
            ]
        );
    }

    #[test]
    fn within_folds_deep_control_flow_into_its_last_header() {
        let mut within = Within::default();
        for i in 0..MAX_WITHIN {
            within = within.nest(format!("@if {i}"));
        }
        let entries = within.entries();
        assert_eq!(entries.len(), MAX_WITHIN);
        assert_eq!(entries[MAX_WITHIN - 1], format!("@if {}", MAX_WITHIN - 1));
        let deeper = within.nest("@if x".into());
        let other = within.nest("@if y".into());
        let deep = deeper.entries();
        assert_eq!(deep.len(), MAX_WITHIN);
        assert_eq!(deep[..MAX_WITHIN - 1], entries[..MAX_WITHIN - 1]);
        assert!(deep[MAX_WITHIN - 1].starts_with('~'));
        assert_ne!(deep, other.entries());
        let deepest = deeper.nest("@if z".into());
        assert_eq!(deepest.entries().len(), MAX_WITHIN);
        assert_ne!(deepest.entries(), deep);
        assert!(Rc::ptr_eq(&deepest.lead, &within.lead));
    }
}
