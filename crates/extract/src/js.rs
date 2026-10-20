//! JS modules, their exports, OWL components, legacy widgets, registry entries and patches from
//! the web client's `static/src` files, read with tree-sitter. Only code that runs when a module
//! loads declares something, so function bodies are skipped, `odoo.define` bodies aside. Names
//! and attrs hold references written `<module>#<member>`, where an empty or relative module is
//! the path's to complete.

mod hash;
mod path;
mod scope;
mod value;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;
use tree_sitter::Node;

use crate::bound::{Budget, bounded};
use crate::syntax::{self, Visit, Visitor, named, statements, text, visit};
use crate::{Extractor, Span, Symbol, SymbolTable};
use hash::fn_hash;
use path::{Header, qualify_ref};
use scope::{Binding, Resolved, Scope, Shadowed, method_call, require};
use value::{canonical, is_empty_object, key_text, method_key, object, single, string_value, tagless, value};

const MODULE: &str = "js_module";
const EXPORT: &str = "js_export";
const COMPONENT: &str = "js_component";
const WIDGET: &str = "js_widget";
const REGISTRY: &str = "js_registry";
const PATCH: &str = "js_patch";

/// The serialized symbols one file may give, per byte of it plus a constant: a backstop for
/// any path that would make the output grow faster than the input, past which the reader drops
/// the symbols it has yet to give and warns once.
const OUTPUT_PER_BYTE: usize = 32;
const OUTPUT_SLACK: usize = 1 << 20;

/// Duplicate warnings past this many are summed up in one, like syntax warnings.
const MAX_DUPLICATE_WARNINGS: usize = 10;

/// Roots whose `extend` or `include` merges plain objects, never a widget class.
const NOT_CLASSES: [&str; 5] = ["$", "_", "jQuery", "Object", "window"];

/// The statics an OWL component declares, kept apart from its other statics.
const COMPONENT_STATICS: [&str; 3] = ["template", "props", "components"];

pub struct Js;

impl Extractor for Js {
    fn name(&self) -> &'static str {
        "js"
    }

    /// Bump with any change to the output, including a tree-sitter or tree-sitter-javascript
    /// upgrade and changes to `syntax`.
    fn version(&self) -> u32 {
        1
    }

    fn claims(&self, path: &str) -> bool {
        path::claims(path)
    }

    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        let tree = syntax::parse(blob, &tree_sitter_javascript::LANGUAGE.into())?;
        let root = tree.root_node();
        let budget = Budget::new(blob, OUTPUT_PER_BYTE, OUTPUT_SLACK);
        let mut reader = Reader::new(blob, budget, syntax::syntax_warnings(root, |_| None));
        reader.program(root);
        Ok(reader.finish())
    }

    /// The path completes a reference: `@<addon>/<path>` for an empty or relative module. A
    /// registry entry or a patch fills a slot every addon shares, so it also takes `@<addon>`.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        let addon = path::addon(path);
        match symbol.kind.as_str() {
            REGISTRY => {
                let (category, key) = symbol.name.split_once(':').unwrap_or((&symbol.name, ""));
                format!("{}:{key}@{addon}", qualify_ref(path, category))
            }
            PATCH => format!("{}@{addon}", qualify_ref(path, &symbol.name)),
            _ => qualify_ref(path, &symbol.name).into_owned(),
        }
    }

    /// Every `{"ref": ...}` with an empty or relative module, completed like names.
    fn qualify_attrs<'a>(&self, path: &str, symbol: &'a Symbol) -> Cow<'a, BTreeMap<String, Value>> {
        let mut out: Option<BTreeMap<String, Value>> = None;
        for (key, value) in &symbol.attrs {
            if let Some(q) = qualified(path, value) {
                out.get_or_insert_with(|| symbol.attrs.clone()).insert(key.clone(), q);
            }
        }
        out.map_or(Cow::Borrowed(&symbol.attrs), Cow::Owned)
    }
}

/// `value` with its references qualified at `path`, or `None` when none changes.
fn qualified(path: &str, value: &Value) -> Option<Value> {
    match value {
        Value::Object(map) => {
            if let (1, Some(Value::String(r))) = (map.len(), map.get("ref")) {
                return match qualify_ref(path, r) {
                    Cow::Owned(q) => Some(single("ref", q)),
                    Cow::Borrowed(_) => None,
                };
            }
            let mut out = None;
            for (key, v) in map {
                if let Some(q) = qualified(path, v) {
                    out.get_or_insert_with(|| map.clone()).insert(key.clone(), q);
                }
            }
            out.map(Value::Object)
        }
        Value::Array(items) => {
            let changed: Vec<Option<Value>> = items.iter().map(|i| qualified(path, i)).collect();
            changed.iter().any(Option::is_some).then(|| {
                let items = items.iter().zip(changed).map(|(i, q)| q.unwrap_or_else(|| i.clone()));
                Value::Array(items.collect())
            })
        }
        _ => None,
    }
}

/// Nodes whose bodies run after the module loads, if ever.
fn is_function_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "function_expression"
            | "generator_function"
            | "generator_function_declaration"
            | "arrow_function"
            | "method_definition"
    )
}

/// The byte range and line of a name: a literal string's content without quotes, or the node.
fn name_span(node: Node) -> (Span, u32) {
    let mut cursor = node.walk();
    let literal = match node.kind() {
        "string" => true,
        "template_string" => !node.children(&mut cursor).any(|c| c.kind() == "template_substitution"),
        _ => false,
    };
    let (start, end) = match literal && node.byte_range().len() >= 2 {
        true => (node.start_byte() + 1, node.end_byte() - 1),
        false => (node.start_byte(), node.end_byte()),
    };
    let span = Span {
        start: start as u32,
        end: end as u32,
    };
    (span, node.start_position().row as u32 + 1)
}

fn symbol(kind: &str, name: String, at: Node, attrs: BTreeMap<String, Value>) -> Symbol {
    let (span, line) = name_span(at);
    Symbol {
        kind: kind.into(),
        name,
        line,
        span,
        attrs,
    }
}

/// The first child token of `node` of kind `kind`, such as `default` or `return`.
fn token<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| !c.is_named() && c.kind() == kind)
}

/// Whether `import X from "S"` reads as `const X = require("S")`, as Odoo's transpiler does
/// for a lone default import whose source starts with neither `@` nor `.`: a legacy module.
fn legacy_default(clause: Node, source: &str) -> bool {
    let parts = named(clause);
    matches!(parts[..], [only] if only.kind() == "identifier")
        && source.chars().next().is_some_and(|c| c != '@' && c != '.')
}

/// The leftmost name of a `.member` chain, such as `$` in `$.fn.extend`.
fn chain_root<'t>(mut node: Node<'t>) -> Node<'t> {
    while let Some(object) = node
        .child_by_field_name("object")
        .filter(|_| node.kind() == "member_expression")
    {
        node = object;
    }
    node
}

fn line_at(src: &[u8], offset: usize) -> u32 {
    src[..offset].iter().filter(|&&b| b == b'\n').count() as u32 + 1
}

/// The top-level declarations of one scope, for its exports and components.
#[derive(Default)]
struct Decls<'t> {
    by_name: BTreeMap<String, Decl<'t>>,
    classes: Vec<Class<'t>>,
}

enum Decl<'t> {
    Class(usize),
    Function(Node<'t>),
    /// `const`, `let` or `var`, and the value.
    Var(&'static str, Option<Node<'t>>),
}

struct Class<'t> {
    node: Node<'t>,
    name: Node<'t>,
    /// `Foo.<x> = ...` at scope level, the 14.0 to 16.0 way of setting statics.
    assigned: Vec<(String, Node<'t>)>,
}

/// Class entries by name: a field's value, `None` for a field without one, or a method.
type Entries<'t> = Vec<(String, Option<Node<'t>>)>;

struct Reader<'t> {
    src: &'t [u8],
    table: SymbolTable,
    /// The last entry of each registry key: a legacy registry and a forced ES `add` replace an
    /// earlier one, and an unforced ES `add` of a taken key throws. Past the output budget, an
    /// `add` of a key already read keeps the earlier entry.
    registry: BTreeMap<String, Symbol>,
    /// The first export of each declaration, by reference, so that later exports of it point
    /// there instead of repeating it: output stays linear in the input.
    exports: BTreeMap<String, String>,
    /// The scope declarations already read as a service, by reference, for the same reason.
    services: BTreeSet<String>,
    /// Spent in the order the reader gives symbols.
    budget: Budget,
    /// How many duplicates were dropped, and the offset of the first one past those that warn.
    duplicates: usize,
    more_duplicates_at: Option<usize>,
    /// How often each target was patched so far.
    patches: BTreeMap<String, usize>,
    /// By byte offset, so that they end up in document order.
    warnings: Vec<(usize, String)>,
}

impl<'t> Reader<'t> {
    fn new(src: &'t [u8], budget: Budget, warnings: Vec<(usize, String)>) -> Self {
        Reader {
            src,
            table: SymbolTable::default(),
            registry: BTreeMap::new(),
            patches: BTreeMap::new(),
            exports: BTreeMap::new(),
            services: BTreeSet::new(),
            budget,
            duplicates: 0,
            more_duplicates_at: None,
            warnings,
        }
    }

    fn text(&self, node: Node) -> Cow<'t, str> {
        text(node, self.src)
    }

    fn value(&self, node: Node, scope: &Scope) -> Value {
        value(node, self.src, scope)
    }

    /// `{"ref"}` when `node` resolves, else `{"expr"}`.
    fn reference(&self, node: Node, scope: &Scope) -> Value {
        match scope.resolve(node, self.src) {
            Some(Resolved::Ref(r)) => single("ref", r),
            Some(Resolved::Expr(e)) => single("expr", e),
            None => single("expr", canonical(node, self.src)),
        }
    }

    /// The reference `node` resolves to, or its canonical text, bounded.
    fn target(&self, node: Node, scope: &Scope) -> String {
        match scope.resolve(node, self.src) {
            Some(Resolved::Ref(r) | Resolved::Expr(r)) => r,
            None => bounded(canonical(node, self.src)),
        }
    }

    /// Keeps the first of a `(kind, name)` and warns about a later one.
    fn insert(&mut self, symbol: Symbol) {
        if self.table.contains(&symbol.kind, &symbol.name) {
            self.duplicates += 1;
            if self.duplicates <= MAX_DUPLICATE_WARNINGS {
                let warning = format!(
                    "line {}: duplicate {} {} dropped",
                    symbol.line, symbol.kind, symbol.name
                );
                self.warnings.push((symbol.span.start as usize, warning));
            } else {
                self.more_duplicates_at.get_or_insert(symbol.span.start as usize);
            }
        } else if self.budget.symbol(&symbol) {
            self.table.insert(symbol);
        }
    }

    fn program(&mut self, root: Node<'t>) {
        let statements = statements(root);
        let mut scope = Scope::new(String::new());
        // First, so that the module symbol is the last one the output budget drops.
        self.es_module(path::header(self.src), &statements);
        let mut decls = self.bind(&mut scope, &statements);
        self.walk(root, &mut scope, &mut decls, true);
        let components = self.components(&scope, &decls);
        self.es_exports(&scope, &decls, &components, &statements);
    }

    /// Binds the imports, requires and declarations of a scope's statements.
    fn bind(&self, scope: &mut Scope, statements: &[Node<'t>]) -> Decls<'t> {
        let mut decls = Decls::default();
        for &statement in statements {
            match statement.kind() {
                "import_statement" => self.import(scope, statement),
                "export_statement" => {
                    if let Some(declaration) = statement.child_by_field_name("declaration") {
                        self.declare(scope, &mut decls, declaration);
                    }
                }
                _ => self.declare(scope, &mut decls, statement),
            }
        }
        decls
    }

    fn import(&self, scope: &mut Scope, statement: Node<'t>) {
        let Some(source) = self.source(statement) else {
            return;
        };
        let clause = named(statement).into_iter().find(|n| n.kind() == "import_clause");
        let legacy = clause.is_some_and(|c| legacy_default(c, &source));
        for part in clause.map(named).unwrap_or_default() {
            let member = |path: &str| Binding::Member {
                module: source.clone(),
                path: path.into(),
            };
            match part.kind() {
                "identifier" if legacy => {
                    let module = Binding::Module {
                        module: source.clone(),
                        namespace: false,
                    };
                    scope.bind(self.text(part), module);
                }
                "identifier" => scope.bind(self.text(part), member("default")),
                "namespace_import" => {
                    for name in named(part) {
                        let module = Binding::Module {
                            module: source.clone(),
                            namespace: true,
                        };
                        scope.bind(self.text(name), module);
                    }
                }
                "named_imports" => {
                    for specifier in named(part) {
                        let Some(name) = specifier.child_by_field_name("name") else {
                            continue;
                        };
                        let local = specifier.child_by_field_name("alias").unwrap_or(name);
                        scope.bind(self.text(local), member(&bounded(key_text(name, self.src))));
                    }
                }
                _ => {}
            }
        }
    }

    /// The module an import or export reads from, `/index` dropped as Odoo does.
    fn source(&self, statement: Node) -> Option<String> {
        let source = string_value(statement.child_by_field_name("source")?, self.src)?;
        Some(bounded(path::remove_index(&source).to_string()))
    }

    fn declare(&self, scope: &mut Scope, decls: &mut Decls<'t>, node: Node<'t>) {
        let name = node.child_by_field_name("name").filter(|n| n.kind() == "identifier");
        match node.kind() {
            "class_declaration" => {
                if let Some(name) = name {
                    scope.bind(self.text(name), Binding::Local);
                    let index = decls.classes.len();
                    decls.by_name.insert(self.text(name).into(), Decl::Class(index));
                    decls.classes.push(Class {
                        node,
                        name,
                        assigned: Vec::new(),
                    });
                }
            }
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = name {
                    scope.bind(self.text(name), Binding::Local);
                    decls.by_name.insert(self.text(name).into(), Decl::Function(node));
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                let kind = match node.child_by_field_name("kind").map(|k| k.kind()) {
                    Some("const") => "const",
                    Some("let") => "let",
                    _ => "var",
                };
                for declarator in named(node).into_iter().filter(|d| d.kind() == "variable_declarator") {
                    self.declarator_binding(scope, decls, kind, declarator);
                }
            }
            _ => {}
        }
    }

    /// A require, an alias of an imported or required member, a registry category, names
    /// destructured from one of those or from a global, or a local.
    fn declarator_binding(&self, scope: &mut Scope, decls: &mut Decls<'t>, kind: &'static str, declarator: Node<'t>) {
        let (Some(target), value) = (
            declarator.child_by_field_name("name"),
            declarator.child_by_field_name("value"),
        ) else {
            return;
        };
        let src = self.src;
        let alias = value.and_then(|v| scope.alias(v, src));
        match target.kind() {
            "identifier" => {
                let name = self.text(target).into_owned();
                let category = value.and_then(|v| scope.category(v, src, |arg| self.label(arg)));
                let binding = match (alias, category) {
                    (Some(binding), _) => binding,
                    (None, Some(path)) => Binding::Category(path),
                    (None, None) => Binding::Local,
                };
                scope.bind(name.clone(), binding);
                decls.by_name.insert(name, Decl::Var(kind, value));
            }
            "object_pattern" => {
                let Some(value) = value else {
                    return;
                };
                // `chain` bounds the global's text, so each name holds a short copy of it.
                let global = (alias.is_none() && scope.is_global(value, src)).then(|| canonical(value, src));
                for (key, local) in self.destructured(target) {
                    let binding = match (&alias, &global) {
                        (Some(base), _) => scope::member_of(base, &key),
                        (None, Some(global)) => Some(Binding::Global(bounded(format!("{global}.{key}")))),
                        (None, None) => None,
                    };
                    scope.bind(local, binding.unwrap_or(Binding::Local));
                }
            }
            _ => {}
        }
    }

    /// `(key, local name)` per name an object pattern binds, nested patterns and rests aside.
    fn destructured(&self, pattern: Node) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for entry in named(pattern) {
            match entry.kind() {
                "shorthand_property_identifier_pattern" => {
                    let name = self.text(entry).into_owned();
                    out.push((bounded(name.clone()), name));
                }
                "object_assignment_pattern" => {
                    if let Some(left) = entry.child_by_field_name("left") {
                        let name = self.text(left).into_owned();
                        out.push((bounded(name.clone()), name));
                    }
                }
                "pair_pattern" => {
                    let (Some(key), Some(mut local)) =
                        (entry.child_by_field_name("key"), entry.child_by_field_name("value"))
                    else {
                        continue;
                    };
                    if local.kind() == "assignment_pattern" {
                        local = local.child_by_field_name("left").unwrap_or(local);
                    }
                    if local.kind() == "identifier" {
                        out.push((bounded(key_text(key, self.src)), self.text(local).into_owned()));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// A category name or a key: the decoded string, or `[<canonical>]`, bounded.
    fn label(&self, node: Node) -> String {
        bounded(string_value(node, self.src).unwrap_or_else(|| format!("[{}]", canonical(node, self.src))))
    }

    /// Reads what the code of a scope that runs at load declares: defines (`program` only),
    /// registry entries, patches, widgets and statics assigned to the scope's classes.
    fn walk(&mut self, root: Node<'t>, scope: &mut Scope, decls: &mut Decls<'t>, program: bool) {
        let mut walker = Walker {
            reader: self,
            scope,
            decls,
            program,
            root,
            blocks: Vec::new(),
        };
        visit(root, &mut walker);
    }

    /// `odoo.define(name, [deps], function (require) {...})`: a module, and its body a scope.
    fn define(&mut self, call: Node<'t>) -> bool {
        let Some((object, args)) = method_call(call, "define", self.src) else {
            return false;
        };
        let Some(body) = args
            .last()
            .filter(|f| matches!(f.kind(), "function_expression" | "arrow_function"))
            .and_then(|f| f.child_by_field_name("body"))
            .filter(|b| b.kind() == "statement_block")
        else {
            return false;
        };
        if object.kind() != "identifier" || self.text(object) != "odoo" {
            return false;
        }
        let Some(name) = string_value(args[0], self.src) else {
            let line = call.start_position().row + 1;
            let warning = format!("line {line}: odoo.define without a literal name; ignored");
            self.warnings.push((call.start_byte(), warning));
            return true;
        };
        let mut requires = BTreeSet::new();
        visit(body, &mut |v: &Visit| {
            if v.node.kind() == "call_expression" {
                requires.extend(require(v.node, self.src));
            }
            true
        });
        let mut attrs = BTreeMap::from([("declared".to_string(), Value::from("define"))]);
        attrs.insert("requires".into(), Value::from_iter(requires));
        // Every local of the define repeats its name.
        let name = bounded(name);
        self.insert(symbol(MODULE, name.clone(), args[0], attrs));

        let statements = statements(body);
        let mut scope = Scope::new(name);
        let mut decls = self.bind(&mut scope, &statements);
        self.walk(body, &mut scope, &mut decls, false);
        let components = self.components(&scope, &decls);
        if let Some(&returned) = statements.iter().find(|s| s.kind() == "return_statement") {
            self.define_exports(returned, &scope, &decls, &components);
        }
        true
    }

    /// `<receiver>.add(key, value[, options])`, chained or not, on a registry: an ES category,
    /// or a legacy registry, any resolved reference whose last segment holds `registry`.
    fn registry_adds(&mut self, v: &Visit<'t>, scope: &Scope, decls: &Decls<'t>) -> bool {
        // The outermost `.add` of a chain reads the whole chain.
        let chained = v.fills("object")
            && v.parent
                .and_then(|p| p.child_by_field_name("property"))
                .is_some_and(|p| self.text(p) == "add");
        if chained {
            return false;
        }
        let mut adds = Vec::new();
        let mut receiver = v.node;
        while let Some((object, args)) = method_call(receiver, "add", self.src) {
            adds.push(args);
            receiver = object;
        }
        if adds.is_empty() {
            return false;
        }
        let Some((category, services)) = self.category(receiver, scope) else {
            return false;
        };
        for args in adds.into_iter().rev() {
            self.registry_entry(&category, services, &args, scope, decls);
        }
        true
    }

    /// The category name of a registry receiver, and whether it holds services.
    fn category(&self, receiver: Node, scope: &Scope) -> Option<(String, bool)> {
        if let Some(path) = scope.category(receiver, self.src, |arg| self.label(arg)) {
            let services = path == ["services"];
            return Some((bounded(path.join(">")), services));
        }
        let Some(Resolved::Ref(legacy)) = scope.resolve(receiver, self.src) else {
            return None;
        };
        let last = legacy.rsplit(['#', '.', '/']).next().unwrap_or_default();
        // The root ES registry takes entries through its categories only.
        let root = receiver.kind() == "identifier" && legacy.ends_with("#registry");
        let named = last.to_ascii_lowercase().contains("registry") && !root;
        named.then(|| (bounded(legacy.clone()), last == "serviceRegistry"))
    }

    fn registry_entry(&mut self, category: &str, services: bool, args: &[Node<'t>], scope: &Scope, decls: &Decls<'t>) {
        let Some((&key, rest)) = args.split_first() else {
            return;
        };
        let mut attrs = BTreeMap::new();
        if let Some(&entry) = rest.first() {
            attrs.insert("value".to_string(), self.value(entry, scope));
            if services {
                for (k, v) in self.service(entry, scope, decls) {
                    attrs.insert(k, v);
                }
            }
        }
        match rest.get(1) {
            Some(&options) if options.kind() == "object" => {
                let mut options = object(options, self.src, scope, 0);
                for promoted in ["sequence", "force"] {
                    if let Some(v) = options.remove(promoted) {
                        attrs.insert(promoted.into(), v);
                    }
                }
                if !options.is_empty() {
                    attrs.insert("options".into(), tagless(options));
                }
            }
            // A legacy registry's third argument is the sequence.
            Some(&options) if options.kind() == "number" => {
                attrs.insert("sequence".into(), self.value(options, scope));
            }
            Some(&options) => {
                attrs.insert("options".into(), self.value(options, scope));
            }
            None => {}
        }
        let name = format!("{category}:{}", self.label(key));
        let entry = symbol(REGISTRY, name.clone(), key, attrs);
        if self.budget.symbol(&entry) {
            self.registry.insert(name, entry);
        }
    }

    /// The `dependencies` and `async` of a service: an inline object, a scope `const` object or
    /// the object a scope widget extends with. A declaration added again gives nothing more, its
    /// entry's `value` naming it.
    fn service(&mut self, entry: Node<'t>, scope: &Scope, decls: &Decls<'t>) -> Vec<(String, Value)> {
        let declared = match decls.by_name.get(self.text(entry).as_ref()) {
            Some(Decl::Var(_, Some(v))) if entry.kind() == "identifier" => {
                if !self.services.insert(scope.local(&self.text(entry))) {
                    return Vec::new();
                }
                *v
            }
            _ => entry,
        };
        let object = match self.extend(declared) {
            Some((_, args)) => args.last().copied(),
            None => Some(declared).filter(|o| o.kind() == "object"),
        };
        let mut out = Vec::new();
        for pair in object.map(named).unwrap_or_default() {
            let (Some(key), Some(v)) = (pair.child_by_field_name("key"), pair.child_by_field_name("value")) else {
                continue;
            };
            let key = key_text(key, self.src);
            if matches!(key.as_str(), "dependencies" | "async") && pair.kind() == "pair" {
                out.push((key, self.value(v, scope)));
            }
        }
        out
    }

    /// `patch(target, [name,] {...})`, where `patch` is any reference ending in `#patch`, an
    /// import or a local function, or the unbound global.
    fn patch(&mut self, call: Node<'t>, scope: &Scope) -> bool {
        let Some(callee) = call.child_by_field_name("function") else {
            return false;
        };
        let is_patch = match scope.resolve(callee, self.src) {
            Some(Resolved::Ref(r)) => r.ends_with("#patch"),
            Some(Resolved::Expr(_)) => false,
            None => callee.kind() == "identifier" && self.text(callee) == "patch" && scope.get("patch").is_none(),
        };
        let args = call.child_by_field_name("arguments").map(named).unwrap_or_default();
        let (target, object) = match (is_patch, &args[..]) {
            (true, [target, object]) | (true, [target, _, object]) => (*target, *object),
            _ => return false,
        };
        let mut attrs = BTreeMap::from([("declared".to_string(), Value::from("patch"))]);
        if let [_, name, _] = args[..] {
            attrs.insert("name".into(), self.value(name, scope));
        }
        match object.kind() {
            "object" => attrs.insert("members".into(), self.value(object, scope)),
            _ => attrs.insert("value".into(), self.value(object, scope)),
        };
        self.insert_patch(target, scope, attrs);
        true
    }

    /// `<target>.include({...})`, the legacy class patch.
    fn include(&mut self, call: Node<'t>, scope: &Scope) -> bool {
        let Some((target, args)) = method_call(call, "include", self.src) else {
            return false;
        };
        let root = chain_root(target);
        if args.len() != 1 || args[0].kind() != "object" || NOT_CLASSES.contains(&self.text(root).as_ref()) {
            return false;
        }
        let attrs = BTreeMap::from([
            ("declared".to_string(), Value::from("include")),
            ("members".to_string(), self.value(args[0], scope)),
        ]);
        self.insert_patch(target, scope, attrs);
        true
    }

    /// A second patch of one target in the file is named `<target>[2]`, and so on.
    fn insert_patch(&mut self, target: Node<'t>, scope: &Scope, attrs: BTreeMap<String, Value>) {
        let name = self.target(target, scope);
        let count = self.patches.entry(name.clone()).or_default();
        *count += 1;
        let name = match *count {
            1 => name,
            n => format!("{name}[{n}]"),
        };
        self.insert(symbol(PATCH, name, target, attrs));
    }

    /// `Foo.<x> = ...` on a scope class, or `<target> = X.extend(...)`.
    fn assignment(&mut self, node: Node<'t>, scope: &Scope, decls: &mut Decls<'t>) -> bool {
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
            return false;
        };
        let (Some(object), Some(property)) = (left.child_by_field_name("object"), left.child_by_field_name("property"))
        else {
            return false;
        };
        if left.kind() != "member_expression" {
            return false;
        }
        if object.kind() == "identifier"
            && let Some(Decl::Class(i)) = decls.by_name.get(self.text(object).as_ref())
            && scope.get(&self.text(object)) == Some(&Binding::Local)
        {
            decls.classes[*i]
                .assigned
                .push((self.text(property).into_owned(), right));
            return true;
        }
        let Some((parent, args)) = self.extend(right) else {
            return false;
        };
        let name = self.target(left, scope);
        self.widget(name, property, parent, &args, scope);
        true
    }

    /// `const X = Parent.extend(...)`.
    fn declarator(&mut self, node: Node<'t>, scope: &Scope) -> bool {
        let (Some(name), Some(value)) = (node.child_by_field_name("name"), node.child_by_field_name("value")) else {
            return false;
        };
        let Some((parent, args)) = self.extend(value).filter(|_| name.kind() == "identifier") else {
            return false;
        };
        self.widget(scope.local(&self.text(name)), name, parent, &args, scope);
        true
    }

    /// The parent and arguments of `Parent.extend([mixins...,] {...})`, the legacy class
    /// system's subclassing, and not an object merge such as `$.extend` or `_.extend({}, ...)`.
    fn extend(&self, call: Node<'t>) -> Option<(Node<'t>, Vec<Node<'t>>)> {
        let (parent, args) = method_call(call, "extend", self.src)?;
        let root = chain_root(parent);
        let literal = args.last().is_some_and(|a| a.kind() == "object");
        let merge = NOT_CLASSES.contains(&self.text(root).as_ref()) || is_empty_object(*args.first()?);
        (literal && !merge).then_some((parent, args))
    }

    fn widget(&mut self, name: String, at: Node<'t>, parent: Node<'t>, args: &[Node<'t>], scope: &Scope) {
        let mut attrs = BTreeMap::from([("extends".to_string(), self.reference(parent, scope))]);
        let (object, mixins) = args.split_last().expect("extend has an object argument");
        if !mixins.is_empty() {
            let mixins = mixins.iter().map(|&m| self.reference(m, scope));
            attrs.insert("mixins".into(), Value::from_iter(mixins));
        }
        let mut members = value::object(*object, self.src, scope, 0);
        for key in ["template", "events", "custom_events"] {
            if let Some(v) = members.remove(key) {
                attrs.insert(key.into(), v);
            }
        }
        if !members.is_empty() {
            attrs.insert("members".into(), tagless(members));
        }
        self.insert(symbol(WIDGET, name, at, attrs));
    }

    /// The members and statics of a class body, private ones aside: a field's value or `null`,
    /// a method's hash.
    fn class_members(&self, class: &Class<'t>) -> (Entries<'t>, Entries<'t>) {
        let (mut members, mut statics) = (Vec::new(), Vec::new());
        let body = class.node.child_by_field_name("body").map(named).unwrap_or_default();
        for member in body {
            let (key, value) = match member.kind() {
                "method_definition" => (method_key(member, self.src), Some(member)),
                "field_definition" => match member.child_by_field_name("property") {
                    Some(p) => (key_text(p, self.src), member.child_by_field_name("value")),
                    None => continue,
                },
                _ => continue,
            };
            if key.starts_with('#') {
                continue;
            }
            match token(member, "static") {
                Some(_) => statics.push((key, value)),
                None => members.push((key, value)),
            }
        }
        statics.extend(class.assigned.iter().map(|(k, v)| (k.clone(), Some(*v))));
        (members, statics)
    }

    /// A class's heritage expression.
    fn extends(&self, class: Node<'t>) -> Option<Node<'t>> {
        let mut cursor = class.walk();
        let heritage = class.named_children(&mut cursor).find(|c| c.kind() == "class_heritage");
        heritage.and_then(|h| named(h).first().copied())
    }

    /// Emits the scope's OWL components, and returns their local names: classes that extend
    /// `Component` or a component of the scope, or declare a template, props or components.
    fn components(&mut self, scope: &Scope, decls: &Decls<'t>) -> BTreeSet<String> {
        let mut components = BTreeSet::new();
        for class in &decls.classes {
            let name = self.text(class.name).into_owned();
            let extends = self.extends(class.node);
            let (_, statics) = self.class_members(class);
            let by_parent = extends.is_some_and(|e| match scope.resolve(e, self.src) {
                Some(Resolved::Ref(r)) => {
                    r.ends_with("#Component")
                        || (e.kind() == "identifier" && components.contains(self.text(e).as_ref()))
                }
                Some(Resolved::Expr(x)) => x == "owl.Component",
                None => canonical(e, self.src) == "owl.Component",
            });
            let by_statics = statics.iter().any(|(k, _)| COMPONENT_STATICS.contains(&k.as_str()));
            if !(by_parent || by_statics) {
                continue;
            }
            let mut attrs = BTreeMap::new();
            if let Some(e) = extends {
                attrs.insert("extends".to_string(), self.reference(e, scope));
            }
            let mut other = BTreeMap::new();
            for (key, node) in statics {
                let v = match (key.as_str(), node) {
                    (_, None) => Value::Null,
                    ("template", Some(n)) => self.template(n, scope),
                    ("props", Some(n)) => self.props(n, scope),
                    (_, Some(n)) => self.value(n, scope),
                };
                match COMPONENT_STATICS.contains(&key.as_str()) {
                    true => attrs.insert(key, v),
                    false => other.insert(key, v),
                };
            }
            if !other.is_empty() {
                attrs.insert("static".into(), tagless(other));
            }
            self.insert(symbol(COMPONENT, scope.local(&name), class.name, attrs));
            components.insert(name);
        }
        components
    }

    /// A template name, or `{"inline": <hash>}` for an `xml` tagged template, whose whitespace
    /// runs do not count.
    fn template(&self, node: Node, scope: &Scope) -> Value {
        let tagged = node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .is_some_and(|f| self.text(f) == "xml")
            && node
                .child_by_field_name("arguments")
                .is_some_and(|a| a.kind() == "template_string");
        if !tagged {
            return self.value(node, scope);
        }
        let mut hasher = blake3::Hasher::new();
        let template = node.child_by_field_name("arguments").expect("a tagged template");
        for word in self.text(template).split_whitespace() {
            hasher.update(word.as_bytes());
            hasher.update(&[0]);
        }
        single("inline", crate::hash::digest(&hasher))
    }

    /// Props as an array come sorted, since their order means nothing to OWL.
    fn props(&self, node: Node, scope: &Scope) -> Value {
        match self.value(node, scope) {
            Value::Array(mut items) => {
                items.sort_by_cached_key(|i| i.to_string());
                Value::Array(items)
            }
            v => v,
        }
    }

    /// Entries as one object, the last of a name winning as in JS.
    fn members(&self, entries: Entries<'t>, scope: &Scope) -> Value {
        let entries = entries
            .into_iter()
            .map(|(k, v)| (k, v.map_or(Value::Null, |v| self.value(v, scope))));
        tagless(entries.collect())
    }

    fn class_attrs(&self, class: &Class<'t>, scope: &Scope, component: bool) -> BTreeMap<String, Value> {
        let mut attrs = BTreeMap::from([("kind".to_string(), Value::from("class"))]);
        if let Some(e) = self.extends(class.node) {
            attrs.insert("extends".into(), self.reference(e, scope));
        }
        let (members, statics) = self.class_members(class);
        if !members.is_empty() {
            attrs.insert("members".into(), self.members(members, scope));
        }
        if !component && !statics.is_empty() {
            attrs.insert("static".into(), self.members(statics, scope));
        }
        attrs
    }

    fn function_attrs(&self, function: Node) -> BTreeMap<String, Value> {
        let params = function
            .child_by_field_name("parameters")
            .map(named)
            .unwrap_or_default();
        let params = params.into_iter().map(|p| Value::from(canonical(p, self.src)));
        let mut attrs = BTreeMap::from([
            ("kind".to_string(), Value::from("function")),
            ("params".to_string(), Value::from_iter(params)),
            ("body".to_string(), Value::from(fn_hash(function, self.src))),
        ]);
        if token(function, "async").is_some() {
            attrs.insert("async".into(), true.into());
        }
        if function.kind().starts_with("generator") || token(function, "*").is_some() {
            attrs.insert("generator".into(), true.into());
        }
        attrs
    }

    /// The attrs of an export of the scope's `name`, read at `at`: its declaration's, or a
    /// `value` for a name the scope does not declare.
    fn exported(
        &self,
        name: &str,
        at: Node,
        scope: &Scope,
        decls: &Decls<'t>,
        components: &BTreeSet<String>,
    ) -> BTreeMap<String, Value> {
        match decls.by_name.get(name) {
            Some(Decl::Class(i)) => self.class_attrs(&decls.classes[*i], scope, components.contains(name)),
            Some(Decl::Function(f)) => self.function_attrs(*f),
            Some(Decl::Var(_, Some(v))) if self.extend(*v).is_some() => {
                BTreeMap::from([("kind".to_string(), Value::from("widget"))])
            }
            Some(Decl::Var(kind, v)) => {
                let mut attrs = BTreeMap::from([("kind".to_string(), Value::from(*kind))]);
                if let Some(v) = v {
                    attrs.insert("value".into(), self.value(*v, scope));
                }
                attrs
            }
            None => BTreeMap::from([
                ("kind".to_string(), Value::from("value")),
                ("value".to_string(), self.value(at, scope)),
            ]),
        }
    }

    /// The `kind` an export of the scope's `name` takes.
    fn kind_of(&self, name: &str, decls: &Decls<'t>) -> &'static str {
        match decls.by_name.get(name) {
            Some(Decl::Class(_)) => "class",
            Some(Decl::Function(_)) => "function",
            Some(Decl::Var(_, Some(v))) if self.extend(*v).is_some() => "widget",
            Some(Decl::Var(kind, _)) => kind,
            None => "value",
        }
    }

    /// `exported`, with `local` when the export renames the declaration. A declaration exported
    /// again gets its kind and `of`, a reference to its first export.
    fn export(
        &mut self,
        exported: &str,
        local: Node,
        at: Node,
        scope: &Scope,
        decls: &Decls<'t>,
        components: &BTreeSet<String>,
    ) {
        let name = self.text(local).into_owned();
        let declaration = scope.local(&name);
        let mut attrs = match self.exports.get(&declaration) {
            Some(first) => BTreeMap::from([
                ("kind".to_string(), Value::from(self.kind_of(&name, decls))),
                ("of".to_string(), single("ref", scope.local(first))),
            ]),
            None => {
                self.exports.insert(declaration, bounded(exported.to_string()));
                self.exported(&name, local, scope, decls, components)
            }
        };
        if name != exported {
            attrs.insert("local".into(), name.into());
        }
        self.insert(symbol(EXPORT, scope.local(exported), at, attrs));
    }

    fn es_exports(&mut self, scope: &Scope, decls: &Decls<'t>, components: &BTreeSet<String>, statements: &[Node<'t>]) {
        let mut star: Option<(Node<'t>, Vec<Value>)> = None;
        for &statement in statements.iter().filter(|s| s.kind() == "export_statement") {
            let default = token(statement, "default");
            let source = self.source(statement);
            if let Some(declaration) = statement.child_by_field_name("declaration") {
                let names: Vec<Node> = match declaration.kind() {
                    "lexical_declaration" | "variable_declaration" => named(declaration)
                        .into_iter()
                        .filter_map(|d| d.child_by_field_name("name"))
                        .filter(|n| n.kind() == "identifier")
                        .collect(),
                    _ => declaration.child_by_field_name("name").into_iter().collect(),
                };
                for name in names {
                    match default {
                        Some(at) => self.export("default", name, at, scope, decls, components),
                        None => self.export(&self.text(name), name, name, scope, decls, components),
                    }
                }
            } else if let Some(v) = statement.child_by_field_name("value") {
                let at = default.unwrap_or(v);
                let attrs = match v.kind() {
                    "identifier" => {
                        self.export("default", v, at, scope, decls, components);
                        continue;
                    }
                    _ if is_function_kind(v.kind()) => self.function_attrs(v),
                    _ => BTreeMap::from([
                        ("kind".to_string(), Value::from("value")),
                        ("value".to_string(), self.value(v, scope)),
                    ]),
                };
                self.insert(symbol(EXPORT, scope.local("default"), at, attrs));
            } else if let Some(source) = source {
                for part in named(statement) {
                    match part.kind() {
                        "export_clause" => {
                            for specifier in named(part) {
                                let Some(name) = specifier.child_by_field_name("name") else {
                                    continue;
                                };
                                let at = specifier.child_by_field_name("alias").unwrap_or(name);
                                let of = format!("{source}#{}", bounded(key_text(name, self.src)));
                                self.reexport(&bounded(key_text(at, self.src)), at, single("ref", of), scope);
                            }
                        }
                        "namespace_export" => {
                            if let Some(&at) = named(part).first() {
                                let of = single("ref", format!("{source}#*"));
                                self.reexport(&bounded(key_text(at, self.src)), at, of, scope);
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(at) =
                    token(statement, "*").filter(|_| !named(statement).iter().any(|p| p.kind() == "namespace_export"))
                {
                    let (_, of) = star.get_or_insert_with(|| (at, Vec::new()));
                    of.push(single("ref", format!("{source}#*")));
                }
            } else {
                for clause in named(statement).into_iter().filter(|p| p.kind() == "export_clause") {
                    for specifier in named(clause) {
                        let Some(name) = specifier.child_by_field_name("name") else {
                            continue;
                        };
                        let at = specifier.child_by_field_name("alias").unwrap_or(name);
                        self.export(&bounded(key_text(at, self.src)), name, at, scope, decls, components);
                    }
                }
            }
        }
        if let Some((at, mut of)) = star {
            of.sort_by_cached_key(|r| r.to_string());
            of.dedup();
            self.reexport("*", at, Value::Array(of), scope);
        }
    }

    fn reexport(&mut self, exported: &str, at: Node, of: Value, scope: &Scope) {
        let attrs = BTreeMap::from([("kind".to_string(), Value::from("reexport")), ("of".to_string(), of)]);
        self.insert(symbol(EXPORT, scope.local(exported), at, attrs));
    }

    /// A define body's `return`: an object's keys, or the default export.
    fn define_exports(&mut self, returned: Node<'t>, scope: &Scope, decls: &Decls<'t>, components: &BTreeSet<String>) {
        let Some(&value) = named(returned).first() else {
            return;
        };
        let at = token(returned, "return").unwrap_or(returned);
        match value.kind() {
            "identifier" => self.export("default", value, at, scope, decls, components),
            "object" => {
                for entry in named(value) {
                    let (key, v) = match entry.kind() {
                        "pair" => match (entry.child_by_field_name("key"), entry.child_by_field_name("value")) {
                            (Some(k), Some(v)) => (k, v),
                            _ => continue,
                        },
                        "shorthand_property_identifier" => (entry, entry),
                        _ => continue,
                    };
                    let exported = key_text(key, self.src);
                    if matches!(v.kind(), "identifier" | "shorthand_property_identifier") {
                        self.export(&exported, v, key, scope, decls, components);
                    } else {
                        let attrs = BTreeMap::from([
                            ("kind".to_string(), Value::from("value")),
                            ("value".to_string(), self.value(v, scope)),
                        ]);
                        self.insert(symbol(EXPORT, scope.local(&exported), key, attrs));
                    }
                }
            }
            _ => {
                let attrs = BTreeMap::from([
                    ("kind".to_string(), Value::from("value")),
                    ("value".to_string(), self.value(value, scope)),
                ]);
                self.insert(symbol(EXPORT, scope.local("default"), at, attrs));
            }
        }
    }

    /// The ES module, named by its path, and the alias its header declares. A file is one when
    /// its header says so or it imports or exports: from saas-17.2 Odoo needs no header.
    fn es_module(&mut self, header: Option<Header>, statements: &[Node<'t>]) {
        let header = header.filter(|h| !h.ignore);
        let mut imports = BTreeSet::new();
        let mut es = header.is_some();
        for &statement in statements {
            match statement.kind() {
                "import_statement" => {
                    es = true;
                    let Some(source) = self.source(statement) else {
                        continue;
                    };
                    let clause = named(statement).into_iter().find(|n| n.kind() == "import_clause");
                    let Some(clause) = clause.filter(|&c| !legacy_default(c, &source)) else {
                        imports.insert(source);
                        continue;
                    };
                    for part in named(clause) {
                        match part.kind() {
                            "identifier" => {
                                imports.insert(format!("{source}#default"));
                            }
                            "namespace_import" => {
                                imports.insert(format!("{source}#*"));
                            }
                            "named_imports" => {
                                for name in named(part).into_iter().filter_map(|s| s.child_by_field_name("name")) {
                                    imports.insert(format!("{source}#{}", bounded(key_text(name, self.src))));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "export_statement" => es = true,
                _ => {}
            }
        }
        if !es {
            return;
        }
        let mut attrs = BTreeMap::new();
        if header.is_some() {
            attrs.insert("header".to_string(), Value::Bool(true));
        }
        if !imports.is_empty() {
            attrs.insert(
                "imports".into(),
                imports.into_iter().map(|i| single("ref", i)).collect(),
            );
        }
        let at = header.as_ref().map_or(0..0, |h| h.at.clone());
        let line = line_at(self.src, at.start);
        self.insert(Symbol {
            kind: MODULE.into(),
            name: String::new(),
            line,
            span: Span {
                start: at.start as u32,
                end: at.end as u32,
            },
            attrs,
        });
        let Some(alias) = header.and_then(|h| h.alias.map(|a| (a, h.default))) else {
            return;
        };
        let (at, default) = alias;
        // Any `default=` makes the alias the whole module, as 18.0 reads it; 17.0 accepts only
        // `false`, `False` and `0` there, which read the same.
        let whole = default.is_some();
        let attrs = BTreeMap::from([
            ("declared".to_string(), Value::from("alias")),
            ("of".to_string(), single("ref", if whole { "#*" } else { "#default" })),
        ]);
        self.insert(Symbol {
            kind: MODULE.into(),
            name: bounded(String::from_utf8_lossy(&self.src[at.clone()]).into_owned()),
            line: line_at(self.src, at.start),
            span: Span {
                start: at.start as u32,
                end: at.end as u32,
            },
            attrs,
        });
    }

    fn finish(mut self) -> SymbolTable {
        for symbol in std::mem::take(&mut self.registry).into_values() {
            self.table.insert(symbol);
        }
        if let Some(at) = self.more_duplicates_at {
            let more = self.duplicates - MAX_DUPLICATE_WARNINGS;
            self.warnings.push((at, format!("{more} more duplicates")));
        }
        self.warnings.extend(self.budget.warning("symbols"));
        self.warnings.sort_by_key(|(at, _)| *at);
        for (_, warning) in self.warnings {
            self.table.warn(warning);
        }
        self.table
    }
}

/// The walk of one scope. The names an inner block, loop or `catch` declares shadow the scope's
/// while the walk is inside it.
struct Walker<'w, 't> {
    reader: &'w mut Reader<'t>,
    scope: &'w mut Scope,
    decls: &'w mut Decls<'t>,
    program: bool,
    root: Node<'t>,
    blocks: Vec<(Node<'t>, Shadowed)>,
}

impl<'t> Visitor<'t> for Walker<'_, 't> {
    fn enter(&mut self, v: &Visit<'t>) -> bool {
        let n = v.node;
        if n.is_extra() && !n.is_error() {
            return false;
        }
        let (reader, scope, decls) = (&mut *self.reader, &*self.scope, &mut *self.decls);
        let descend = match n.kind() {
            "class_body" => false,
            k if is_function_kind(k) => false,
            "call_expression" => {
                let read = (self.program && reader.define(n))
                    || reader.registry_adds(v, scope, decls)
                    || reader.patch(n, scope)
                    || reader.include(n, scope);
                !read
            }
            "assignment_expression" => !reader.assignment(n, scope, decls),
            "variable_declarator" => !reader.declarator(n, scope),
            _ => true,
        };
        if descend && n != self.root {
            let names = block_names(n, reader.src);
            if !names.is_empty() {
                let saved = self.scope.shadow(names);
                self.blocks.push((n, saved));
            }
        }
        descend
    }

    fn leave(&mut self, v: &Visit<'t>) {
        if self.blocks.last().is_some_and(|(n, _)| *n == v.node)
            && let Some((_, saved)) = self.blocks.pop()
        {
            self.scope.restore(saved);
        }
    }
}

/// The names a block, a loop header or a `catch` declares for itself.
fn block_names(node: Node, src: &[u8]) -> Vec<String> {
    let mut patterns = Vec::new();
    match node.kind() {
        "statement_block" | "switch_case" | "switch_default" => {
            for statement in named(node) {
                match statement.kind() {
                    "lexical_declaration" | "variable_declaration" => patterns.extend(
                        named(statement)
                            .into_iter()
                            .filter_map(|d| d.child_by_field_name("name")),
                    ),
                    "class_declaration" | "function_declaration" | "generator_function_declaration" => {
                        patterns.extend(statement.child_by_field_name("name"))
                    }
                    _ => {}
                }
            }
        }
        "for_statement" => {
            let initializer = node.child_by_field_name("initializer");
            for declarator in initializer.map(named).unwrap_or_default() {
                patterns.extend(declarator.child_by_field_name("name"));
            }
        }
        "for_in_statement" if node.child_by_field_name("kind").is_some() => {
            patterns.extend(node.child_by_field_name("left"))
        }
        "catch_clause" => patterns.extend(node.child_by_field_name("parameter")),
        _ => {}
    }
    let mut names = Vec::new();
    while let Some(p) = patterns.pop() {
        match p.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => names.push(text(p, src).into_owned()),
            "object_pattern" | "array_pattern" | "rest_pattern" => patterns.extend(named(p)),
            "pair_pattern" => patterns.extend(p.child_by_field_name("value")),
            "assignment_pattern" | "object_assignment_pattern" => patterns.extend(p.child_by_field_name("left")),
            _ => {}
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn past_the_budget_symbols_are_dropped_with_one_warning() {
        let src =
            b"registry.category('a').add('one', 1);\nregistry.category('a').add('two', 2);\nexport const three = 3;\n";
        let full = Js.extract(src).unwrap();
        let size = |name: &str| {
            serde_json::to_vec(full.iter().find(|s| s.name == name).unwrap())
                .unwrap()
                .len()
        };
        let tree = syntax::parse(src, &tree_sitter_javascript::LANGUAGE.into()).unwrap();
        let mut reader = Reader::new(src, Budget::new(b"", 0, size("") + size("a:one")), Vec::new());
        reader.program(tree.root_node());
        let table = reader.finish();
        let names: Vec<&str> = table.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["", "a:one"]);
        // The module comes first, then `a:one`; `a:two` and the export `#three` are dropped.
        assert_eq!(table.warnings(), ["output budget exceeded; 2 symbols dropped"]);
    }
}
