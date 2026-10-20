//! Models, fields, methods, selection values, constraints and routes from Python files, read with
//! tree-sitter. Model-scope symbols are named after the model, the way Odoo code refers to them;
//! the others after their import path.

mod body;
mod value;

use std::borrow::Cow;
use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use tree_sitter::{Node, Parser};

use crate::path::module_of;
use crate::{Extractor, Span, Symbol, SymbolTable};
use body::body_hash;
use value::{canonical, expr, plain_string, value};

const MODEL: &str = "model";
const FIELD: &str = "field";
const METHOD: &str = "method";
const SELECTION: &str = "selection";
const CONSTRAINT: &str = "constraint";
const CLASS: &str = "class";
const FUNCTION: &str = "function";
const ROUTE: &str = "route";

/// Syntax diagnostics past this many are summed up in one warning.
const MAX_SYNTAX_WARNINGS: usize = 10;

const ROUTE_DECORATORS: [&str; 3] = ["http.route", "route", "odoo.http.route"];

pub struct Python;

impl Extractor for Python {
    fn name(&self) -> &'static str {
        "python"
    }

    /// Bump with any change to the output, including a tree-sitter or tree-sitter-python upgrade.
    fn version(&self) -> u32 {
        1
    }

    /// Importable files only: migration scripts and other paths that are no Python identifier
    /// cannot hold models, since Odoo refuses a model outside `odoo.addons`. Manifests are data.
    fn claims(&self, path: &str) -> bool {
        let Some(stem) = path.strip_suffix(".py") else {
            return false;
        };
        let file = stem.rsplit('/').next().unwrap_or(stem);
        file != "__manifest__" && file != "__openerp__" && stem.split('/').all(is_identifier)
    }

    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        let tree = parser
            .parse(blob, None)
            .ok_or_else(|| anyhow!("tree-sitter returned no tree"))?;
        let root = tree.root_node();
        let mut reader = Reader {
            src: blob,
            constants: BTreeMap::new(),
            models: Vec::new(),
            bindings: BTreeMap::new(),
            warnings: syntax_warnings(root, blob),
        };
        reader.module(root);
        Ok(reader.finish())
    }

    /// Model-scope symbols get `@<module>`; the others are prefixed with the file's import path.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        match symbol.kind.as_str() {
            CLASS | FUNCTION | ROUTE => format!("{}.{}", import_path(path), symbol.name),
            _ => format!("{}@{}", symbol.name, module_of(path).unwrap_or_default()),
        }
    }
}

fn is_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The dotted path custom code imports the file by.
fn import_path(path: &str) -> String {
    let stem = path.strip_suffix(".py").unwrap_or(path);
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    let dotted = stem.replace('/', ".");
    if let Some(rest) = stem.strip_prefix("addons/") {
        format!("odoo.addons.{}", rest.replace('/', "."))
    } else if stem == "odoo" || stem.starts_with("odoo/") || module_of(path).is_none() {
        dotted
    } else {
        format!("odoo.addons.{dotted}")
    }
}

/// One step of `visit`: the node, its parent and the field it fills there.
struct Visit<'t> {
    node: Node<'t>,
    parent: Option<Node<'t>>,
    field: Option<&'t str>,
}

impl Visit<'_> {
    /// Whether this is the comma of `(a,)`, which makes the tuple, unlike a trailing one.
    fn closes_single_tuple(&self) -> bool {
        self.parent
            .is_some_and(|p| matches!(p.kind(), "tuple" | "tuple_pattern") && p.named_child_count() == 1)
    }
}

trait Visitor<'t> {
    /// Returns whether to walk the node's children.
    fn enter(&mut self, v: &Visit<'t>) -> bool;
    fn leave(&mut self, _: &Visit<'t>) {}
}

impl<'t, F: FnMut(&Visit<'t>) -> bool> Visitor<'t> for F {
    fn enter(&mut self, v: &Visit<'t>) -> bool {
        self(v)
    }
}

/// Walks `root`'s subtree in source order without recursion, which long operator chains would
/// overflow.
fn visit<'t>(root: Node<'t>, visitor: &mut impl Visitor<'t>) {
    let mut cursor = root.walk();
    let mut ancestors: Vec<Visit<'t>> = Vec::new();
    loop {
        let here = Visit {
            node: cursor.node(),
            parent: ancestors.last().map(|a| a.node),
            field: cursor.field_name(),
        };
        if visitor.enter(&here) && cursor.goto_first_child() {
            ancestors.push(here);
            continue;
        }
        visitor.leave(&here);
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
            visitor.leave(&ancestors.pop().expect("the cursor's parent was entered"));
        }
    }
}

/// Feeds tokens on, holding each comma back until the next token shows whether it is a trailing
/// one, which is dropped. Comments never reach it, so a comma before one is still trailing.
struct Tokens<F: FnMut(&str)> {
    emit: F,
    comma: bool,
}

impl<F: FnMut(&str)> Tokens<F> {
    fn new(emit: F) -> Self {
        Tokens { emit, comma: false }
    }

    fn push(&mut self, token: &str) {
        if std::mem::take(&mut self.comma) && !matches!(token, ")" | "]" | "}") {
            (self.emit)(",");
        }
        if token == "," {
            self.comma = true;
        } else {
            (self.emit)(token);
        }
    }

    /// A token that no comma before it can be trailing for.
    fn mark(&mut self, token: &str) {
        self.finish();
        (self.emit)(token);
    }

    fn finish(&mut self) {
        if std::mem::take(&mut self.comma) {
            (self.emit)(",");
        }
    }
}

fn text<'s>(node: Node, src: &'s [u8]) -> Cow<'s, str> {
    String::from_utf8_lossy(&src[node.byte_range()])
}

/// Named children without comments.
fn named(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|n| !n.is_extra()).collect()
}

/// The statements of a module or block in order, those inside ERROR nodes included, so that the
/// definitions around a syntax error survive it.
fn statements(block: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut todo = named(block);
    todo.reverse();
    while let Some(n) = todo.pop() {
        if n.is_error() {
            todo.extend(named(n).into_iter().rev());
        } else {
            out.push(n);
        }
    }
    out
}

/// `(left, right)` of a statement assigning to one name.
fn assignment(statement: Node) -> Option<(Node, Node)> {
    let a = named(statement)
        .into_iter()
        .next()
        .filter(|_| statement.kind() == "expression_statement")?;
    let left = a.child_by_field_name("left").filter(|_| a.kind() == "assignment")?;
    let right = a.child_by_field_name("right")?;
    (left.kind() == "identifier").then_some((left, right))
}

/// `X` when `node` is the call `<object>.X(...)`.
fn call_of<'s>(node: Node, object: &str, src: &'s [u8]) -> Option<Cow<'s, str>> {
    let callee = node.child_by_field_name("function").filter(|_| node.kind() == "call")?;
    let target = callee
        .child_by_field_name("object")
        .filter(|_| callee.kind() == "attribute")?;
    let attribute = callee.child_by_field_name("attribute")?;
    (target.kind() == "identifier" && text(target, src) == object).then(|| text(attribute, src))
}

/// Positional arguments, then keyword arguments with `**x` as `("**", x)`.
fn call_args<'t>(call: Node<'t>, src: &[u8]) -> (Vec<Node<'t>>, Vec<(String, Node<'t>)>) {
    let (mut positional, mut keywords) = (Vec::new(), Vec::new());
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return (positional, keywords);
    };
    if arguments.kind() != "argument_list" {
        positional.push(arguments);
        return (positional, keywords);
    }
    for arg in named(arguments) {
        match arg.kind() {
            "keyword_argument" => {
                if let (Some(k), Some(v)) = (arg.child_by_field_name("name"), arg.child_by_field_name("value")) {
                    keywords.push((text(k, src).into_owned(), v));
                }
            }
            "dictionary_splat" => keywords.extend(named(arg).first().map(|&v| ("**".to_string(), v))),
            _ => positional.push(arg),
        }
    }
    (positional, keywords)
}

/// A call's arguments by parameter name: positionals take `names` in order, then `#<index>`.
fn arguments<'t>(call: Node<'t>, names: &[&str], src: &[u8]) -> BTreeMap<String, Node<'t>> {
    let (positional, keywords) = call_args(call, src);
    let mut out = BTreeMap::new();
    for (i, arg) in positional.into_iter().enumerate() {
        let name = names.get(i).map_or_else(|| format!("#{i}"), |n| n.to_string());
        out.insert(name, arg);
    }
    out.extend(keywords);
    out
}

fn values(args: BTreeMap<String, Node>, src: &[u8]) -> Value {
    Value::Object(args.into_iter().map(|(k, v)| (k, value(v, src))).collect())
}

/// Whether `node` is a name such as `api.depends`.
fn is_dotted(node: Node) -> bool {
    let mut n = node;
    while n.kind() == "attribute" {
        match (n.child_by_field_name("object"), n.child_by_field_name("attribute")) {
            (Some(object), Some(a)) if a.kind() == "identifier" => n = object,
            _ => return false,
        }
    }
    n.kind() == "identifier"
}

/// Positional parameters of a field type, stable from 12.0 to master.
fn field_parameters(field_type: &str) -> &'static [&'static str] {
    match field_type {
        "Selection" | "Reference" => &["selection", "string"],
        "Many2one" => &["comodel_name", "string"],
        "One2many" => &["comodel_name", "inverse_name", "string"],
        "Many2many" => &["comodel_name", "relation", "column1", "column2", "string"],
        "Float" => &["string", "digits"],
        "Monetary" => &["string", "currency_field"],
        _ => &["string"],
    }
}

/// The byte range and line of a name: a string literal's content without quotes, or the node.
fn name_span(node: Node) -> (Span, u32) {
    let (start, end, row) = match value::delimiters(node) {
        Some((open, close)) if node.kind() == "string" => {
            (open.end_byte(), close.start_byte(), open.end_position().row)
        }
        _ => (node.start_byte(), node.end_byte(), node.start_position().row),
    };
    (
        Span {
            start: start as u32,
            end: end as u32,
        },
        row as u32 + 1,
    )
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

/// `line N: ...` for ERROR and MISSING nodes and Python 2 syntax, the outermost ERROR only,
/// keyed by byte offset. tree-sitter accepts `print x`, `exec x`, `except E, e`, backticks and
/// `<>`, but each is an import error under Python 3, which loses the whole file.
fn syntax_warnings(root: Node, src: &[u8]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    visit(root, &mut |v: &Visit| {
        let n = v.node;
        let line = n.start_position().row + 1;
        let (warning, descend) = if n.is_error() {
            (Some(format!("line {line}: syntax error")), false)
        } else if n.is_missing() {
            (Some(format!("line {line}: missing {}", n.kind())), false)
        } else {
            let python2 = match n.kind() {
                "print_statement" | "exec_statement" | "<>" => true,
                "except_clause" => {
                    n.child_by_field_name("alias").is_none()
                        && n.children_by_field_name("value", &mut n.walk()).count() == 2
                }
                "string_start" => src.get(n.start_byte()) == Some(&b'`'),
                _ => false,
            };
            (python2.then(|| format!("line {line}: Python 2 syntax")), true)
        };
        found.extend(warning.map(|w| (n.start_byte(), w)));
        descend
    });
    if found.len() > MAX_SYNTAX_WARNINGS {
        let more = found.len() - MAX_SYNTAX_WARNINGS;
        let at = found[MAX_SYNTAX_WARNINGS].0;
        found.truncate(MAX_SYNTAX_WARNINGS);
        found.push((at, format!("{more} more syntax errors")));
    }
    found
}

struct Reader<'t> {
    src: &'t [u8],
    /// Module-level names last assigned a literal list or tuple, for `selection=NAME`.
    constants: BTreeMap<String, Node<'t>>,
    /// Model classes in source order.
    models: Vec<ModelClass>,
    /// Module-level names, the last definition of each winning as in Python: a function, or a
    /// plain class with its methods and routes.
    bindings: BTreeMap<String, Vec<Symbol>>,
    /// By byte offset, so that they end up in document order.
    warnings: Vec<(usize, String)>,
}

struct ModelClass {
    class: String,
    model: Symbol,
    /// By `(kind, name within the model)`, the last definition winning as in Python.
    members: BTreeMap<(&'static str, String), Member>,
}

/// A member, and for a field, its selection values by value.
struct Member {
    symbol: Symbol,
    values: BTreeMap<String, Symbol>,
}

impl From<Symbol> for Member {
    fn from(symbol: Symbol) -> Self {
        Member {
            symbol,
            values: BTreeMap::new(),
        }
    }
}

enum Naming<'t> {
    Model(String, Node<'t>),
    /// Sets `_name` or `_inherit` without naming one model; the reason.
    Unnamed(&'static str),
    Plain,
}

impl<'t> Reader<'t> {
    fn text(&self, node: Node) -> Cow<'t, str> {
        text(node, self.src)
    }

    fn module(&mut self, root: Node<'t>) {
        for statement in statements(root) {
            match statement.kind() {
                "expression_statement" => self.constant(statement),
                "class_definition" => self.class(statement, &[]),
                "function_definition" => self.function(statement, &[]),
                "decorated_definition" => {
                    let decorators: Vec<Node> = named(statement)
                        .into_iter()
                        .filter(|d| d.kind() == "decorator")
                        .collect();
                    match statement.child_by_field_name("definition") {
                        Some(d) if d.kind() == "class_definition" => self.class(d, &decorators),
                        Some(d) if d.kind() == "function_definition" => self.function(d, &decorators),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn constant(&mut self, statement: Node<'t>) {
        if let Some((left, right)) = assignment(statement) {
            let name = self.text(left).into_owned();
            if matches!(right.kind(), "list" | "tuple") {
                self.constants.insert(name, right);
            } else {
                self.constants.remove(&name);
            }
        }
    }

    fn function(&mut self, def: Node<'t>, decorators: &[Node<'t>]) {
        if let Some(name) = def.child_by_field_name("name") {
            let fname = self.text(name).into_owned();
            let function = symbol(FUNCTION, fname.clone(), name, self.def_attrs(def, decorators));
            self.bindings.insert(fname, vec![function]);
        }
    }

    fn class(&mut self, node: Node<'t>, decorators: &[Node<'t>]) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let class = self.text(name).into_owned();
        let body = node.child_by_field_name("body").map(statements).unwrap_or_default();
        let bases: Vec<Value> = node
            .child_by_field_name("superclasses")
            .map(named)
            .unwrap_or_default()
            .into_iter()
            .map(|b| canonical(b, self.src).into())
            .collect();
        let last = |attr: &str| {
            body.iter()
                .rev()
                .filter_map(|&s| assignment(s))
                .find(|(left, _)| text(*left, self.src) == attr)
                .map(|(_, right)| right)
        };
        match self.naming(last("_name"), last("_inherit")) {
            Naming::Model(model, at) => {
                let mut attrs = BTreeMap::from([("class".to_string(), Value::from(class.as_str()))]);
                attrs.insert("bases".into(), bases.into());
                self.model_class(class, model, at, &body, attrs);
            }
            Naming::Unnamed(reason) => {
                let line = name.start_position().row + 1;
                self.warnings.push((
                    name.start_byte(),
                    format!("line {line}: class {class} names no model ({reason}); read as a plain class"),
                ));
                self.plain_class(class, name, &body, bases, decorators);
            }
            Naming::Plain => self.plain_class(class, name, &body, bases, decorators),
        }
    }

    /// Odoo's rule: `_name`, else a single `_inherit`.
    fn naming(&self, name: Option<Node<'t>>, inherit: Option<Node<'t>>) -> Naming<'t> {
        let model = |n: Node<'t>| plain_string(n, self.src).map(|s| Naming::Model(s, n));
        let sequence = |n: Node| matches!(n.kind(), "list" | "tuple");
        let name = name.filter(|n| n.kind() != "none");
        let inherit = inherit.filter(|&i| !(sequence(i) && named(i).is_empty()));
        match (name, inherit) {
            (Some(n), _) => model(n).unwrap_or(Naming::Unnamed("_name is not a string")),
            (None, None) => Naming::Plain,
            (None, Some(i)) if sequence(i) => {
                let items = named(i);
                match items[..] {
                    [single] => model(single).unwrap_or(Naming::Unnamed("_inherit is not a string")),
                    _ => Naming::Unnamed("several _inherit and no _name"),
                }
            }
            (None, Some(i)) => model(i).unwrap_or(Naming::Unnamed("_inherit is not a string")),
        }
    }

    fn model_class(
        &mut self,
        class: String,
        model: String,
        at: Node<'t>,
        body: &[Node<'t>],
        mut attrs: BTreeMap<String, Value>,
    ) {
        let mut members = BTreeMap::new();
        for &statement in body {
            match statement.kind() {
                "expression_statement" => {
                    let Some((left, right)) = assignment(statement) else {
                        continue;
                    };
                    let name = self.text(left).into_owned();
                    if let Some(field) = self.field(&name, left, right) {
                        members.insert((FIELD, name), field);
                    } else if let Some(constraint) = self.table_object(&name, left, right) {
                        members.insert((CONSTRAINT, constraint.name.clone()), constraint.into());
                    } else if let Some(constraints) = self.sql_constraints(&name, right) {
                        for c in constraints {
                            members.insert((CONSTRAINT, c.name.clone()), c.into());
                        }
                    } else if name.starts_with('_') && !name.starts_with("__") {
                        let value = match value(right, self.src) {
                            Value::String(s) if name == "_inherit" => Value::Array(vec![s.into()]),
                            v => v,
                        };
                        attrs.insert(name, value);
                    }
                }
                "function_definition" | "decorated_definition" => {
                    if let Some((def, decorators)) = self.definition(statement) {
                        let name = def.child_by_field_name("name").expect("a def has a name");
                        let method = symbol(METHOD, self.text(name).into(), name, self.def_attrs(def, &decorators));
                        members.insert((METHOD, method.name.clone()), method.into());
                    }
                }
                _ => {}
            }
        }
        self.bindings.remove(&class);
        self.models.push(ModelClass {
            class,
            model: symbol(MODEL, model, at, attrs),
            members,
        });
    }

    /// A def and its decorators, from a statement that is one.
    fn definition(&self, statement: Node<'t>) -> Option<(Node<'t>, Vec<Node<'t>>)> {
        let (def, decorators) = match statement.kind() {
            "decorated_definition" => (
                statement.child_by_field_name("definition")?,
                named(statement)
                    .into_iter()
                    .filter(|d| d.kind() == "decorator")
                    .collect(),
            ),
            _ => (statement, Vec::new()),
        };
        (def.kind() == "function_definition" && def.child_by_field_name("name").is_some()).then_some((def, decorators))
    }

    fn field(&self, name: &str, left: Node<'t>, right: Node<'t>) -> Option<Member> {
        let field_type = call_of(right, "fields", self.src)?;
        let mut args = arguments(right, field_parameters(&field_type), self.src);
        let mut attrs = BTreeMap::from([("type".to_string(), Value::from(field_type.as_ref()))]);
        if let Some(comodel) = args.remove("comodel_name") {
            attrs.insert("comodel".into(), value(comodel, self.src));
        }
        let mut selection = BTreeMap::new();
        for declared in ["selection", "selection_add"] {
            if let Some(&list) = args.get(declared) {
                self.selection_values(list, declared, &mut selection);
            }
        }
        if !args.is_empty() {
            attrs.insert("args".into(), values(args, self.src));
        }
        Some(Member {
            symbol: symbol(FIELD, name.into(), left, attrs),
            values: selection,
        })
    }

    /// One symbol per `(value, label)` of a literal list, or of a module constant assigned one
    /// earlier in the file.
    fn selection_values(&self, node: Node<'t>, declared: &str, out: &mut BTreeMap<String, Symbol>) {
        let list = match node.kind() {
            "identifier" => self.constants.get(self.text(node).as_ref()).copied(),
            "list" | "tuple" => Some(node),
            _ => None,
        };
        for item in list.map(named).unwrap_or_default() {
            if !matches!(item.kind(), "tuple" | "list") {
                continue;
            }
            let parts = named(item);
            let Some(&key) = parts.first() else {
                continue;
            };
            let name = match value(key, self.src) {
                Value::String(s) => s,
                Value::Number(n) if n.is_i64() => n.to_string(),
                _ => continue,
            };
            let label = parts.get(1).map_or(Value::Null, |&l| value(l, self.src));
            let attrs = BTreeMap::from([("declared".to_string(), declared.into()), ("label".to_string(), label)]);
            out.insert(name.clone(), symbol(SELECTION, name, key, attrs));
        }
    }

    /// `_x = models.Constraint(...)`, `UniqueIndex` or `Index` (19.0 and later), named like Odoo
    /// does: the attribute without one leading underscore.
    fn table_object(&self, name: &str, left: Node<'t>, right: Node<'t>) -> Option<Symbol> {
        let object = call_of(right, "models", self.src)?;
        let parameters: &[&str] = match object.as_ref() {
            "Constraint" | "UniqueIndex" => &["definition", "message"],
            "Index" => &["definition"],
            _ => return None,
        };
        let mut args = arguments(right, parameters, self.src);
        let mut attrs = BTreeMap::from([("declared".to_string(), Value::from(format!("models.{object}")))]);
        for key in ["definition", "message"] {
            if let Some(v) = args.remove(key) {
                attrs.insert(key.into(), value(v, self.src));
            }
        }
        if !args.is_empty() {
            attrs.insert("args".into(), values(args, self.src));
        }
        let member = name.strip_prefix('_').unwrap_or(name);
        let mut constraint = symbol(CONSTRAINT, member.into(), left, attrs);
        constraint.span.start += (name.len() - member.len()) as u32;
        Some(constraint)
    }

    /// `None` unless `_sql_constraints` is a literal list of `(name, definition, message)`.
    fn sql_constraints(&self, name: &str, right: Node<'t>) -> Option<Vec<Symbol>> {
        if name != "_sql_constraints" || !matches!(right.kind(), "list" | "tuple") {
            return None;
        }
        let mut out = Vec::new();
        for item in named(right) {
            let parts = Some(named(item)).filter(|p| matches!(item.kind(), "tuple" | "list") && p.len() == 3)?;
            let constraint = plain_string(parts[0], self.src)?;
            let attrs = BTreeMap::from([
                ("declared".to_string(), Value::from("_sql_constraints")),
                ("definition".to_string(), value(parts[1], self.src)),
                ("message".to_string(), value(parts[2], self.src)),
            ]);
            out.push(symbol(CONSTRAINT, constraint, parts[0], attrs));
        }
        Some(out)
    }

    /// The class and the methods of a class that declares no model. `test*` methods are left
    /// out: unittest runs them and no code calls them.
    fn plain_class(
        &mut self,
        class: String,
        name: Node<'t>,
        body: &[Node<'t>],
        bases: Vec<Value>,
        decorators: &[Node<'t>],
    ) {
        let mut attrs = BTreeMap::from([("bases".to_string(), Value::from(bases))]);
        if !decorators.is_empty() {
            attrs.insert("decorators".into(), self.decorators(decorators));
        }
        let mut methods: BTreeMap<String, Vec<Symbol>> = BTreeMap::new();
        for &statement in body {
            let Some((def, decorators)) = self.definition(statement) else {
                continue;
            };
            let at = def.child_by_field_name("name").expect("a def has a name");
            let method = self.text(at).into_owned();
            if method.starts_with("test") {
                continue;
            }
            let qualified = format!("{class}.{method}");
            let mut symbols = vec![symbol(
                FUNCTION,
                qualified.clone(),
                at,
                self.def_attrs(def, &decorators),
            )];
            symbols.extend(self.route(&decorators).map(|attrs| symbol(ROUTE, qualified, at, attrs)));
            methods.insert(method, symbols);
        }
        let mut symbols = vec![symbol(CLASS, class.clone(), name, attrs)];
        symbols.extend(methods.into_values().flatten());
        self.bindings.insert(class, symbols);
    }

    /// `urls` and the other arguments of an `http.route` decorator, if one is among them.
    fn route(&self, decorators: &[Node<'t>]) -> Option<BTreeMap<String, Value>> {
        let expression = decorators
            .iter()
            .filter_map(|&d| named(d).first().copied())
            .find(|&e| {
                let callee = if e.kind() == "call" {
                    e.child_by_field_name("function")
                } else {
                    Some(e)
                };
                callee.is_some_and(|c| ROUTE_DECORATORS.contains(&canonical(c, self.src).as_str()))
            })?;
        let mut args = if expression.kind() == "call" {
            arguments(expression, &["route"], self.src)
        } else {
            BTreeMap::new()
        };
        let urls = match args.remove("route").map(|r| value(r, self.src)) {
            None => Vec::new(),
            Some(Value::Array(urls)) => urls,
            Some(url) => vec![url],
        };
        let mut attrs = BTreeMap::from([("urls".to_string(), Value::from(urls))]);
        if !args.is_empty() {
            attrs.insert("args".into(), values(args, self.src));
        }
        Some(attrs)
    }

    fn def_attrs(&self, def: Node<'t>, decorators: &[Node<'t>]) -> BTreeMap<String, Value> {
        let signature: Vec<Value> = def
            .child_by_field_name("parameters")
            .map(named)
            .unwrap_or_default()
            .into_iter()
            .map(|p| self.parameter(p).into())
            .collect();
        let mut attrs = BTreeMap::from([
            ("signature".to_string(), Value::from(signature)),
            ("body".to_string(), body_hash(def, self.src).into()),
        ]);
        if !decorators.is_empty() {
            attrs.insert("decorators".into(), self.decorators(decorators));
        }
        if let Some(returns) = def.child_by_field_name("return_type") {
            attrs.insert("returns".into(), canonical(returns, self.src).into());
        }
        if def.child(0).is_some_and(|c| c.kind() == "async") {
            attrs.insert("async".into(), true.into());
        }
        attrs
    }

    fn parameter(&self, p: Node<'t>) -> String {
        let field = |name: &str| {
            p.child_by_field_name(name)
                .map(|n| canonical(n, self.src))
                .unwrap_or_default()
        };
        match p.kind() {
            "typed_parameter" => {
                let name = named(p).first().map(|&n| canonical(n, self.src)).unwrap_or_default();
                format!("{name}: {}", field("type"))
            }
            "typed_default_parameter" => format!("{}: {}={}", field("name"), field("type"), field("value")),
            _ => canonical(p, self.src),
        }
    }

    /// `{"name", "args"?, "kwargs"?}` per decorator, or `{"expr"}` when it calls no plain name.
    fn decorators(&self, decorators: &[Node<'t>]) -> Value {
        let mut out = Vec::new();
        for expression in decorators.iter().filter_map(|&d| named(d).first().copied()) {
            let call = (expression.kind() == "call").then_some(expression);
            let callee = call.map_or(Some(expression), |c| c.child_by_field_name("function"));
            let Some(callee) = callee.filter(|&c| is_dotted(c)) else {
                out.push(expr(expression, self.src));
                continue;
            };
            let mut decorator = Map::new();
            decorator.insert("name".into(), canonical(callee, self.src).into());
            if let Some(call) = call {
                let (positional, keywords) = call_args(call, self.src);
                if !positional.is_empty() {
                    let args = positional.into_iter().map(|a| value(a, self.src));
                    decorator.insert("args".into(), Value::Array(args.collect()));
                }
                if !keywords.is_empty() {
                    let kwargs: BTreeMap<String, Node> = keywords.into_iter().collect();
                    decorator.insert("kwargs".into(), values(kwargs, self.src));
                }
            }
            out.push(Value::Object(decorator));
        }
        Value::Array(out)
    }

    /// Model-scope members go in class order, then in key order; a name an earlier class took
    /// becomes `<model>[<Class>]...`, and one taken even so is dropped with a warning.
    fn finish(mut self) -> SymbolTable {
        let mut table = SymbolTable::default();
        let mut dropped = Vec::new();
        let mut insert = |table: &mut SymbolTable, symbol: Symbol| {
            if table.contains(&symbol.kind, &symbol.name) {
                let warning = format!(
                    "line {}: duplicate {} {} dropped",
                    symbol.line, symbol.kind, symbol.name
                );
                dropped.push((symbol.span.start as usize, warning));
                false
            } else {
                table.insert(symbol)
            }
        };
        for ModelClass {
            class,
            mut model,
            members,
        } in self.models
        {
            let plain = model.name.clone();
            if table.contains(MODEL, &plain) {
                model.name = format!("{plain}[{class}]");
            }
            insert(&mut table, model);
            for ((kind, local), Member { mut symbol, values }) in members {
                symbol.name = format!("{plain}#{local}");
                if table.contains(kind, &symbol.name) {
                    symbol.name = format!("{plain}[{class}]#{local}");
                }
                let prefix = symbol.name.clone();
                if !insert(&mut table, symbol) {
                    continue;
                }
                for (v, mut value) in values {
                    value.name = format!("{prefix}:{v}");
                    insert(&mut table, value);
                }
            }
        }
        for symbol in self.bindings.into_values().flatten() {
            table.insert(symbol);
        }
        self.warnings.extend(dropped);
        self.warnings.sort_by_key(|(at, _)| *at);
        for (_, warning) in self.warnings {
            table.warn(warning);
        }
        table
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_path_of_every_layout() {
        for (path, expected) in [
            (
                "addons/sale/controllers/portal.py",
                "odoo.addons.sale.controllers.portal",
            ),
            ("addons/sale/__init__.py", "odoo.addons.sale"),
            (
                "odoo/addons/base/models/res_users.py",
                "odoo.addons.base.models.res_users",
            ),
            ("odoo/tools/misc.py", "odoo.tools.misc"),
            ("odoo/__init__.py", "odoo"),
            (
                "sale_subscription/models/sale_order.py",
                "odoo.addons.sale_subscription.models.sale_order",
            ),
            ("setup.py", "setup"),
        ] {
            assert_eq!(import_path(path), expected, "{path}");
        }
    }
}
