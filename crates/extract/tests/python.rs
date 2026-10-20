//! The Python extractor on hand-written fixtures. Each `fixtures/python/<name>.py` has its
//! expected output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::path::{Path, PathBuf};

use common::{check, render};
use odoo_changelog_extract::{Extractor, Python, Symbol, SymbolId, SymbolTable, delta};
use serde_json::{Value, json};

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "py"))
        .collect();
    files.sort();
    files
}

/// Where a fixture is qualified: a controller module, or a model file of the `demo` module.
fn fixture_path(file: &Path) -> String {
    match file.file_stem().unwrap().to_str().unwrap() {
        "controllers" => "addons/demo/controllers/main.py".into(),
        stem => format!("addons/demo/models/{stem}.py"),
    }
}

#[test]
fn fixtures_match_expected_output() {
    let files = fixtures();
    assert!(files.len() >= 9, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        check(&file.with_extension("json"), &render(&fixture_path(&file), &blob));
    }
}

fn extract(src: &str) -> SymbolTable {
    Python.extract(src.as_bytes()).unwrap()
}

fn find<'a>(table: &'a SymbolTable, kind: &str, name: &str) -> &'a Symbol {
    table
        .iter()
        .find(|s| s.kind == kind && s.name == name)
        .unwrap_or_else(|| panic!("no {kind} {name} in {table:?}"))
}

/// The `body` hash of `demo.m#<method>` in `class M` around `def`.
fn body(def: &str, method: &str) -> Value {
    let src = format!("class M(models.Model):\n    _name = 'demo.m'\n\n{def}");
    find(&extract(&src), "method", &format!("demo.m#{method}")).attrs["body"].clone()
}

const INHERITED: &str = "class SaleOrder(models.Model):\n    _inherit = 'sale.order'\n    foo = fields.Char()\n";

#[test]
fn inherited_field_belongs_to_the_inherited_model_and_module() {
    let table = extract(INHERITED);
    let path = "addons/sale_ext/models/sale_order.py";
    let fields: Vec<&Symbol> = table.iter().filter(|s| s.kind == "field").collect();
    assert_eq!(fields.len(), 1);
    assert_eq!(Python.qualify(path, fields[0]), "sale.order#foo@sale_ext");
    assert_eq!(fields[0].attrs["type"], "Char");
    assert_eq!(&INHERITED.as_bytes()[fields[0].span.range()], b"foo");

    let model = find(&table, "model", "sale.order");
    assert_eq!(Python.qualify(path, model), "sale.order@sale_ext");
    assert_eq!(model.attrs["_inherit"], json!(["sale.order"]));
    assert!(!model.attrs.contains_key("_name"));
}

#[test]
fn depends_decorator_holds_both_paths() {
    let src =
        "class M(models.Model):\n    _name = 'm'\n\n    @api.depends('a', 'b.c')\n    def f(self):\n        pass\n";
    let table = extract(src);
    let method = find(&table, "method", "m#f");
    assert_eq!(
        method.attrs["decorators"],
        json!([{"name": "api.depends", "args": ["a", "b.c"]}])
    );
}

#[test]
fn field_kwargs_are_normalized() {
    let field = |decl: &str| {
        let src = format!("class M(models.Model):\n    _name = 'm'\n    f = {decl}\n");
        find(&extract(&src), "field", "m#f").attrs.clone()
    };
    assert_eq!(field("fields.Char('X')"), field("fields.Char(string=\"X\")"));
    assert_eq!(
        field("fields.Char(help='a b')"),
        field("fields.Char(help=\"a \"\n        'b')")
    );
    assert_eq!(
        field("fields.Many2one('res.company', default=lambda self: self.env.company)")["args"],
        json!({"default": {"expr": "lambda self:self.env.company"}})
    );
}

/// Each is the same method as the first, as far as rename scoring is concerned.
#[test]
fn body_hash_ignores_layout_comments_docstrings_and_local_names() {
    let base = "    def f(self, vals):\n        total = vals['a'] + 1\n        return self.g(total, key=\"x\")\n";
    let same = [
        "    def f(self, vals):\n\n            total = vals['a'] \\\n                + 1  # note\n            return self.g(\n                total,\n                key='x',\n            )\n",
        "    def f(self, vals):\n        \"\"\"Docstring.\"\"\"\n        total = vals[\"\"\"a\"\"\"] + 1\n        return self.g(total, key='x')\n",
        "    def f(self, values):\n        sum_ = values['a'] + 1\n        return self.g(sum_, key='x')\n",
    ];
    for def in same {
        assert_eq!(body(base, "f"), body(def, "f"), "{def}");
    }
}

#[test]
fn body_hash_sees_real_changes() {
    let base = "    def f(self, a, b):\n        if a:\n            b = a\n        return a - b\n";
    let changed = [
        "    def f(self, a, b):\n        if a:\n            b = a\n        return b - a\n",
        "    def f(self, a, b):\n        if a:\n            b = a\n        a.touch()\n        return a - b\n",
        "    def f(self, a, b):\n        if a:\n            b = a\n            return a - b\n",
        "    def f(self, a, b):\n        if a:\n            b = a\n        return a - c\n",
    ];
    for def in changed {
        assert_ne!(body(base, "f"), body(def, "f"), "{def}");
    }
}

#[test]
fn renamed_override_hashes_alike() {
    let write = body(
        "    def write(self, vals):\n        return super().write(vals)\n",
        "write",
    );
    let update = body(
        "    def update(self, values):\n        return super().update(values)\n",
        "update",
    );
    assert_eq!(write, update);
}

#[test]
fn python2_and_syntax_errors_give_a_partial_table_and_a_warning() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
    for (file, before, after) in [
        ("python2.py", "demo.legacy#name", "demo.legacy#after"),
        ("syntax_error.py", "demo.broken#first", "demo.after#after"),
    ] {
        let table = Python.extract(&std::fs::read(dir.join(file)).unwrap()).unwrap();
        assert!(!table.warnings().is_empty(), "{file}");
        for name in [before, after] {
            assert!(table.iter().any(|s| s.name == name), "{file}: no {name}");
        }
    }
}

#[test]
fn every_symbol_spans_its_name() {
    for file in fixtures() {
        let blob = std::fs::read(&file).unwrap();
        for symbol in Python.extract(&blob).unwrap().iter() {
            let name = symbol.name.as_str();
            let last = match symbol.kind.as_str() {
                "selection" => name.split_once(':').unwrap().1,
                "model" => name.split('[').next().unwrap(),
                "class" | "function" | "route" => name.rsplit('.').next().unwrap(),
                _ => name.split_once('#').unwrap().1,
            };
            let spanned = String::from_utf8_lossy(&blob[symbol.span.range()]);
            assert_eq!(spanned, last, "{}: {symbol:?}", file.display());
            let line = blob[..symbol.span.start as usize]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            assert_eq!(symbol.line as usize, line, "{}: {symbol:?}", file.display());
        }
    }
}

#[test]
fn inserting_lines_above_gives_no_event() {
    let path = "addons/sale_ext/models/sale_order.py";
    let old = extract(INHERITED);
    let new = extract(&format!("# A comment.\nimport logging\n\n_logger = 1\n\n{INHERITED}"));
    assert_ne!(old, new);
    assert!(delta(&Python, Some((path, &old)), Some((path, &new))).is_empty());
}

#[test]
fn module_decides_model_scope_ids_and_the_file_python_scope_ones() {
    let src = format!("{INHERITED}\n\ndef helper():\n    return 1\n");
    let table = extract(&src);
    let field = find(&table, "field", "sale.order#foo");
    let in_a = Python.qualify("addons/a/models/sale.py", field);
    let in_b = Python.qualify("addons/b/models/sale.py", field);
    assert_ne!(SymbolId::new("field", &in_a), SymbolId::new("field", &in_b));

    let moved = delta(
        &Python,
        Some(("addons/a/models/sale.py", &table)),
        Some(("addons/a/models/order.py", &table)),
    );
    let ops: Vec<(&str, &str)> = moved.iter().map(|d| (d.qualname.as_str(), d.op.as_str())).collect();
    assert_eq!(
        ops,
        [
            ("sale.order#foo@a", "moved"),
            ("odoo.addons.a.models.order.helper", "added"),
            ("odoo.addons.a.models.sale.helper", "removed"),
            ("sale.order@a", "moved"),
        ]
    );
}

#[test]
fn claims_importable_python_files_only() {
    for path in [
        "addons/sale/models/sale_order.py",
        "addons/sale/__init__.py",
        "addons/sale/tests/common.py",
        "odoo/models.py",
        "setup.py",
    ] {
        assert!(Python.claims(path), "{path}");
    }
    for path in [
        "addons/sale/__manifest__.py",
        "addons/sale/__openerp__.py",
        "addons/sale/migrations/17.0.1.2/pre-migrate.py",
        "addons/account/upgrades/1.2/post-migrate.py",
        "setup/odoo-wsgi.example.py",
        "addons/mail/static/scripts/odoo-mailgate.py",
        "addons/sale/models/sale_order.pyc",
        "addons/sale/views/sale_order.xml",
    ] {
        assert!(!Python.claims(path), "{path}");
    }
}

#[test]
fn extract_is_a_function_of_the_bytes() {
    for file in fixtures() {
        let blob = std::fs::read(&file).unwrap();
        let first = Python.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Python.extract(&blob).unwrap().to_bytes(), "{}", file.display());
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first);
    }
}
