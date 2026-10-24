//! The Python extractor on hand-written fixtures. Each `fixtures/python/<name>.py` has its
//! expected output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::path::Path;

use common::{check, find, fixtures, render};
use odoo_changelog_extract::{Extractor, Python, SymbolId, SymbolTable, delta};
use serde_json::{Value, json};

/// Where a fixture is qualified: a controller module, or a model file of the `demo` module.
fn fixture_path(file: &Path) -> String {
    match file.file_stem().unwrap().to_str().unwrap() {
        "controllers" => "addons/demo/controllers/main.py".into(),
        stem => format!("addons/demo/models/{stem}.py"),
    }
}

#[test]
fn fixtures_match_expected_output() {
    let files = fixtures("python", "py");
    assert!(files.len() >= 9, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        check(
            &file.with_extension("json"),
            &render(&Python, &fixture_path(&file), &blob),
        );
    }
}

/// Every walk over the tree stays within a rayon worker's 2 MiB stack: a recursive one would
/// overflow it at a few thousand levels in a debug build. Nested blocks repeat their depth as
/// indentation on every line, so they stop at 2,000 levels, about 2 MB of source.
#[test]
fn deep_nesting_reads_within_a_worker_stack() {
    let n = 20_000;
    let deep = |open: &str, close: &str| format!("{}{}", open.repeat(n), close.repeat(n));
    let model = |body: String| format!("class M(models.Model):\n    _name = 'demo.m'\n\n{body}\n");
    let blocks = |indent: usize, head: &str| {
        let depth = 2_000;
        let src: String = (0..depth)
            .map(|i| format!("{}{head}\n", " ".repeat(indent + i)))
            .collect();
        src + &" ".repeat(indent + depth) + "pass\n"
    };
    let sources = [
        format!("A = {}", deep("[", "]")),
        format!("A = {}1{}", "(".repeat(n), ")".repeat(n)),
        format!("A = {}1{}", "{'a': ".repeat(n), "}".repeat(n)),
        format!("a = {}1{}", "f(".repeat(n), ")".repeat(n)),
        format!("a = x{}", ".b".repeat(n)),
        format!("a = 1{}", " + 1".repeat(n)),
        format!("a = {}1", "lambda: ".repeat(n)),
        model(format!(
            "    name = fields.Char(default={}1{})",
            "f(".repeat(n),
            ")".repeat(n)
        )),
        model(format!("    _sql_constraints = {}", deep("[", "]"))),
        model(format!(
            "    @api.depends({}1{})\n    def f(self):\n        return {}1",
            "f(".repeat(n),
            ")".repeat(n),
            "lambda: ".repeat(n),
        )),
        model(format!("    def f(self):\n        return {}", deep("[", "]"))),
        model(format!("    def f(self):\n{}", blocks(8, "def g():"))),
        blocks(0, "def f():"),
        blocks(0, "class A:"),
        blocks(0, "if a:"),
        "[".repeat(n),
        "(".repeat(n),
        "{".repeat(n),
    ];
    let tables = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || sources.map(|src| Python.extract(src.as_bytes()).map(|t| t.to_bytes().len())))
        .unwrap()
        .join()
        .unwrap();
    for table in tables {
        assert!(table.is_ok());
    }
}

fn extract(src: &str) -> SymbolTable {
    Python.extract(src.as_bytes()).unwrap()
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
    let fields: Vec<_> = table.iter().filter(|s| s.kind == "field").collect();
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

/// Each hashes like the first, so that moved or renamed code still pairs with it.
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

/// A local named like a field, method or keyword never renames the field, method or keyword.
#[test]
fn body_hash_keeps_attribute_and_keyword_names() {
    let attribute = |param: &str| {
        body(
            &format!("    def f(self, {param}):\n        return self.{param}\n"),
            "f",
        )
    };
    assert_ne!(attribute("name"), attribute("other"));
    let keyword = body("    def f(self, name):\n        return self.name(name=name)\n", "f");
    let renamed = body("    def f(self, value):\n        return self.name(name=value)\n", "f");
    assert_eq!(keyword, renamed);
}

#[test]
fn body_hash_keeps_the_comma_of_a_single_tuple_through_comments() {
    let plain = body("    def f(self, y):\n        return (y,)\n", "f");
    let commented = body("    def f(self, y):\n        return (y,  # c\n        )\n", "f");
    assert_eq!(plain, commented);
    assert_ne!(plain, body("    def f(self, y):\n        return (y)\n", "f"));
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
        ("error_region.py", "before", "after"),
        ("error_region_class.py", "demo.pond#depth", "demo.pond#drain"),
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
    for file in fixtures("python", "py") {
        let blob = std::fs::read(&file).unwrap();
        for symbol in Python.extract(&blob).unwrap().iter() {
            let name = symbol.name.as_str();
            let last = match symbol.kind.as_str() {
                "selection" => name.split_once(':').unwrap().1,
                "model" => name.split('[').next().unwrap(),
                "class" | "function" | "route" | "constant" => name.rsplit('.').next().unwrap(),
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
fn spans_count_bytes_in_any_encoding() {
    let blob = b"# d\xe9j\xe0 \xe2\x82\xac\nclass M(models.Model):\n    _name = 'm'\n    foo = fields.Char()\n";
    let table = Python.extract(blob).unwrap();
    assert_eq!(&blob[find(&table, "field", "m#foo").span.range()], b"foo");
    assert_eq!(&blob[find(&table, "model", "m").span.range()], b"m");
    assert_eq!(find(&table, "field", "m#foo").line, 4);

    let empty = Python.extract(b"").unwrap();
    assert_eq!((empty.iter().count(), empty.warnings().len()), (0, 0));
}

#[test]
fn inserting_lines_above_gives_no_event() {
    let path = "addons/sale_ext/models/sale_order.py";
    let old = extract(INHERITED);
    let new = extract(&format!("# A comment.\nimport logging\n\n_logger = 1\n\n{INHERITED}"));
    assert_ne!(old, new);
    assert!(delta(&Python, &[(Some((path, &old)), Some((path, &new)))]).is_empty());
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
        &[(
            Some(("addons/a/models/sale.py", &table)),
            Some(("addons/a/models/order.py", &table)),
        )],
    );
    let ops: Vec<(&str, &str)> = moved.iter().map(|d| (d.qualname.as_str(), d.op.as_str())).collect();
    assert_eq!(
        ops,
        [
            ("sale.order#foo@a", "moved"),
            ("odoo.addons.a.models.order.helper", "renamed"),
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
    for file in fixtures("python", "py") {
        let blob = std::fs::read(&file).unwrap();
        let first = Python.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Python.extract(&blob).unwrap().to_bytes(), "{}", file.display());
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first);
    }
}

/// A model whose one method is written `body`, named `name`.
fn model_with(name: &str, body: &str) -> String {
    format!(
        "from odoo import models\n\n\nclass Order(models.Model):\n    _inherit = 'sale.order'\n\n    def {name}(self):\n{body}"
    )
}

const TOTAL: &str = "        total = 0
        for line in self.order_line:
            if line.product_id and line.qty > 0:
                total += line.price_unit * line.qty
        self.amount = total
        return total
";

#[test]
fn a_renamed_method_with_an_edited_body_pairs_below_an_exact_rename() {
    let path = "addons/sale_ext/models/sale_order.py";
    let rename = |new_body: &str| {
        let (old, new) = (
            extract(&model_with("_compute_total", TOTAL)),
            extract(&model_with("_compute_amount", new_body)),
        );
        let changes = delta(&Python, &[(Some((path, &old)), Some((path, &new)))]);
        changes.into_iter().find_map(|d| d.rename)
    };
    let exact = rename(TOTAL).expect("an exact rename");
    assert_eq!((exact.confidence, exact.similarity), (1.0, None));

    let edited = rename(&TOTAL.replace("line.price_unit", "line.price_subtotal")).expect("an edited rename");
    assert!(
        edited.confidence < exact.confidence && edited.confidence >= 0.6,
        "{edited:?}"
    );
    assert!(edited.similarity.is_some_and(|s| s >= 0.5), "{edited:?}");
    assert!(edited.signals.contains(&"similar".to_string()), "{edited:?}");

    let unrelated = "        partner = self.env['res.partner'].search([('name', '=', self.name)])
        if not partner:
            raise UserError('No partner')
        return partner.id
";
    assert_eq!(rename(unrelated), None);
}

/// The renames from `old` to `new`, each `(path, src)` on its own side, as `(old, new)`.
fn renames(old: &[(&str, &str)], new: &[(&str, &str)]) -> Vec<(String, String)> {
    let read = |files: &[(&str, &str)]| -> Vec<(String, SymbolTable)> {
        files.iter().map(|(p, s)| (p.to_string(), extract(s))).collect()
    };
    let (old, new) = (read(old), read(new));
    let files: Vec<_> = (old.iter().map(|(p, t)| (Some((p.as_str(), t)), None)))
        .chain(new.iter().map(|(p, t)| (None, Some((p.as_str(), t)))))
        .collect();
    delta(&Python, &files)
        .into_iter()
        .filter(|d| d.rename.is_some())
        .map(|d| (d.old.unwrap().qualname, d.qualname))
        .collect()
}

fn fields(model: &str, fields: &str) -> String {
    format!("from odoo import fields, models\n\n\nclass Ext(models.Model):\n    _inherit = '{model}'\n\n{fields}")
}

#[test]
fn a_field_renamed_with_its_label_pairs() {
    let path = "addons/garden/models/garden_plot.py";
    let old = fields(
        "garden.plot",
        "    tool_id = fields.Many2one('garden.tool', string='Tool', domain=\"[('broken', '=', False)]\")\n",
    );
    let new = fields(
        "garden.plot",
        "    spare_tool_id = fields.Many2one('garden.tool', string='Spare Tool', domain=\"[('broken', '=', False)]\")\n",
    );
    assert_eq!(
        renames(&[(path, &old)], &[(path, &new)]),
        [(
            "garden.plot#tool_id@garden".into(),
            "garden.plot#spare_tool_id@garden".into()
        )]
    );
}

#[test]
fn a_model_renamed_with_its_description_and_class_pairs() {
    let model = |class: &str, name: &str, description: &str| {
        format!(
            "from odoo import fields, models\n\n\nclass {class}(models.Model):\n    _name = '{name}'\n    _description = '{description}'\n\n    plot_id = fields.Many2one('garden.plot')\n    bed_id = fields.Many2one('garden.bed')\n    text = fields.Html()\n"
        )
    };
    let old = model("GardenPlotNote", "garden.plot.note", "Garden Plot Note");
    let new = model("GardenPlotRemark", "garden.plot.remark", "Garden Plot Remark");
    let path = "addons/garden/models/garden_plot.py";
    let r = renames(&[(path, &old)], &[(path, &new)]);
    assert!(
        r.contains(&("garden.plot.note@garden".into(), "garden.plot.remark@garden".into())),
        "{r:?}"
    );
    assert_eq!(r.len(), 4, "the model and its three fields: {r:?}");
}

#[test]
fn a_field_moved_to_another_module_with_its_rename_pairs() {
    let side = |field: &str| {
        fields(
            "res.partner",
            &format!(
                "    {field} = fields.Char(string='Plot code', help='Code of the plot the partner rents', size=20)\n"
            ),
        )
    };
    let r = renames(
        &[(
            "addons/garden_rent/models/res_partner.py",
            &side("garden_rent_plot_code"),
        )],
        &[("addons/garden/models/res_partner.py", &side("plot_code"))],
    );
    assert!(
        r.contains(&(
            "res.partner#garden_rent_plot_code@garden_rent".into(),
            "res.partner#plot_code@garden".into()
        )),
        "{r:?}"
    );
}

#[test]
fn an_unrelated_field_of_another_type_is_no_rename() {
    let path = "addons/garden/models/garden_plot.py";
    let old = fields("garden.plot", "    partner_note = fields.Text(string='Partner note')\n");
    let new = fields(
        "garden.plot",
        "    partner_count = fields.Integer(string='Partner count', compute='_compute_partner_count')\n",
    );
    assert_eq!(renames(&[(path, &old)], &[(path, &new)]), []);
}

#[test]
fn a_field_renamed_with_its_compute_method_pairs() {
    let path = "addons/garden/models/garden_plot.py";
    let side = |field: &str, label: &str| {
        fields(
            "garden.plot",
            &format!(
                "    {field} = fields.Float(string='{label}', compute='_compute_{field}', store=True, help='Weight harvested this season')\n"
            ),
        )
    };
    let (old, new) = (side("harvest_weight", "Harvest weight"), side("yield_kg", "Yield"));
    assert_eq!(
        renames(&[(path, &old)], &[(path, &new)]),
        [(
            "garden.plot#harvest_weight@garden".into(),
            "garden.plot#yield_kg@garden".into()
        )]
    );
}
