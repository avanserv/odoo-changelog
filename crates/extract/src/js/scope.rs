//! What the names of one scope stand for, the program or an `odoo.define` body, so that a
//! reference reads as the module member it names.

use std::collections::BTreeMap;

use tree_sitter::Node;

use super::value::string_value;
use crate::hash::digest;
use crate::syntax::{named, text};

/// The most `.member` segments a reference or a category path is read through, and the most
/// bytes of text a binding, a scope or a reference holds: a reference is read through a longer
/// chain as an expression, and a longer text is stored as `bounded` makes it. A symbol repeats
/// such text, so the bound keeps one file's output linear in its size.
pub(super) const MAX_SEGMENTS: usize = 64;
pub(super) const MAX_TEXT: usize = 256;

/// `text`, or past `MAX_TEXT` bytes `~<hash>` of it followed by its last `#`, `.` or `/`
/// segment when that is short, for name text a symbol may repeat. The segment keeps what a
/// name ends with, such as `.registry` or `#patch`, which the reader recognizes names by.
pub(super) fn bounded(text: String) -> String {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    let tail = text.rfind(['#', '.', '/']).map_or("", |at| &text[at..]);
    let tail = if tail.len() <= MAX_TAIL { tail } else { "" };
    format!("~{}{tail}", digest(&hasher))
}

/// The longest last segment `bounded` keeps.
const MAX_TAIL: usize = 64;

/// A reference's target, `<module>#<member path>` or `<module>`, or the canonical text of a
/// global's member, which no module names.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Resolved {
    Ref(String),
    Expr(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Binding {
    /// A member of a module, through an import, a require or a destructured require.
    Member { module: String, path: String },
    /// A whole module: `ns` of `import * as ns` (`S#*`), or a required one (`S`).
    Module { module: String, namespace: bool },
    /// A declaration of this scope.
    Local,
    /// A name destructured from a global, such as `Component` from `owl`.
    Global(String),
    /// A registry category, its path from the root registry.
    Category(Vec<String>),
    /// A name an inner block, loop or `catch` declares, which names nothing the file exports.
    Block,
}

/// The bindings block names hid, to put back when the block ends.
pub(super) type Shadowed = Vec<(String, Option<Binding>)>;

pub(super) struct Scope {
    /// `""` for an ES module, whose members the path names, or the `odoo.define` name.
    prefix: String,
    bindings: BTreeMap<String, Binding>,
}

impl Scope {
    /// `prefix` is at most `MAX_TEXT` bytes, as every text the scope stores.
    pub fn new(prefix: String) -> Self {
        Scope {
            prefix,
            bindings: BTreeMap::new(),
        }
    }

    pub fn bind(&mut self, name: impl Into<String>, binding: Binding) {
        self.bindings.insert(name.into(), binding);
    }

    pub fn get(&self, name: &str) -> Option<&Binding> {
        self.bindings.get(name)
    }

    /// The reference to the local `name`.
    pub fn local(&self, name: &str) -> String {
        format!("{}#{name}", self.prefix)
    }

    /// The target of an identifier, a `require("m")` call or a chain of `.member` accesses on
    /// one, or `None` when its root is unbound or the chain holds anything else.
    pub fn resolve(&self, node: Node, src: &[u8]) -> Option<Resolved> {
        self.resolve_unbounded(node, src).map(|r| match r {
            Resolved::Ref(r) => Resolved::Ref(bounded(r)),
            Resolved::Expr(e) => Resolved::Expr(bounded(e)),
        })
    }

    fn resolve_unbounded(&self, node: Node, src: &[u8]) -> Option<Resolved> {
        let (root, path) = chain(node, src)?;
        let joined = |base: &str| {
            let rest = path.join(".");
            match (base.is_empty(), rest.is_empty()) {
                (_, true) => base.to_string(),
                (true, false) => rest,
                (false, false) => format!("{base}.{rest}"),
            }
        };
        let module_member = |module: &str, member: String| match member.is_empty() {
            true => module.to_string(),
            false => format!("{module}#{member}"),
        };
        Some(match root {
            Root::Require(module) => Resolved::Ref(module_member(&module, joined(""))),
            Root::Name(name) => match self.bindings.get(&name)? {
                Binding::Member { module, path } => Resolved::Ref(module_member(module, joined(path))),
                Binding::Module { module, namespace } => {
                    let member = joined("");
                    match (member.is_empty(), namespace) {
                        (true, true) => Resolved::Ref(format!("{module}#*")),
                        _ => Resolved::Ref(module_member(module, member)),
                    }
                }
                Binding::Local => Resolved::Ref(format!("{}#{}", self.prefix, joined(&name))),
                Binding::Global(expr) => Resolved::Expr(joined(expr)),
                Binding::Category(_) | Binding::Block => return None,
            },
        })
    }

    /// Binds `names` as block locals until `restore` gets the bindings this returns.
    pub fn shadow(&mut self, names: Vec<String>) -> Shadowed {
        names
            .into_iter()
            .map(|name| {
                let previous = self.bindings.insert(name.clone(), Binding::Block);
                (name, previous)
            })
            .collect()
    }

    pub fn restore(&mut self, saved: Shadowed) {
        for (name, previous) in saved.into_iter().rev() {
            match previous {
                Some(binding) => self.bindings.insert(name, binding),
                None => self.bindings.remove(&name),
            };
        }
    }

    /// The binding a declarator takes for a value that is an import, a require or a global, or
    /// a member of one, so that the declared name resolves to what it aliases.
    pub fn alias(&self, node: Node, src: &[u8]) -> Option<Binding> {
        let (root, path) = chain(node, src)?;
        let base = match root {
            Root::Require(module) => Binding::Module {
                module,
                namespace: false,
            },
            Root::Name(name) => match self.bindings.get(&name)? {
                Binding::Local | Binding::Category(_) | Binding::Block => return None,
                b => b.clone(),
            },
        };
        if path.is_empty() {
            return Some(base);
        }
        member_of(&base, &path.join("."))
    }

    /// Whether `node` is a name or a `.member` chain on a name this scope does not bind.
    pub fn is_global(&self, node: Node, src: &[u8]) -> bool {
        matches!(chain(node, src), Some((Root::Name(name), _)) if !self.bindings.contains_key(&name))
    }

    /// The registry category `node` names: a bound category, or `registry.category(..)`, chained,
    /// on the root registry, an unbound `registry` or an imported one. Legacy registries are the
    /// caller's to recognize.
    pub fn category(&self, node: Node, src: &[u8], label: impl Fn(Node) -> String) -> Option<Vec<String>> {
        let mut names = Vec::new();
        let mut n = node;
        while let Some((object, arg)) = method_call(n, "category", src).filter(|(_, args)| args.len() == 1) {
            if names.len() == MAX_SEGMENTS {
                return None;
            }
            names.push(label(arg[0]));
            n = object;
        }
        names.reverse();
        let base = match n.kind() {
            "identifier" => match self.bindings.get(text(n, src).as_ref()) {
                Some(Binding::Category(base)) => base.clone(),
                None if text(n, src) == "registry" => Vec::new(),
                Some(Binding::Member { path, .. }) if path == "registry" => Vec::new(),
                _ => return None,
            },
            _ => return None,
        };
        let full = [base, names].concat();
        (!full.is_empty() && full.len() <= MAX_SEGMENTS).then_some(full)
    }
}

/// What `<base>.<key>` stands for, when `base` names a module or a global.
pub(super) fn member_of(base: &Binding, key: &str) -> Option<Binding> {
    Some(match base {
        Binding::Module { module, .. } => Binding::Member {
            module: module.clone(),
            path: bounded(key.into()),
        },
        Binding::Member { module, path } => Binding::Member {
            module: module.clone(),
            path: bounded(format!("{path}.{key}")),
        },
        Binding::Global(expr) => Binding::Global(bounded(format!("{expr}.{key}"))),
        Binding::Local | Binding::Category(_) | Binding::Block => return None,
    })
}

enum Root {
    Name(String),
    Require(String),
}

/// The root of a `.member` chain and the member names after it, outermost last, within
/// `MAX_SEGMENTS` and `MAX_TEXT`.
fn chain(node: Node, src: &[u8]) -> Option<(Root, Vec<String>)> {
    let mut path = Vec::new();
    let mut size = 0;
    let mut n = node;
    loop {
        match n.kind() {
            "member_expression" => {
                let property = n.child_by_field_name("property")?;
                if property.kind() != "property_identifier" || n.child_by_field_name("optional_chain").is_some() {
                    return None;
                }
                size += property.byte_range().len() + 1;
                if path.len() == MAX_SEGMENTS || size > MAX_TEXT {
                    return None;
                }
                path.push(text(property, src).into_owned());
                n = n.child_by_field_name("object")?;
            }
            "identifier" | "shorthand_property_identifier" => {
                if size + n.byte_range().len() > MAX_TEXT {
                    return None;
                }
                path.reverse();
                return Some((Root::Name(text(n, src).into_owned()), path));
            }
            "call_expression" => {
                path.reverse();
                let module = require(n, src).filter(|m| size + m.len() <= MAX_TEXT)?;
                return Some((Root::Require(module), path));
            }
            "parenthesized_expression" if named(n).len() == 1 => n = named(n)[0],
            _ => return None,
        }
    }
}

/// `m` for the call `require("m")`.
pub(super) fn require(call: Node, src: &[u8]) -> Option<String> {
    let callee = call.child_by_field_name("function")?;
    let args = call.child_by_field_name("arguments").map(named)?;
    match args[..] {
        [arg] if callee.kind() == "identifier" && text(callee, src) == "require" => string_value(arg, src),
        _ => None,
    }
}

/// The object and the arguments of the call `<object>.<method>(...)`.
pub(super) fn method_call<'t>(call: Node<'t>, method: &str, src: &[u8]) -> Option<(Node<'t>, Vec<Node<'t>>)> {
    let callee = call
        .child_by_field_name("function")
        .filter(|_| call.kind() == "call_expression")?;
    let property = callee
        .child_by_field_name("property")
        .filter(|_| callee.kind() == "member_expression")?;
    let args = call
        .child_by_field_name("arguments")
        .filter(|a| a.kind() == "arguments")?;
    if text(property, src) != method {
        return None;
    }
    Some((callee.child_by_field_name("object")?, named(args)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse;

    /// The resolution of the last expression statement of `src`, after binding `bindings`.
    fn resolve(bindings: &[(&str, Binding)], prefix: &str, expr: &str) -> Option<Resolved> {
        let tree = parse(expr.as_bytes(), &tree_sitter_javascript::LANGUAGE.into()).unwrap();
        let statement = named(tree.root_node())[0];
        let mut scope = Scope::new(prefix.into());
        for (name, b) in bindings {
            scope.bind(*name, b.clone());
        }
        scope.resolve(named(statement)[0], expr.as_bytes())
    }

    fn reference(s: &str) -> Option<Resolved> {
        Some(Resolved::Ref(s.into()))
    }

    #[test]
    fn every_binding_resolves() {
        let member = |module: &str, path: &str| Binding::Member {
            module: module.into(),
            path: path.into(),
        };
        let module = |module: &str, namespace: bool| Binding::Module {
            module: module.into(),
            namespace,
        };
        let bindings = [
            ("B", member("./b", "A")),
            ("d", member("@x/d", "default")),
            ("ns", module("@x/ns", true)),
            ("core", module("web.core", false)),
            ("Foo", Binding::Local),
            ("Component", Binding::Global("owl.Component".into())),
            ("cat", Binding::Category(vec!["fields".into()])),
        ];
        for (expr, prefix, resolved) in [
            ("B", "", reference("./b#A")),
            ("B.prototype", "", reference("./b#A.prototype")),
            ("d.x", "", reference("@x/d#default.x")),
            ("ns", "", reference("@x/ns#*")),
            ("ns.Foo", "", reference("@x/ns#Foo")),
            ("core", "", reference("web.core")),
            ("core.action_registry", "", reference("web.core#action_registry")),
            ("(core).Class", "", reference("web.core#Class")),
            ("Foo", "", reference("#Foo")),
            ("Foo.prototype", "web.x", reference("web.x#Foo.prototype")),
            ("require('web.Widget')", "", reference("web.Widget")),
            ("require('web.core').bus", "", reference("web.core#bus")),
            ("Component", "", Some(Resolved::Expr("owl.Component".into()))),
            ("unbound.x", "", None),
            ("cat", "", None),
            ("Foo?.x", "", None),
            ("Foo[0]", "", None),
            ("this.x", "", None),
        ] {
            assert_eq!(resolve(&bindings, prefix, expr), resolved, "{expr}");
        }
    }
}
