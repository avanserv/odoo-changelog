//! Stylesheets an addon serves: the variables, mixins, functions and placeholders a file
//! declares at global scope, which themes and other addons override or use. Odoo compiles with
//! libsass, so there is no `@use` or `@forward` to follow.

mod scan;

use std::collections::BTreeMap;
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
        1
    }

    /// Every stylesheet an addon serves, vendored Bootstrap included: themes override its
    /// `!default` variables, so its defaults are half of every override.
    fn claims(&self, path: &str) -> bool {
        path.ends_with(".scss")
            && static_file(path).is_some_and(|(_, rest)| !rest.iter().any(|s| matches!(*s, "tests" | "test")))
    }

    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        Ok(read(blob))
    }

    /// `<name>@<addon>`, names keeping their `$` or `%`: a theme's override of `$spacer` is
    /// `$spacer@theme_x` beside `$spacer@web`.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
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
    within: Rc<Within>,
    def: Option<Def>,
}

/// The first global assignment of a variable, and the later ones.
struct Variable {
    span: Span,
    first: BTreeMap<String, Value>,
    reassigned: Vec<Value>,
}

struct Reader<'t> {
    src: &'t [u8],
    tokens: &'t [Token],
    lines: &'t Lines,
    open: Vec<Block>,
    /// The `within` of the top level: none.
    top: Rc<Within>,
    variables: BTreeMap<String, Variable>,
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

    /// A variable assignment: `$name:` then a value, with trailing `!default` and `!global`
    /// flags in any order. One with `!global` assigns the global at any depth.
    fn statement(&mut self, range: std::ops::Range<usize>) {
        let solid = self.solid(range.clone());
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
        let name = self.text(name_token);
        let Some((&first, &last)) = value.first().zip(value.last()) else {
            return self.warn(name_token.start, format!("{name} without a value ignored"));
        };
        let mut entry = flags;
        entry.insert("value".into(), self.render(first..last + 1).into());
        if let Some(within) = within.value() {
            entry.insert("within".into(), within);
        }
        let entry_size = serde_json::to_vec(&entry).map_or(usize::MAX, |b| b.len());
        match self.variables.get_mut(&name) {
            Some(variable) => {
                if self.budget.afford(entry_size, name_token.start) {
                    variable.reassigned.push(Value::from_iter(entry));
                }
            }
            None => {
                if self
                    .budget
                    .afford(entry_size + json_len(&name) + ENVELOPE, name_token.start)
                {
                    let span = Self::span(name_token);
                    let variable = Variable {
                        span,
                        first: entry,
                        reassigned: Vec::new(),
                    };
                    self.variables.insert(name, variable);
                }
            }
        }
    }

    /// A block: control flow keeps the scope global, a mixin, function or placeholder at
    /// global scope starts a definition, and anything else opens a local scope.
    fn open(&mut self, header: std::ops::Range<usize>) {
        let (global, within) = self.scope();
        let solid = self.solid(header.clone());
        let word = |i: usize| -> Option<String> {
            let token = self.tokens[*solid.get(i)?];
            (token.kind == Kind::Word).then(|| self.text(token))
        };
        let first = word(0).unwrap_or_default();
        if CONTROL.contains(&first.as_str()) {
            let within = Rc::new(within.nest(self.render(header)));
            return self.open.push(Block {
                global,
                within,
                def: None,
            });
        }
        let def = match (global, first.as_str()) {
            (true, "@mixin" | "@function") => word(1).map(|name| {
                let kind = if first == "@mixin" { MIXIN } else { FUNCTION };
                let params = self.params(&solid[2..]);
                Def {
                    kind,
                    name,
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
        for (name, variable) in std::mem::take(&mut self.variables) {
            let mut attrs = variable.first;
            if !variable.reassigned.is_empty() {
                attrs.insert("reassigned".into(), variable.reassigned.into());
            }
            self.table.insert(Symbol {
                kind: VARIABLE.into(),
                name,
                line: self.lines.line(variable.span.start as usize),
                span: variable.span,
                attrs,
            });
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
        ] {
            assert!(Scss.claims(path), "{path}");
        }
        for path in [
            "addons/point_of_sale/static/tests/unit/qunit_tests.scss",
            "addons/web/static/src/x.css",
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
