//! Models, fields, methods, selection values, constraints, routes and constants from Python
//! files, read with tree-sitter. Model-scope symbols are named after the model, the way Odoo code
//! refers to them; the others after their import path.

mod body;
pub(crate) mod value;

use std::borrow::Cow;
use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{Map, Value};
use tree_sitter::{Node, Tree};

use crate::path::module_of;
use crate::syntax::{self, Visit, named, named_with_errors, statements, text};
use crate::{Extractor, Parent, Span, Symbol, SymbolTable};
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
const CONSTANT: &str = "constant";

const ROUTE_DECORATORS: [&str; 3] = ["http.route", "route", "odoo.http.route"];

pub struct Python;

impl Extractor for Python {
    fn name(&self) -> &'static str {
        "python"
    }

    /// Bump with any change to the output, including a tree-sitter or tree-sitter-python upgrade
    /// and changes to `syntax`.
    fn version(&self) -> u32 {
        2
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
        let tree = parse(blob)?;
        let root = tree.root_node();
        let mut reader = Reader {
            src: blob,
            constants: BTreeMap::new(),
            models: Vec::new(),
            bindings: BTreeMap::new(),
            warnings: syntax::syntax_warnings(root, |n| python2(n, blob)),
        };
        reader.module(root);
        Ok(reader.finish())
    }

    /// Model-scope symbols get `@<module>`; the others are prefixed with the file's import path.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        match symbol.kind.as_str() {
            CLASS | FUNCTION | ROUTE | CONSTANT => format!("{}.{}", import_path(path), symbol.name),
            _ => format!("{}@{}", symbol.name, module_of(path).unwrap_or_default()),
        }
    }

    /// A model's members belong to it, and a selection value to its field.
    fn parent<'q>(&self, kind: &str, qualname: &'q str) -> Option<Parent<'q>> {
        match kind {
            FIELD | METHOD | CONSTRAINT => Parent::member(MODEL, qualname),
            SELECTION => {
                let model = Parent::member(MODEL, qualname)?;
                let (field, value) = model.local.split_once(':')?;
                let (head, module) = model.qualname.rsplit_once('@')?;
                Some(Parent {
                    kind: FIELD,
                    qualname: format!("{head}#{field}@{module}"),
                    local: value,
                })
            }
            _ => None,
        }
    }
}

pub(crate) fn parse(blob: &[u8]) -> Result<Tree> {
    syntax::parse(blob, &tree_sitter_python::LANGUAGE.into())
}

/// Python 2 syntax tree-sitter accepts: `print x`, `exec x`, `except E, e`, backticks and `<>`.
/// Each is an import error under Python 3, which loses the whole file. The manifest extractor
/// flags it too: a change here bumps both extractors' `version()`.
pub(crate) fn python2(n: Node, src: &[u8]) -> Option<String> {
    let python2 = match n.kind() {
        "print_statement" | "exec_statement" | "<>" => true,
        "except_clause" => {
            n.child_by_field_name("alias").is_none() && n.children_by_field_name("value", &mut n.walk()).count() == 2
        }
        "string_start" => src.get(n.start_byte()) == Some(&b'`'),
        _ => false,
    };
    python2.then(|| "Python 2 syntax".to_string())
}

/// Whether `v` is the comma of `(a,)`, which makes the tuple, unlike a trailing one.
fn closes_single_tuple(v: &Visit) -> bool {
    v.parent
        .is_some_and(|p| matches!(p.kind(), "tuple" | "tuple_pattern") && named(p).len() == 1)
}

/// `[A-Z][A-Z0-9_]*`.
fn is_constant(name: &str) -> bool {
    name.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
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

/// `(name, value)` per name a statement assigns, in order: `a = b = v` assigns `v` to both.
/// Targets that are no plain name, such as tuples and attributes, are left out.
fn assignments(statement: Node) -> Vec<(Node, Node)> {
    let mut targets = Vec::new();
    let mut node = named(statement)
        .into_iter()
        .next()
        .filter(|_| statement.kind() == "expression_statement");
    while let Some(a) = node.filter(|a| a.kind() == "assignment") {
        let (Some(left), Some(right)) = (a.child_by_field_name("left"), a.child_by_field_name("right")) else {
            return Vec::new();
        };
        targets.push(left);
        node = Some(right);
    }
    match node {
        Some(value) if !targets.is_empty() => targets
            .into_iter()
            .filter(|t| t.kind() == "identifier")
            .map(|t| (t, value))
            .collect(),
        _ => Vec::new(),
    }
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

/// Positional parameters of a field type, the same from 12.0 to master as of 2026-10-20.
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
        parts: None,
    }
}

struct Reader<'t> {
    src: &'t [u8],
    /// Module-level names last assigned a literal list or tuple, for `selection=NAME`.
    constants: BTreeMap<String, Node<'t>>,
    /// Model classes in source order.
    models: Vec<ModelClass>,
    /// Module-level names, the last definition of each winning as in Python: a function, a
    /// constant, or a plain class with its methods and routes.
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

    /// An all-caps name is a constant custom code may import; lower-case module globals such as
    /// `_logger` are not.
    fn constant(&mut self, statement: Node<'t>) {
        self.unpacked(statement);
        for (left, right) in assignments(statement) {
            let name = self.text(left).into_owned();
            if is_constant(&name) {
                self.bind_constant(left, value(right, self.src));
            }
            if matches!(right.kind(), "list" | "tuple") {
                self.constants.insert(name, right);
            } else {
                self.constants.remove(&name);
            }
        }
    }

    fn bind_constant(&mut self, name: Node<'t>, value: Value) {
        let text = self.text(name).into_owned();
        let attrs = BTreeMap::from([("value".to_string(), value)]);
        self.bindings
            .insert(text.clone(), vec![symbol(CONSTANT, text, name, attrs)]);
    }

    /// `A, B = 1, 2` gives each name its element, and `A, B = f()` the whole value. `A += v`
    /// appends `{op, value}` to an earlier constant's `augmented`, so editing it is a change.
    fn unpacked(&mut self, statement: Node<'t>) {
        let Some(&node) = named(statement).first() else { return };
        let (Some(left), Some(right)) = (node.child_by_field_name("left"), node.child_by_field_name("right")) else {
            return;
        };
        match (node.kind(), left.kind()) {
            ("assignment", "pattern_list" | "tuple_pattern") => {
                let targets = named(left);
                let values = match right.kind() {
                    "expression_list" | "tuple" => named(right),
                    _ => Vec::new(),
                };
                for (i, &target) in targets.iter().enumerate() {
                    if target.kind() != "identifier" || !is_constant(&self.text(target)) {
                        continue;
                    }
                    let v = match values.get(i).filter(|_| values.len() == targets.len()) {
                        Some(&v) => value(v, self.src),
                        None => expr(right, self.src),
                    };
                    self.bind_constant(target, v);
                }
            }
            ("augmented_assignment", "identifier") => {
                let op = node
                    .child_by_field_name("operator")
                    .map(|o| text(o, self.src).into_owned());
                let step = serde_json::json!({"op": op, "value": value(right, self.src)});
                let Some([constant]) = self
                    .bindings
                    .get_mut(text(left, self.src).as_ref())
                    .map(Vec::as_mut_slice)
                else {
                    return;
                };
                if constant.kind != CONSTANT {
                    return;
                }
                let steps = constant
                    .attrs
                    .entry("augmented".to_string())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Value::Array(steps) = steps {
                    steps.push(step);
                }
            }
            _ => {}
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
        // tree-sitter puts the statements before a syntax error in a class body in an ERROR child
        // of the class, ahead of `body`.
        let mut body: Vec<Node<'t>> = named_with_errors(node)
            .into_iter()
            .filter(|n| n.is_error())
            .flat_map(statements)
            .collect();
        body.extend(node.child_by_field_name("body").map(statements).unwrap_or_default());
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
                .flat_map(|&s| assignments(s))
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
        let inherit = inherit.filter(|&i| i.kind() != "none" && !(sequence(i) && named(i).is_empty()));
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
                    for (left, right) in assignments(statement) {
                        self.class_attribute(left, right, &mut members, &mut attrs);
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

    /// A field, a constraint or a model attr from `left = right` in a model class.
    fn class_attribute(
        &self,
        left: Node<'t>,
        right: Node<'t>,
        members: &mut BTreeMap<(&'static str, String), Member>,
        attrs: &mut BTreeMap<String, Value>,
    ) {
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

    /// The class and the methods of a class that declares no model. `test*` methods without a
    /// route are left out: unittest runs them and no code calls them.
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
            let route = self.route(&decorators);
            if method.starts_with("test") && route.is_none() {
                continue;
            }
            let qualified = format!("{class}.{method}");
            let mut symbols = vec![symbol(
                FUNCTION,
                qualified.clone(),
                at,
                self.def_attrs(def, &decorators),
            )];
            symbols.extend(route.map(|attrs| symbol(ROUTE, qualified, at, attrs)));
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
