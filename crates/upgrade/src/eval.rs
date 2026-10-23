//! A small static evaluator: the strings a call's arguments hold, following the loops around the
//! call, names assigned once, brace expansion and string formatting. Anything else is unknown,
//! which leaves the event partial rather than guessed.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use tree_sitter::Node;

/// Loop combinations past this many leave the call partial.
const MAX_RUNS: usize = 10_000;
/// Deeper than this, an expression or a chain of assignments reads as unknown, which also ends
/// a name assigned from itself.
const MAX_DEPTH: usize = 32;
/// Expressions evaluated for one call past this many leave it partial: a name read several times
/// per assignment down a chain of assignments costs exponential time otherwise.
const MAX_STEPS: usize = 100_000;
/// Methods that change their object in place, which leaves its literal value stale.
const MUTATORS: &[&str] = &[
    "add",
    "append",
    "clear",
    "discard",
    "extend",
    "insert",
    "pop",
    "popitem",
    "remove",
    "reverse",
    "setdefault",
    "sort",
    "update",
];

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Str(String),
    Int(i64),
    /// A tuple, or any other sequence but a list.
    Seq(Vec<Val>),
    /// Kept apart from tuples, which alone spread over `%`.
    List(Vec<Val>),
    /// Insertion order, as Python keeps it.
    Map(Vec<(Val, Val)>),
    /// A literal that is none of the above, such as `True` or `None`.
    Other,
}

impl Val {
    fn items(self) -> Option<Vec<Val>> {
        match self {
            Val::Seq(items) | Val::List(items) => Some(items),
            Val::Map(pairs) => Some(pairs.into_iter().map(|(k, _)| k).collect()),
            _ => None,
        }
    }

    fn display(&self) -> Option<String> {
        match self {
            Val::Str(s) => Some(s.clone()),
            Val::Int(i) => Some(i.to_string()),
            _ => None,
        }
    }
}

/// Names bound by the loops being expanded.
type Env = HashMap<String, Val>;

#[derive(Debug, Clone, Copy)]
enum Binding<'t> {
    /// `name = value`.
    Value(Node<'t>),
    /// A `for` target.
    Loop,
    /// Anything else: a parameter, an import, an augmented or unpacking assignment, a mutation.
    Other,
}

/// Each name a scope binds, with every binding of it.
type Bindings<'t> = HashMap<String, Vec<Binding<'t>>>;

pub(crate) struct File<'t, 's> {
    src: &'s [u8],
    /// Names the `util` module goes by.
    modules: HashSet<String>,
    /// A name bound to an upgrade-util function, by import or assignment, and that function.
    functions: HashMap<String, String>,
    scopes: RefCell<HashMap<usize, Rc<Bindings<'t>>>>,
    steps: Cell<usize>,
}

impl<'t, 's> File<'t, 's> {
    pub(crate) fn new(root: Node<'t>, src: &'s [u8]) -> Self {
        let mut file = File {
            src,
            modules: HashSet::from(["util".to_string()]),
            functions: HashMap::new(),
            scopes: RefCell::default(),
            steps: Cell::new(0),
        };
        let mut todo = vec![root];
        while let Some(n) = todo.pop() {
            match n.kind() {
                "import_from_statement" => file.import_from(n),
                "import_statement" => {
                    for (name, alias) in file.imported(n) {
                        if name.rsplit('.').next() == Some("util") && alias != name {
                            file.modules.insert(alias);
                        }
                    }
                }
                _ => {}
            }
            let mut cursor = n.walk();
            todo.extend(n.named_children(&mut cursor));
        }
        // `eb = util.expand_braces`, after the imports so that an imported name can be aliased.
        let mut todo = vec![root];
        while let Some(n) = todo.pop() {
            if n.kind() == "assignment"
                && let (Some(left), Some(right)) = (n.child_by_field_name("left"), n.child_by_field_name("right"))
                && left.kind() == "identifier"
                && let Some(function) = file.function(right)
            {
                file.functions.insert(file.text(left).into_owned(), function);
            }
            let mut cursor = n.walk();
            todo.extend(n.named_children(&mut cursor));
        }
        file
    }

    fn import_from(&mut self, n: Node) {
        let Some(module) = n.child_by_field_name("module_name") else {
            return;
        };
        let module = self.text(module).into_owned();
        let from_util = module.split('.').any(|part| part == "util");
        for (name, alias) in self.imported(n) {
            if name == "util" {
                self.modules.insert(alias);
            } else if from_util {
                self.functions.insert(alias, name);
            }
        }
    }

    /// `(name, alias)` of each name an import binds.
    fn imported(&self, n: Node) -> Vec<(String, String)> {
        let mut cursor = n.walk();
        n.children_by_field_name("name", &mut cursor)
            .map(|name| match name.kind() {
                "aliased_import" => {
                    let original = name.child_by_field_name("name").map(|o| self.text(o).into_owned());
                    let alias = name.child_by_field_name("alias").map(|a| self.text(a).into_owned());
                    (original.unwrap_or_default(), alias.unwrap_or_default())
                }
                _ => (self.text(name).into_owned(), self.text(name).into_owned()),
            })
            .collect()
    }

    pub(crate) fn text(&self, node: Node) -> Cow<'s, str> {
        String::from_utf8_lossy(&self.src[node.byte_range()])
    }

    /// The upgrade-util function `node` names: `util.<f>` or a name bound to one.
    fn function(&self, node: Node) -> Option<String> {
        match node.kind() {
            "attribute" => {
                let object = node.child_by_field_name("object")?;
                let attribute = node.child_by_field_name("attribute")?;
                (object.kind() == "identifier" && self.modules.contains(self.text(object).as_ref()))
                    .then(|| self.text(attribute).into_owned())
            }
            "identifier" => self.functions.get(self.text(node).as_ref()).cloned(),
            _ => None,
        }
    }

    /// The upgrade-util function `node` calls, if it is a call.
    pub(crate) fn callee(&self, node: Node) -> Option<String> {
        if node.kind() != "call" {
            return None;
        }
        self.function(node.child_by_field_name("function")?)
    }

    /// The arguments of `call` by parameter name, one map per run of the loops around it: `cr`,
    /// then `params` in order. A parameter the call passes maps to `None` when its value is not
    /// a string.
    pub(crate) fn bind(&self, call: Node<'t>, params: &[&str]) -> Vec<BTreeMap<String, Option<String>>> {
        let Some(arguments) = call
            .child_by_field_name("arguments")
            .filter(|a| a.kind() == "argument_list")
        else {
            return vec![BTreeMap::new()];
        };
        let args: Vec<Node> = named(arguments);
        let function = enclosing_scope(call);
        let loops = self.loops_around(call);
        let mut needed = HashSet::new();
        for &a in &args {
            self.free_names(a, &mut needed, 0);
        }
        // Inner to outer: a loop matters when it binds a name the call needs, and then the names
        // its own iterable needs matter too.
        let mut expanded = Vec::new();
        for &l in &loops {
            let targets = self.targets(l);
            if targets.iter().any(|t| needed.contains(t)) {
                expanded.push(l);
                if let Some(right) = l.child_by_field_name("right") {
                    self.free_names(right, &mut needed, 0);
                }
            }
        }
        let loop_names: HashSet<String> = loops.iter().flat_map(|&l| self.targets(l)).collect();
        let mut envs = vec![Env::new()];
        for &l in expanded.iter().rev() {
            match self.expand(l, &envs, &loop_names, function) {
                Some(next) => envs = next,
                None => {
                    envs = vec![Env::new()];
                    break;
                }
            }
        }
        let shadowed = self.shadowed(&loops);
        for env in &mut envs {
            env.retain(|name, _| !shadowed.contains(name));
        }
        let mut runs: Vec<_> = envs
            .iter()
            .map(|env| {
                self.bind_once(
                    &args,
                    params,
                    &Scope {
                        env,
                        loop_names: &loop_names,
                        function,
                    },
                )
            })
            .collect();
        // Runs that all lost the same arguments say nothing apart: keep one.
        if runs.windows(2).all(|w| w[0] == w[1]) && runs.first().is_some_and(|r| r.values().any(Option::is_none)) {
            runs.truncate(1);
        }
        runs
    }

    /// The names a loop around the call binds that another loop in its body binds again, which
    /// leaves their value at the call unknown.
    fn shadowed(&self, loops: &[Node<'t>]) -> HashSet<String> {
        let mut out = HashSet::new();
        for &l in loops {
            let targets = self.targets(l);
            let mut todo: Vec<Node> = l.child_by_field_name("body").into_iter().collect();
            while let Some(n) = todo.pop() {
                match n.kind() {
                    "function_definition" | "class_definition" | "lambda" => continue,
                    "for_statement" if !loops.contains(&n) => {
                        out.extend(self.targets(n).into_iter().filter(|t| targets.contains(t)));
                    }
                    _ => {}
                }
                let mut cursor = n.walk();
                todo.extend(n.named_children(&mut cursor));
            }
        }
        out
    }

    fn bind_once(&self, args: &[Node<'t>], params: &[&str], scope: &Scope<'_, 't>) -> BTreeMap<String, Option<String>> {
        self.steps.set(0);
        let mut positional: Vec<Option<Val>> = Vec::new();
        // After a splat that does not resolve, positions are unknown.
        let mut lost = false;
        let mut out = BTreeMap::new();
        for &a in args {
            match a.kind() {
                "keyword_argument" => {
                    if let (Some(k), Some(v)) = (a.child_by_field_name("name"), a.child_by_field_name("value")) {
                        out.insert(self.text(k).into_owned(), self.eval(v, scope, 0));
                    }
                }
                "dictionary_splat" => {}
                "list_splat" if !lost => match named(a).first().and_then(|&v| self.eval(v, scope, 0)?.items()) {
                    Some(items) => positional.extend(items.into_iter().map(Some)),
                    None => lost = true,
                },
                _ if !lost => positional.push(self.eval(a, scope, 0)),
                _ => {}
            }
        }
        // The first positional is the cursor.
        for (i, name) in params.iter().enumerate() {
            match positional.get(i + 1) {
                Some(v) => {
                    out.insert(name.to_string(), v.clone());
                }
                None if lost && !out.contains_key(*name) => {
                    out.insert(name.to_string(), None);
                }
                None => {}
            }
        }
        out.into_iter()
            .filter(|(k, _)| params.contains(&k.as_str()))
            .map(|(k, v)| {
                let s = match v {
                    Some(Val::Str(s)) => Some(s),
                    _ => None,
                };
                (k, s)
            })
            .collect()
    }

    /// The `for` loops whose body holds `node`, inner first, up to the enclosing function.
    fn loops_around(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = Vec::new();
        let mut child = node;
        while let Some(parent) = child.parent() {
            match parent.kind() {
                "function_definition" | "lambda" | "class_definition" => break,
                "for_statement" if parent.child_by_field_name("body") == Some(child) => out.push(parent),
                _ => {}
            }
            child = parent;
        }
        out
    }

    fn targets(&self, for_statement: Node) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(left) = for_statement.child_by_field_name("left") {
            self.pattern_names(left, &mut out);
        }
        out
    }

    fn pattern_names(&self, pattern: Node, out: &mut Vec<String>) {
        match pattern.kind() {
            "identifier" => out.push(self.text(pattern).into_owned()),
            "pattern_list" | "tuple_pattern" | "list_pattern" | "parenthesized_expression" | "list_splat_pattern" => {
                for p in named(pattern) {
                    self.pattern_names(p, out);
                }
            }
            _ => {}
        }
    }

    /// Every environment of `envs` extended with each item the loop runs over, or `None` when
    /// its iterable does not resolve or the runs pass `MAX_RUNS`.
    fn expand(
        &self,
        for_statement: Node<'t>,
        envs: &[Env],
        loop_names: &HashSet<String>,
        function: Node<'t>,
    ) -> Option<Vec<Env>> {
        let left = for_statement.child_by_field_name("left")?;
        let right = for_statement.child_by_field_name("right")?;
        let mut out = Vec::new();
        for env in envs {
            let scope = Scope {
                env,
                loop_names,
                function,
            };
            self.steps.set(0);
            let items = self.eval(right, &scope, 0)?.items()?;
            for item in items {
                let mut next = env.clone();
                self.destructure(left, item, &mut next)?;
                out.push(next);
                if out.len() > MAX_RUNS {
                    return None;
                }
            }
        }
        Some(out)
    }

    fn destructure(&self, pattern: Node, value: Val, env: &mut Env) -> Option<()> {
        match pattern.kind() {
            "identifier" => {
                env.insert(self.text(pattern).into_owned(), value);
                Some(())
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" | "parenthesized_expression" => {
                let patterns = named(pattern);
                let values = value.items()?;
                if patterns.len() != values.len() {
                    return None;
                }
                for (p, v) in patterns.into_iter().zip(values) {
                    self.destructure(p, v, env)?;
                }
                Some(())
            }
            _ => None,
        }
    }

    /// The names `node` reads, and those the single assignments of them read in turn.
    fn free_names(&self, node: Node<'t>, out: &mut HashSet<String>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let mut todo = vec![node];
        while let Some(n) = todo.pop() {
            if n.kind() == "identifier" {
                let parent = n.parent();
                let is_attribute = parent.is_some_and(|p| {
                    matches!(p.kind(), "attribute" | "keyword_argument")
                        && p.child_by_field_name(if p.kind() == "attribute" { "attribute" } else { "name" }) == Some(n)
                });
                let name = self.text(n).into_owned();
                if !is_attribute
                    && out.insert(name.clone())
                    && let Some(value) = self.assigned(n, &name)
                {
                    self.free_names(value, out, depth + 1);
                }
                continue;
            }
            let mut cursor = n.walk();
            todo.extend(n.named_children(&mut cursor));
        }
    }

    /// The value of the one plain assignment to `name` in the innermost scope around `at` that
    /// binds it, or `None` when that scope binds it otherwise or more than once.
    fn assigned(&self, at: Node<'t>, name: &str) -> Option<Node<'t>> {
        if self.bound_inline(at, name) {
            return None;
        }
        let mut scope = Some(at);
        while let Some(s) = scope {
            let s = enclosing_scope(s);
            if let Some(sites) = self.bindings(s).get(name) {
                return match sites.as_slice() {
                    [Binding::Value(value)] => Some(*value),
                    _ => None,
                };
            }
            scope = s.parent();
        }
        None
    }

    /// Whether a lambda or comprehension between `at` and its function binds `name`, which no
    /// scope's bindings record.
    fn bound_inline(&self, at: Node, name: &str) -> bool {
        let mut n = at;
        while let Some(parent) = n.parent() {
            let mut names = Vec::new();
            match parent.kind() {
                "function_definition" => return false,
                "lambda" => {
                    if let Some(params) = parent.child_by_field_name("parameters") {
                        names.extend(
                            named(params)
                                .into_iter()
                                .map(|p| p.child_by_field_name("name").unwrap_or(p))
                                .map(|p| self.text(p).into_owned()),
                        );
                    }
                }
                "list_comprehension" | "set_comprehension" | "dictionary_comprehension" | "generator_expression" => {
                    for clause in named(parent).into_iter().filter(|c| c.kind() == "for_in_clause") {
                        if let Some(left) = clause.child_by_field_name("left") {
                            self.pattern_names(left, &mut names);
                        }
                    }
                }
                _ => {}
            }
            if names.iter().any(|n| n == name) {
                return true;
            }
            n = parent;
        }
        false
    }

    /// The names `node` changes in place or declares `global`, nested definitions included.
    fn writes_in(&self, node: Node) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![node];
        while let Some(n) = todo.pop() {
            match n.kind() {
                "global_statement" => out.extend(self.identifiers(n)),
                "assignment" | "augmented_assignment" => {
                    if let Some(left) = n.child_by_field_name("left").filter(|l| l.kind() != "identifier") {
                        out.extend(self.root_name(left));
                    }
                }
                "delete_statement" => out.extend(self.deleted(n)),
                "call" => out.extend(self.mutated(n)),
                _ => {}
            }
            let mut cursor = n.walk();
            todo.extend(n.named_children(&mut cursor));
        }
        out
    }

    fn deleted(&self, delete_statement: Node) -> Vec<String> {
        let mut out = Vec::new();
        for target in named(delete_statement) {
            match target.kind() {
                "expression_list" => out.extend(named(target).into_iter().filter_map(|t| self.root_name(t))),
                _ => out.extend(self.root_name(target)),
            }
        }
        out
    }

    /// The object a call to a mutating method changes: `xs` for `xs.append(x)`.
    fn mutated(&self, call: Node) -> Option<String> {
        let callee = call
            .child_by_field_name("function")
            .filter(|f| f.kind() == "attribute")?;
        let method = callee.child_by_field_name("attribute")?;
        if !MUTATORS.contains(&self.text(method).as_ref()) {
            return None;
        }
        self.root_name(callee.child_by_field_name("object")?)
    }

    fn identifiers(&self, node: Node) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![node];
        while let Some(n) = todo.pop() {
            if n.kind() == "identifier" {
                out.push(self.text(n).into_owned());
            }
            let mut cursor = n.walk();
            todo.extend(n.named_children(&mut cursor));
        }
        out
    }

    /// The variable a subscript or attribute target hangs from: `d` for `d[k].x`.
    fn root_name(&self, node: Node) -> Option<String> {
        match node.kind() {
            "identifier" => Some(self.text(node).into_owned()),
            "subscript" => self.root_name(node.child_by_field_name("value")?),
            "attribute" => self.root_name(node.child_by_field_name("object")?),
            _ => None,
        }
    }

    fn bindings(&self, scope: Node<'t>) -> Rc<Bindings<'t>> {
        if let Some(b) = self.scopes.borrow().get(&scope.id()) {
            return b.clone();
        }
        let mut found: Bindings<'t> = HashMap::new();
        let mut todo = Vec::new();
        if scope.kind() == "function_definition" {
            if let Some(params) = scope.child_by_field_name("parameters") {
                for p in named(params) {
                    let p = p.child_by_field_name("name").unwrap_or(p);
                    let mut cursor = p.walk();
                    let names: Vec<String> = match p.kind() {
                        "identifier" => vec![self.text(p).into_owned()],
                        _ => p
                            .named_children(&mut cursor)
                            .filter(|c| c.kind() == "identifier")
                            .map(|c| self.text(c).into_owned())
                            .collect(),
                    };
                    for name in names {
                        found.entry(name).or_default().push(Binding::Other);
                    }
                }
            }
            todo.extend(scope.child_by_field_name("body"));
        } else {
            todo.push(scope);
        }
        while let Some(n) = todo.pop() {
            let mut names = Vec::new();
            let mut binding = Binding::Other;
            match n.kind() {
                "function_definition" | "class_definition" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        found
                            .entry(self.text(name).into_owned())
                            .or_default()
                            .push(Binding::Other);
                    }
                    // A helper that changes a module name in place or through `global` leaves
                    // its literal value stale. A local of the same name counts too, which only
                    // costs a partial event.
                    if scope.kind() == "module" {
                        for name in self.writes_in(n) {
                            found.entry(name).or_default().push(Binding::Other);
                        }
                    }
                    continue;
                }
                "lambda"
                | "list_comprehension"
                | "set_comprehension"
                | "dictionary_comprehension"
                | "generator_expression" => continue,
                "assignment" | "augmented_assignment" => {
                    if let Some(left) = n.child_by_field_name("left") {
                        match left.kind() {
                            "identifier" => {
                                let right = n.child_by_field_name("right").filter(|r| r.kind() != "assignment");
                                if let (Some(right), "assignment") = (right, n.kind()) {
                                    binding = Binding::Value(right);
                                }
                                names.push(self.text(left).into_owned());
                            }
                            "subscript" | "attribute" => names.extend(self.root_name(left)),
                            _ => self.pattern_names(left, &mut names),
                        }
                    }
                }
                "for_statement" => {
                    if let Some(left) = n.child_by_field_name("left") {
                        self.pattern_names(left, &mut names);
                    }
                    binding = Binding::Loop;
                }
                "named_expression" => names.extend(n.child_by_field_name("name").and_then(|t| self.root_name(t))),
                "delete_statement" => names.extend(self.deleted(n)),
                "call" => names.extend(self.mutated(n)),
                "import_statement" | "import_from_statement" => {
                    names.extend(self.imported(n).into_iter().map(|(_, alias)| alias));
                }
                "global_statement" | "nonlocal_statement" => {
                    names.extend(named(n).into_iter().filter_map(|t| self.root_name(t)))
                }
                "as_pattern_target" | "case_pattern" => names.extend(self.identifiers(n)),
                "except_clause" => names.extend(n.child_by_field_name("alias").and_then(|a| self.root_name(a))),
                _ => {}
            }
            for name in names {
                found.entry(name).or_default().push(binding);
            }
            let mut cursor = n.walk();
            todo.extend(n.named_children(&mut cursor));
        }
        let found = Rc::new(found);
        self.scopes.borrow_mut().insert(scope.id(), found.clone());
        found
    }

    fn eval(&self, node: Node<'t>, scope: &Scope<'_, 't>, depth: usize) -> Option<Val> {
        self.steps.set(self.steps.get() + 1);
        if depth > MAX_DEPTH || self.steps.get() > MAX_STEPS {
            return None;
        }
        let eval = |n: Node<'t>| self.eval(n, scope, depth + 1);
        match node.kind() {
            "string" => self.string(node, scope, depth),
            "concatenated_string" => {
                let mut out = String::new();
                for part in named(node) {
                    out.push_str(&eval(part)?.display().filter(|_| part.kind() == "string")?);
                }
                Some(Val::Str(out))
            }
            // A leading zero is a Python 2 octal.
            "integer" => {
                let digits = self.text(node).replace('_', "");
                if digits.len() > 1 && digits.starts_with('0') {
                    return None;
                }
                digits.parse().ok().map(Val::Int)
            }
            "true" | "false" | "none" | "float" => Some(Val::Other),
            "identifier" => {
                let name = self.text(node);
                // Loop values hold only in the call's own function, and only for a name nothing
                // there binds besides loops.
                if enclosing_scope(node) == scope.function && !self.bound_inline(node, &name) {
                    if let Some(v) = scope.env.get(name.as_ref()) {
                        let only_loops = self
                            .bindings(scope.function)
                            .get(name.as_ref())
                            .is_some_and(|sites| sites.iter().all(|b| matches!(b, Binding::Loop)));
                        return only_loops.then(|| v.clone());
                    }
                    if scope.loop_names.contains(name.as_ref()) {
                        return None;
                    }
                }
                eval(self.assigned(node, &name)?)
            }
            "parenthesized_expression" => eval(*named(node).first()?),
            "list" | "tuple" | "set" | "expression_list" => {
                let mut items = Vec::new();
                for item in named(node) {
                    if item.kind() == "list_splat" {
                        items.extend(eval(*named(item).first()?)?.items()?);
                    } else {
                        items.push(eval(item)?);
                    }
                }
                if node.kind() == "list" {
                    return Some(Val::List(items));
                }
                if node.kind() == "set" {
                    items = unique(items);
                }
                Some(Val::Seq(items))
            }
            "dictionary" => {
                let mut pairs = Vec::new();
                for pair in named(node) {
                    let key = pair.child_by_field_name("key").filter(|_| pair.kind() == "pair")?;
                    let (key, value) = (eval(key)?, eval(pair.child_by_field_name("value")?)?);
                    // A repeated key keeps its first place and takes the last value.
                    match pairs.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, v)) => *v = value,
                        None => pairs.push((key, value)),
                    }
                }
                Some(Val::Map(pairs))
            }
            "binary_operator" => {
                let left = eval(node.child_by_field_name("left")?)?;
                let right = eval(node.child_by_field_name("right")?)?;
                match (self.text(node.child_by_field_name("operator")?).as_ref(), left, right) {
                    ("+", Val::Str(a), Val::Str(b)) => Some(Val::Str(a + &b)),
                    ("+", Val::Seq(a), Val::Seq(b)) => Some(Val::Seq([a, b].concat())),
                    ("+", Val::List(a), Val::List(b)) => Some(Val::List([a, b].concat())),
                    ("%", Val::Str(template), args) => percent(&template, args).map(Val::Str),
                    _ => None,
                }
            }
            "call" => self.call(node, scope, depth),
            "subscript" => {
                let items = match eval(node.child_by_field_name("value")?)? {
                    Val::Seq(items) | Val::List(items) => items,
                    _ => return None,
                };
                let mut cursor = node.walk();
                let [index] = node
                    .children_by_field_name("subscript", &mut cursor)
                    .collect::<Vec<_>>()
                    .try_into()
                    .ok()?;
                let Val::Int(i) = eval(index)? else {
                    return None;
                };
                let i = if i < 0 {
                    items.len().checked_sub(i.unsigned_abs() as usize)?
                } else {
                    i as usize
                };
                items.into_iter().nth(i)
            }
            "unary_operator" if self.text(node.child_by_field_name("operator")?) == "-" => {
                match eval(node.child_by_field_name("argument")?)? {
                    Val::Int(i) => Some(Val::Int(-i)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// A string literal, an f-string's fields evaluated. Bytes and format specs are unknown.
    fn string(&self, node: Node<'t>, scope: &Scope<'_, 't>, depth: usize) -> Option<Val> {
        let start = node.child(0).filter(|n| n.kind() == "string_start")?;
        let flags = self.text(start).trim_end_matches(['\'', '"']).to_ascii_lowercase();
        if flags.contains('b') {
            return None;
        }
        let (raw, f) = (flags.contains('r'), flags.contains('f'));
        let mut out = String::new();
        let mut cursor = node.walk();
        for part in node.named_children(&mut cursor) {
            match part.kind() {
                "string_content" => {
                    let text = std::str::from_utf8(&self.src[part.byte_range()]).ok()?;
                    let text = if raw { text.to_string() } else { unescape(text)? };
                    out.push_str(&if f {
                        text.replace("{{", "{").replace("}}", "}")
                    } else {
                        text
                    });
                }
                "interpolation" => {
                    let mut cursor = part.walk();
                    let self_documenting = part.children(&mut cursor).any(|c| !c.is_named() && c.kind() == "=");
                    if self_documenting
                        || part.child_by_field_name("format_specifier").is_some()
                        || part.child_by_field_name("type_conversion").is_some()
                    {
                        return None;
                    }
                    let value = self.eval(part.child_by_field_name("expression")?, scope, depth + 1)?;
                    out.push_str(&value.display()?);
                }
                _ => {}
            }
        }
        Some(Val::Str(out))
    }

    fn call(&self, node: Node<'t>, scope: &Scope<'_, 't>, depth: usize) -> Option<Val> {
        let eval = |n: Node<'t>| self.eval(n, scope, depth + 1);
        let callee = node.child_by_field_name("function")?;
        let arguments = node
            .child_by_field_name("arguments")
            .filter(|a| a.kind() == "argument_list")?;
        let args = named(arguments);
        if !args.iter().all(|&a| is_value(a)) {
            return None;
        }
        let values = || args.iter().map(|&a| eval(a)).collect::<Option<Vec<Val>>>();
        if let Some(function) = self.function(callee) {
            let [arg] = values()?.try_into().ok()?;
            return match (function.as_str(), arg) {
                ("expand_braces", Val::Str(s)) => {
                    expand_braces(&s).map(|v| Val::Seq(v.into_iter().map(Val::Str).collect()))
                }
                // Python also splits on these; upgrade-util scripts never hold them.
                ("splitlines", Val::Str(s)) if !s.contains(LINE_BREAKS) => Some(Val::Seq(
                    s.lines()
                        .map(|l| l.split('#').next().unwrap_or_default().trim())
                        .filter(|l| !l.is_empty())
                        .map(|l| Val::Str(l.to_string()))
                        .collect(),
                )),
                _ => None,
            };
        }
        match callee.kind() {
            "attribute" => {
                let object = eval(callee.child_by_field_name("object")?)?;
                let method = self.text(callee.child_by_field_name("attribute")?);
                match (method.as_ref(), object) {
                    ("format", Val::Str(template)) => {
                        let mut positional = Vec::new();
                        let mut named_args = HashMap::new();
                        for &a in &args {
                            if a.kind() == "keyword_argument" {
                                let k = self.text(a.child_by_field_name("name")?).into_owned();
                                named_args.insert(k, eval(a.child_by_field_name("value")?)?.display()?);
                            } else {
                                positional.push(eval(a)?.display()?);
                            }
                        }
                        format(&template, &positional, &named_args).map(Val::Str)
                    }
                    ("split", Val::Str(s)) => {
                        let parts: Vec<&str> = match values()?.as_slice() {
                            [] => s.split_whitespace().collect(),
                            [Val::Str(sep)] if !sep.is_empty() => s.split(sep.as_str()).collect(),
                            [Val::Str(sep), Val::Int(n)] if !sep.is_empty() && *n >= 0 => {
                                s.splitn(*n as usize + 1, sep.as_str()).collect()
                            }
                            _ => return None,
                        };
                        Some(Val::Seq(parts.into_iter().map(|p| Val::Str(p.into())).collect()))
                    }
                    ("replace", Val::Str(s)) => match values()?.as_slice() {
                        [Val::Str(from), Val::Str(to)] => Some(Val::Str(s.replace(from.as_str(), to))),
                        _ => None,
                    },
                    ("lower" | "upper" | "strip", Val::Str(s)) if args.is_empty() => {
                        Some(Val::Str(match method.as_ref() {
                            "lower" => s.to_lowercase(),
                            "upper" => s.to_uppercase(),
                            _ => s.trim().to_string(),
                        }))
                    }
                    ("items" | "iteritems" | "viewitems", Val::Map(pairs)) if args.is_empty() => {
                        Some(Val::Seq(pairs.into_iter().map(|(k, v)| Val::Seq(vec![k, v])).collect()))
                    }
                    ("keys" | "iterkeys" | "viewkeys", Val::Map(pairs)) if args.is_empty() => {
                        Some(Val::Seq(pairs.into_iter().map(|(k, _)| k).collect()))
                    }
                    ("values" | "itervalues" | "viewvalues", Val::Map(pairs)) if args.is_empty() => {
                        Some(Val::Seq(pairs.into_iter().map(|(_, v)| v).collect()))
                    }
                    _ => None,
                }
            }
            "identifier" => {
                let builtin = self.text(callee);
                let values = values()?;
                match (builtin.as_ref(), values.as_slice()) {
                    ("list" | "tuple" | "set" | "reversed", [v]) => {
                        let mut items = v.clone().items()?;
                        match builtin.as_ref() {
                            "reversed" => items.reverse(),
                            "set" => items = unique(items),
                            _ => {}
                        }
                        Some(Val::Seq(items))
                    }
                    ("enumerate", [v]) => Some(Val::Seq(
                        (v.clone().items()?.into_iter().enumerate())
                            .map(|(i, item)| Val::Seq(vec![Val::Int(i as i64), item]))
                            .collect(),
                    )),
                    ("sorted", [v]) => {
                        let mut items: Vec<String> = (v.clone().items()?.into_iter())
                            .map(|i| match i {
                                Val::Str(s) => Some(s),
                                _ => None,
                            })
                            .collect::<Option<_>>()?;
                        items.sort();
                        Some(Val::Seq(items.into_iter().map(Val::Str).collect()))
                    }
                    ("zip", _) if !values.is_empty() => {
                        let lists: Vec<Vec<Val>> = values.into_iter().map(Val::items).collect::<Option<_>>()?;
                        let n = lists.iter().map(Vec::len).min()?;
                        Some(Val::Seq(
                            (0..n)
                                .map(|i| Val::Seq(lists.iter().map(|l| l[i].clone()).collect()))
                                .collect(),
                        ))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// What a lookup sees besides the file: the loop values of this run, the names the loops around
/// the call bind, which mean nothing outside a run, and the function they belong to.
struct Scope<'a, 't> {
    env: &'a Env,
    loop_names: &'a HashSet<String>,
    function: Node<'t>,
}

/// Line breaks of Python's `str.splitlines` besides `\n`.
const LINE_BREAKS: &[char] = &[
    '\r', '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
];

/// An argument that is a plain value, not a keyword, splat or generator.
fn is_value(node: Node) -> bool {
    !matches!(
        node.kind(),
        "list_splat" | "dictionary_splat" | "generator_expression" | "parenthesized_list_splat"
    )
}

/// The items in first-seen order without repeats, as a set runs a loop once per item.
fn unique(items: Vec<Val>) -> Vec<Val> {
    let mut out: Vec<Val> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn enclosing_scope(node: Node) -> Node {
    let mut n = node;
    while let Some(parent) = n.parent() {
        if parent.kind() == "function_definition" {
            return parent;
        }
        n = parent;
    }
    n
}

fn named(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).filter(|n| !n.is_extra()).collect()
}

/// upgrade-util's `expand_braces`: exactly one `{a,b,...}`, holding a comma. Its regex stops at
/// a line break.
fn expand_braces(s: &str) -> Option<Vec<String>> {
    if s.contains('\n') {
        return None;
    }
    let open = s.find('{')?;
    let close = s.find('}')?;
    if s.matches('{').count() != 1 || s.matches('}').count() != 1 || close < open {
        return None;
    }
    let (head, inner, tail) = (&s[..open], &s[open + 1..close], &s[close + 1..]);
    inner
        .contains(',')
        .then(|| inner.split(',').map(|m| format!("{head}{m}{tail}")).collect())
}

/// `template % args` for `%s`, `%d`, `%%` and `%(name)s`.
fn percent(template: &str, args: Val) -> Option<String> {
    let (positional, map) = match args {
        Val::Seq(items) => (items, Vec::new()),
        Val::Map(pairs) => (Vec::new(), pairs),
        single => (vec![single], Vec::new()),
    };
    let mut positional = positional.into_iter();
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let value = if let Some(after) = rest.strip_prefix('(') {
            let (key, after) = after.split_once(')')?;
            rest = after;
            let key = Val::Str(key.to_string());
            map.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())?
        } else if let Some(after) = rest.strip_prefix('%') {
            out.push('%');
            rest = after;
            continue;
        } else {
            positional.next()?
        };
        let conversion = rest.chars().next()?;
        rest = &rest[conversion.len_utf8()..];
        match (conversion, value) {
            ('s', v) => out.push_str(&v.display()?),
            ('d', Val::Int(i)) => out.push_str(&i.to_string()),
            _ => return None,
        }
    }
    out.push_str(rest);
    positional.next().is_none().then_some(out)
}

/// `template.format(...)` for `{}`, `{0}` and `{name}`, without format specs.
fn format(template: &str, positional: &[String], named: &HashMap<String, String>) -> Option<String> {
    let mut out = String::new();
    let (mut auto, mut manual) = (0, false);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut field = String::new();
                loop {
                    match chars.next()? {
                        '}' => break,
                        c => field.push(c),
                    }
                }
                // Python rejects mixing automatic and manual numbering.
                let value = if field.is_empty() {
                    auto += 1;
                    (!manual).then(|| positional.get(auto - 1)).flatten()
                } else if let Ok(i) = field.parse::<usize>() {
                    manual = true;
                    (auto == 0).then(|| positional.get(i)).flatten()
                } else if field.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    named.get(&field)
                } else {
                    None
                };
                out.push_str(value?);
            }
            '}' => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

/// Python's escapes; unknown ones stay as written, as Python leaves them. `None` for `\N{...}`
/// and malformed ones.
fn unescape(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push('\\');
            break;
        };
        let decoded = match e {
            '\n' => continue,
            '\r' => {
                chars.next_if_eq(&'\n');
                continue;
            }
            '\\' | '\'' | '"' => e,
            'a' => '\x07',
            'b' => '\x08',
            'f' => '\x0c',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'v' => '\x0b',
            'x' | 'u' | 'U' => {
                let digits = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let hex: String = (0..digits).map(|_| chars.next()).collect::<Option<_>>()?;
                char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?
            }
            '0'..='7' => {
                let mut octal = e.to_string();
                while octal.len() < 3
                    && let Some(d) = chars.next_if(|d| ('0'..='7').contains(d))
                {
                    octal.push(d);
                }
                char::from_u32(u32::from_str_radix(&octal, 8).ok()?)?
            }
            'N' => return None,
            _ => {
                out.push('\\');
                e
            }
        };
        out.push(decoded);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_decode_like_python() {
        assert_eq!(unescape(r"a\x41\u00e9\101\0\d\n").as_deref(), Some("aAéA\0\\d\n"));
        assert_eq!(unescape(r"\N{BULLET}"), None);
        assert_eq!(unescape(r"\x4"), None);
    }

    #[test]
    fn braces_expand_like_upgrade_util() {
        assert_eq!(expand_braces("a_{this,that}_b").unwrap(), ["a_this_b", "a_that_b"]);
        assert_eq!(expand_braces("a.{,x}").unwrap(), ["a.", "a.x"]);
        assert_eq!(expand_braces("a.{x}"), None);
        assert_eq!(expand_braces("{a,b}.{c,d}"), None);
    }

    #[test]
    fn strings_format() {
        let args = Val::Seq(vec![Val::Str("x".into()), Val::Int(2)]);
        assert_eq!(percent("m.%s_%d%%", args).as_deref(), Some("m.x_2%"));
        assert_eq!(percent("m.%s_%s", Val::Str("x".into())), None);
        let named = HashMap::from([("k".to_string(), "v".to_string())]);
        assert_eq!(
            format("{}.{}.{k}{{}}", &["a".into(), "b".into()], &named).as_deref(),
            Some("a.b.v{}")
        );
        assert_eq!(
            format("{1}.{0}", &["a".into(), "b".into()], &named).as_deref(),
            Some("b.a")
        );
        assert_eq!(format("{}.{0}", &["a".into()], &named), None);
        assert_eq!(format("{0", &["a".into()], &named), None);
        assert_eq!(format("{:02}", &["a".into()], &named), None);
        assert_eq!(percent("%é", Val::Str("x".into())), None);
    }
}
