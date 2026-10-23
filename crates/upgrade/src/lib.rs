//! Reads Odoo's upgrade scripts into the renames, moves, merges and removals their `util` calls
//! declare. Never sees git or SQLite: what it returns depends on a script's path and bytes alone,
//! and changing that bumps `VERSION`.

mod eval;
mod script;

use anyhow::{Result, anyhow};
use serde::Serialize;
use tree_sitter::{Node, Parser};

pub use script::{Script, folder_version, script};

/// Bumped whenever `parse` or `script` returns something else for the same input, which makes
/// the next `upgrade` run read every script again.
pub const VERSION: u32 = 1;

/// One rename, move, merge or removal a call declares. A call that a loop or a brace expansion
/// runs several times gives one event per run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    pub line: usize,
    pub col: usize,
    /// Numbers the events of one call.
    pub seq: usize,
    /// The upgrade-util function called, `delete_model` kept apart from `remove_model`.
    pub call: &'static str,
    /// `field`, `model`, `xmlid`, `view` or `module`.
    pub kind: &'static str,
    /// `renamed`, `moved`, `merged` or `removed`.
    pub op: &'static str,
    /// The model of a field.
    pub model: Option<String>,
    /// The field, model, xmlid or module before the call; the one moved or removed otherwise.
    pub old: Option<String>,
    /// The name after a rename or merge, or the moved name again.
    pub new: Option<String>,
    pub old_module: Option<String>,
    pub new_module: Option<String>,
    /// Some field the call sets did not resolve to a string, and is `None`.
    pub partial: bool,
    /// The call's text, for a partial event only.
    pub source: Option<String>,
}

/// Which parameter fills each field of an event; several names are alternatives, the first one
/// the call passes wins.
struct Spec {
    name: &'static str,
    /// After `cr`, in order.
    params: &'static [&'static str],
    kind: &'static str,
    op: &'static str,
    model: &'static [&'static str],
    old: &'static [&'static str],
    new: &'static [&'static str],
    old_module: &'static [&'static str],
    new_module: &'static [&'static str],
}

const BASE: Spec = Spec {
    name: "",
    params: &[],
    kind: "",
    op: "",
    model: &[],
    old: &[],
    new: &[],
    old_module: &[],
    new_module: &[],
};

/// Signatures as upgrade-util declares them, as of 2026-10-23.
const SPECS: &[Spec] = &[
    Spec {
        name: "rename_field",
        params: &["model", "old", "new"],
        kind: "field",
        op: "renamed",
        model: &["model"],
        old: &["old"],
        new: &["new"],
        ..BASE
    },
    Spec {
        name: "rename_model",
        params: &["old", "new"],
        kind: "model",
        op: "renamed",
        old: &["old"],
        new: &["new"],
        ..BASE
    },
    Spec {
        name: "rename_xmlid",
        params: &["old", "new"],
        kind: "xmlid",
        op: "renamed",
        old: &["old"],
        new: &["new"],
        ..BASE
    },
    Spec {
        name: "rename_module",
        params: &["old", "new"],
        kind: "module",
        op: "renamed",
        old: &["old"],
        new: &["new"],
        ..BASE
    },
    Spec {
        name: "merge_module",
        params: &["old", "into"],
        kind: "module",
        op: "merged",
        old: &["old"],
        new: &["into"],
        ..BASE
    },
    Spec {
        name: "move_field_to_module",
        params: &["model", "fieldname", "old_module", "new_module"],
        kind: "field",
        op: "moved",
        model: &["model"],
        old: &["fieldname"],
        new: &["fieldname"],
        old_module: &["old_module"],
        new_module: &["new_module"],
    },
    Spec {
        name: "move_model",
        params: &["model", "from_module", "to_module"],
        kind: "model",
        op: "moved",
        old: &["model"],
        new: &["model"],
        old_module: &["from_module"],
        new_module: &["to_module"],
        ..BASE
    },
    Spec {
        name: "merge_model",
        params: &["source", "target"],
        kind: "model",
        op: "merged",
        old: &["source"],
        new: &["target"],
        ..BASE
    },
    Spec {
        name: "remove_field",
        params: &["model", "fieldname"],
        kind: "field",
        op: "removed",
        model: &["model"],
        old: &["fieldname"],
        ..BASE
    },
    Spec {
        name: "remove_model",
        params: &["model"],
        kind: "model",
        op: "removed",
        old: &["model"],
        ..BASE
    },
    Spec {
        name: "delete_model",
        params: &["model"],
        kind: "model",
        op: "removed",
        old: &["model"],
        ..BASE
    },
    // `view_id` is a database id, not a name: a call passing only that stays partial.
    Spec {
        name: "remove_view",
        params: &["xml_id", "view_id", "silent", "key"],
        kind: "view",
        op: "removed",
        old: &["xml_id", "key"],
        ..BASE
    },
    Spec {
        name: "remove_record",
        params: &["name"],
        kind: "xmlid",
        op: "removed",
        old: &["name"],
        ..BASE
    },
    Spec {
        name: "remove_module",
        params: &["module"],
        kind: "module",
        op: "removed",
        old: &["module"],
        ..BASE
    },
];

fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

/// The events of every recognised `util` call in a script, in source order. A file with syntax
/// errors still gives the calls tree-sitter parses around them.
pub fn parse(src: &[u8]) -> Result<Vec<Event>> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no tree"))?;
    let file = eval::File::new(tree.root_node(), src);
    let mut events = Vec::new();
    let mut todo = vec![tree.root_node()];
    while let Some(node) = todo.pop() {
        if let Some(spec) = file.callee(node).as_deref().and_then(spec) {
            events.extend(call_events(&file, node, spec));
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        todo.extend(children.into_iter().rev());
    }
    Ok(events)
}

fn call_events<'t>(file: &eval::File<'t, '_>, call: Node<'t>, spec: &Spec) -> Vec<Event> {
    let pos = call.start_position();
    let source = file.text(call).into_owned();
    file.bind(call, spec.params)
        .into_iter()
        .enumerate()
        .map(|(seq, args)| {
            let mut partial = false;
            let mut field = |names: &[&str]| -> Option<String> {
                if names.is_empty() {
                    return None;
                }
                let value = names.iter().find_map(|n| args.get(*n)).and_then(|v| v.clone());
                partial |= value.is_none();
                value
            };
            let (model, old, new) = (field(spec.model), field(spec.old), field(spec.new));
            let (old_module, new_module) = (field(spec.old_module), field(spec.new_module));
            Event {
                line: pos.row + 1,
                col: pos.column + 1,
                seq,
                call: spec.name,
                kind: spec.kind,
                op: spec.op,
                model,
                old,
                new,
                old_module,
                new_module,
                partial,
                source: partial.then(|| source.clone()),
            }
        })
        .collect()
}
