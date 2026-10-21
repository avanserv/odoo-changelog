//! `__manifest__.py` and `__openerp__.py` as one `module` symbol, read as a Python literal and
//! never run, like Odoo's own `ast.literal_eval`.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;
use tree_sitter::Node;

use crate::python::value::{canonical, value};
use crate::python::{parse, python2};
use crate::syntax::{named_with_errors, syntax_warnings};
use crate::{Extractor, Span, Symbol, SymbolTable};

const MODULE: &str = "module";

pub struct Manifest;

impl Extractor for Manifest {
    fn name(&self) -> &'static str {
        "manifest"
    }

    /// Bump with any change to the output, including changes to `python::value`,
    /// `python::python2` or `syntax`, and a tree-sitter or tree-sitter-python upgrade.
    fn version(&self) -> u32 {
        1
    }

    /// A manifest in a directory Odoo accepts as a module name (`MODULE_NAME_RE`, ASCII only).
    fn claims(&self, path: &str) -> bool {
        let Some((dir, file)) = path.rsplit_once('/') else {
            return false;
        };
        let module = dir.rsplit('/').next().unwrap_or(dir);
        matches!(file, "__manifest__.py" | "__openerp__.py")
            && !module.is_empty()
            && module.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }

    /// Every top-level key as written, without Odoo's defaults, which differ between versions.
    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        let tree = parse(blob)?;
        let root = tree.root_node();
        let mut warnings = syntax_warnings(root, |n| python2(n, blob));
        let (dict, ignored) = top_level(root);
        let mut table = SymbolTable::default();
        match dict {
            Some(dict) => {
                for statement in ignored {
                    let line = statement.start_position().row + 1;
                    let warning = format!(
                        "line {line}: {} outside the manifest dictionary ignored",
                        statement.kind()
                    );
                    warnings.push((statement.start_byte(), warning));
                }
                let attrs = attrs(dict, blob, &mut warnings);
                table.insert(Symbol {
                    kind: MODULE.into(),
                    name: String::new(),
                    line: dict.start_position().row as u32 + 1,
                    span: Span {
                        start: dict.start_byte() as u32,
                        end: dict.end_byte() as u32,
                    },
                    attrs,
                });
            }
            None => warnings.push((0, "line 1: no manifest dictionary".into())),
        }
        warnings.sort_by_key(|(at, _)| *at);
        for (_, warning) in warnings {
            table.warn(warning);
        }
        Ok(table)
    }

    /// The module is the manifest's directory, the name `depends` lists. `module_of` would give
    /// the outer module for a test module nested in another one's `tests/`.
    fn qualify(&self, path: &str, _: &Symbol) -> String {
        self.module(path).unwrap_or_default().to_string()
    }

    fn module<'a>(&self, path: &'a str) -> Option<&'a str> {
        path.rsplit('/').nth(1)
    }
}

/// The first top-level dictionary, closed or not, and the statements around it. Statements
/// inside an ERROR node are left out: the syntax warning already covers them.
fn top_level(root: Node) -> (Option<Node>, Vec<Node>) {
    let (mut dict, mut ignored) = (None, Vec::new());
    let mut todo: Vec<(Node, bool)> = named_with_errors(root).into_iter().rev().map(|n| (n, false)).collect();
    while let Some((node, in_error)) = todo.pop() {
        match dictionary(node) {
            Some(d) if dict.is_none() => dict = Some(d),
            None if node.is_error() => todo.extend(named_with_errors(node).into_iter().rev().map(|n| (n, true))),
            _ if !in_error => ignored.push(node),
            _ => {}
        }
    }
    (dict, ignored)
}

/// The dictionary `node` is, through statement and parentheses, or an ERROR that opens with `{`,
/// which is how tree-sitter reads a dictionary missing its closing brace.
fn dictionary(mut node: Node) -> Option<Node> {
    loop {
        match node.kind() {
            "dictionary" => return Some(node),
            "expression_statement" | "parenthesized_expression" => match named_with_errors(node)[..] {
                [only] => node = only,
                _ => return None,
            },
            _ if node.is_error() && node.child(0).is_some_and(|c| c.kind() == "{") => return Some(node),
            _ => return None,
        }
    }
}

/// The dictionary's pairs, the last of a key winning as in Python. Entries inside an ERROR node
/// that are no pair are skipped without a warning of their own.
fn attrs(dict: Node, src: &[u8], warnings: &mut Vec<(usize, String)>) -> BTreeMap<String, Value> {
    let mut attrs = BTreeMap::new();
    let mut todo: Vec<(Node, bool)> = named_with_errors(dict).into_iter().rev().map(|n| (n, false)).collect();
    while let Some((entry, in_error)) = todo.pop() {
        let line = entry.start_position().row + 1;
        let mut warn = |warning: String| warnings.push((entry.start_byte(), format!("line {line}: {warning}")));
        if entry.is_error() {
            todo.extend(named_with_errors(entry).into_iter().rev().map(|n| (n, true)));
            continue;
        }
        let pair = (entry.kind() == "pair")
            .then(|| entry.child_by_field_name("key").zip(entry.child_by_field_name("value")))
            .flatten();
        let Some((key, val)) = pair else {
            if !in_error {
                warn(format!("{} is not a key-value pair; ignored", canonical(entry, src)));
            }
            continue;
        };
        if val.is_missing() {
            continue;
        }
        match value(key, src) {
            Value::String(key) => {
                attrs.insert(key, value(val, src));
            }
            _ if in_error => {}
            _ => warn(format!("key {} is not a string; ignored", canonical(key, src))),
        }
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_manifests_of_module_directories() {
        for path in [
            "addons/sale/__manifest__.py",
            "odoo/addons/base/__openerp__.py",
            "sale_subscription/__manifest__.py",
            "odoo/addons/test_base/tests/test_modules/test_install_addons/test_install_auto/__manifest__.py",
        ] {
            assert!(Manifest.claims(path), "{path}");
        }
        for path in [
            "__manifest__.py",
            "addons/sale/__manifest__.pyc",
            "addons/sale/models/sale_order.py",
            "addons/sale-x/__manifest__.py",
            "addons/sale/__init__.py",
        ] {
            assert!(!Manifest.claims(path), "{path}");
        }
    }

    #[test]
    fn qualname_is_the_manifest_directory() {
        let symbol = Symbol {
            kind: MODULE.into(),
            name: String::new(),
            line: 1,
            span: Span::default(),
            attrs: BTreeMap::new(),
        };
        for (path, module) in [
            ("addons/sale/__manifest__.py", "sale"),
            ("odoo/addons/base/__openerp__.py", "base"),
            ("sale_subscription/__manifest__.py", "sale_subscription"),
            (
                "odoo/addons/test_base/tests/test_modules/test_install_addons/test_install_auto/__manifest__.py",
                "test_install_auto",
            ),
        ] {
            assert_eq!(Manifest.qualify(path, &symbol), module, "{path}");
        }
    }
}
