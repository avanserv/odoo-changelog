//! The JS extractor on hand-written fixtures. Each `fixtures/js/<name>.js` has its expected
//! output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{batch, check, clones, find, fixtures, git, odoo_blob, render};
use odoo_changelog_extract::{Extractor, Js, Op, Registry, Symbol, SymbolTable, delta};
use serde_json::{Value, json};

/// Where a fixture is qualified: the `views` directory of the `demo` addon, or its test helpers.
fn fixture_path(file: &Path) -> String {
    match file.file_name().unwrap().to_str().unwrap() {
        "test_helpers.js" => "addons/demo/static/tests/helpers/test_helpers.js".into(),
        name => format!("addons/demo/static/src/views/{name}"),
    }
}

#[test]
fn fixtures_match_expected_output() {
    let files = fixtures("js", "js");
    assert!(files.len() >= 9, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        check(&file.with_extension("json"), &render(&Js, &fixture_path(&file), &blob));

        let first = Js.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Js.extract(&blob).unwrap().to_bytes(), "{}", file.display());
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first);
    }
}

/// Every walk over the tree stays within a rayon worker's 2 MiB stack: a recursive one would
/// overflow it at a few thousand levels in a debug build.
#[test]
fn deep_nesting_reads_within_a_worker_stack() {
    let n = 20_000;
    let deep = |open: &str, close: &str| format!("{}{}", open.repeat(n), close.repeat(n));
    let sources = [
        format!("export const a = {};", deep("[", "]")),
        format!("export const a = {}1{};", "f(".repeat(n), ")".repeat(n)),
        deep("{", "}"),
        format!("export const a = {}1{};", "{a: ".repeat(n), "}".repeat(n)),
        format!("export function f() {{ {} }}", deep("function g() {", "}")),
        format!("export const a = x{};", ".b".repeat(n)),
        format!("registry.category('a'){};", ".add('k', 1)".repeat(n)),
        format!("set{};", ".add(1)".repeat(n)),
        format!("export const a = {}1{};", "(".repeat(n), ")".repeat(n)),
        format!("patch(X, {});", deep("{a: patch(X, ", "{})}")),
        format!("export const a = {}{{}}{};", "_.extend({}, ".repeat(n), ")".repeat(n)),
        format!("export const a = {}{{}}{};", "{...".repeat(n), "}".repeat(n)),
        format!("if (a) {{}}{}", " else if (a) {}".repeat(n)),
        format!("export const a = 1{};", " + 1".repeat(n)),
        deep("odoo.define('a', function (require) {", "});"),
        "[".repeat(n),
        "(".repeat(n),
        "{".repeat(n),
        format!("x = `{}", "${`".repeat(n)),
    ];
    let tables = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || sources.map(|src| Js.extract(src.as_bytes()).map(|t| t.to_bytes().len())))
        .unwrap()
        .join()
        .unwrap();
    for table in tables {
        assert!(table.is_ok());
    }
}

fn extract(src: &str) -> SymbolTable {
    Js.extract(src.as_bytes()).unwrap()
}

/// The attrs of `symbol` qualified at `path`, as one JSON object.
fn attrs(path: &str, symbol: &Symbol) -> Value {
    Value::from_iter(Js.qualify_attrs(path, symbol).into_owned())
}

#[test]
fn registry_entry_is_named_by_category_and_key() {
    let src = "import { registry } from \"@web/core/registry\";\nexport const many2OneField = {};\nregistry.category(\"fields\").add(\"many2one\", many2OneField);\n";
    let path = "addons/web/static/src/views/fields/many2one/many2one_field.js";
    let table = extract(src);
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
    let entry = find(&table, "js_registry", "fields:many2one");
    assert_eq!(&src.as_bytes()[entry.span.range()], b"many2one");
    assert_eq!(entry.line, 3);
    assert_eq!(Js.qualify(path, entry), "fields:many2one@web");
    assert_eq!(
        attrs(path, entry)["value"],
        json!({"ref": "@web/views/fields/many2one/many2one_field#many2OneField"})
    );
}

#[test]
fn module_symbol_takes_the_path_name() {
    let es = extract("/** @odoo-module alias=demo.b */\nexport const x = 1;\n");
    let module = find(&es, "js_module", "");
    assert_eq!(Js.qualify("addons/x/static/src/a/b.js", module), "@x/a/b");
    assert_eq!(Js.qualify("addons/x/static/src/a/index.js", module), "@x/a");
    let alias = find(&es, "js_module", "demo.b");
    assert_eq!(Js.qualify("addons/x/static/src/a/b.js", alias), "demo.b");
    assert_eq!(
        attrs("addons/x/static/src/a/b.js", alias)["of"],
        json!({"ref": "@x/a/b#default"})
    );

    let legacy = extract("odoo.define('x.n', function (require) { return {}; });\n");
    assert_eq!(
        Js.qualify("addons/x/static/src/a/b.js", find(&legacy, "js_module", "x.n")),
        "x.n"
    );
    assert!(!legacy.iter().any(|s| s.name.is_empty()));

    let headerless = extract("import { a } from './a';\n");
    assert_eq!(find(&headerless, "js_module", "").attrs.get("header"), None);
    let ignored = extract("/** @odoo-module ignore */\nwindow.x = 1;\n");
    assert_eq!(ignored.iter().count(), 0);
}

#[test]
fn a_file_that_fails_to_parse_gives_a_partial_table_and_a_warning() {
    let blob = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/js/syntax_error.js")).unwrap();
    let table = Js.extract(&blob).unwrap();
    assert!(!table.warnings().is_empty());
    for (kind, name) in [
        ("js_export", "#before"),
        ("js_export", "#Broken"),
        ("js_export", "#after"),
        ("js_registry", "fields:after"),
    ] {
        find(&table, kind, name);
    }
    let unnamed = extract("odoo.define(name, function (require) {});\nodoo.define('a', function () {});\n");
    assert_eq!(
        unnamed.warnings(),
        ["line 1: odoo.define without a literal name; ignored"]
    );
    let twice = extract("export class A {}\nexport { A };\n");
    assert_eq!(twice.warnings(), ["line 2: duplicate js_export #A dropped"]);

    let empty = extract("");
    assert_eq!((empty.iter().count(), empty.warnings().len()), (0, 0));
    let noise: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    Js.extract(&noise).unwrap();
}

#[test]
fn relative_and_local_refs_resolve_against_the_path() {
    let src = "import { helper } from './utils';\nimport { Base } from '../base/index';\nexport class A extends Base {}\nexport const b = A;\n";
    let (one, two) = ("addons/demo/static/src/a/x.js", "addons/demo/static/src/b/c/x.js");
    let table = extract(src);
    let module = find(&table, "js_module", "");
    assert_eq!(
        attrs(one, module)["imports"],
        json!([{"ref": "@demo/base#Base"}, {"ref": "@demo/a/utils#helper"}])
    );
    assert_eq!(
        attrs(two, module)["imports"],
        json!([{"ref": "@demo/b/base#Base"}, {"ref": "@demo/b/c/utils#helper"}])
    );
    let a = find(&table, "js_export", "#A");
    assert_eq!(attrs(one, a)["extends"], json!({"ref": "@demo/base#Base"}));
    assert_eq!(Js.qualify(two, a), "@demo/b/c/x#A");
    assert_eq!(
        attrs(two, find(&table, "js_export", "#b"))["value"],
        json!({"ref": "@demo/b/c/x#A"})
    );
    assert_eq!(extract(src).to_bytes(), table.to_bytes());
}

const COMPONENT: &str = r#"/** @odoo-module **/
import { Component } from "@odoo/owl";
import { registry } from "@web/core/registry";
import { patch } from "@web/core/utils/patch";
import { Other } from "@demo/other";

export class Thing extends Component {
    static template = "demo.Thing";
    static props = { label: { type: String, optional: true } };

    setup() {
        this.label = this.props.label || "none";
    }
}

patch(Other.prototype, {
    run(a, b) {
        return [a, b];
    },
});

registry.category("fields").add("thing", { component: Thing, supportedTypes: ["char"] });
"#;

const REFORMATTED: &str = r#"/** @odoo-module **/
// Imports.
import {Component} from '@odoo/owl'
import {registry} from '@web/core/registry'
import {patch} from '@web/core/utils/patch'
import {Other} from '@demo/other'

/**
 * A thing.
 */
export class Thing extends Component {
	static template = 'demo.Thing'
	static props = {
		label: {type: String, optional: true,},
	}
	setup() {
		// Defaults the label.
		this.label = this.props.label || 'none'
	}
}

patch(Other.prototype, {
	run(a, b) {
		return [
			a,
			b,
		]
	}
})

registry
	.category('fields')
	.add('thing', {
		component: Thing,
		supportedTypes: ['char',],
	})
"#;

#[test]
fn reformatting_gives_no_event() {
    let path = "addons/demo/static/src/thing.js";
    let (old, new) = (extract(COMPONENT), extract(REFORMATTED));
    assert!(new.warnings().is_empty(), "{:?}", new.warnings());
    assert_ne!(old, new);
    assert_eq!(delta(&Js, &[(Some((path, &old)), Some((path, &new)))]), []);

    let locals = extract(&COMPONENT.replace(
        "run(a, b) {\n        return [a, b];",
        "run(x, y) {\n        return [x, y];",
    ));
    assert_eq!(delta(&Js, &[(Some((path, &old)), Some((path, &locals)))]), []);

    let edited = extract(&COMPONENT.replace("return [a, b];", "return [b, a];"));
    let changes = delta(&Js, &[(Some((path, &old)), Some((path, &edited)))]);
    let ops: Vec<(&str, &str, Op)> = changes
        .iter()
        .map(|d| (d.kind.as_str(), d.qualname.as_str(), d.op))
        .collect();
    assert_eq!(ops, [("js_patch", "@demo/other#Other.prototype@demo", Op::Changed)]);
    let (before, after): (Value, Value) = (
        serde_json::from_str(changes[0].attrs_before.as_deref().unwrap()).unwrap(),
        serde_json::from_str(changes[0].attrs_after.as_deref().unwrap()).unwrap(),
    );
    assert_ne!(before["members"]["run"], after["members"]["run"]);
    assert_eq!(before["declared"], after["declared"]);

    assert_eq!(only_changed_key(&changes[0]), "members");

    let setup = extract(&COMPONENT.replace("|| \"none\"", "|| \"empty\""));
    let changes = delta(&Js, &[(Some((path, &old)), Some((path, &setup)))]);
    let ops: Vec<(&str, &str, Op)> = changes
        .iter()
        .map(|d| (d.kind.as_str(), d.qualname.as_str(), d.op))
        .collect();
    assert_eq!(ops, [("js_export", "@demo/thing#Thing", Op::Changed)]);
    assert_eq!(only_changed_key(&changes[0]), "members");
}

/// The one top-level attr a change touches.
fn only_changed_key(change: &odoo_changelog_extract::Delta) -> String {
    let parse = |attrs: &Option<String>| -> serde_json::Map<String, Value> {
        serde_json::from_str(attrs.as_deref().unwrap()).unwrap()
    };
    let (before, after) = (parse(&change.attrs_before), parse(&change.attrs_after));
    let keys: std::collections::BTreeSet<&String> = before
        .keys()
        .chain(after.keys())
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    keys.first().unwrap().to_string()
}

#[test]
fn exporting_one_declaration_again_points_at_its_first_export() {
    let k = 1000;
    let methods: String = (0..50).map(|i| format!("    m{i}() {{ return {i}; }}\n")).collect();
    let aliases: Vec<String> = (0..k).map(|i| format!("A as a{i}")).collect();
    let src = format!(
        "class A {{\n{methods}}}\nconst d = {{ dependencies: [{}] }};\n{}export {{ {} }};\n",
        (0..50).map(|i| format!("'s{i}'")).collect::<Vec<_>>().join(", "),
        (0..k)
            .map(|i| format!("registry.category('services').add('s{i}', d);\n"))
            .collect::<String>(),
        aliases.join(", "),
    );
    let table = extract(&src);
    let size = table.to_bytes().len();
    assert!(size < 30 * src.len(), "{size} bytes from {}", src.len());
    let first = find(&table, "js_export", "#a0");
    assert_eq!(first.attrs["members"].as_object().unwrap().len(), 50);
    let later = find(&table, "js_export", "#a1");
    assert_eq!(
        Value::from_iter(later.attrs.clone()),
        json!({"kind": "class", "local": "A", "of": {"ref": "#a0"}})
    );
    assert_eq!(
        find(&table, "js_registry", "services:s0").attrs["dependencies"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
    assert!(
        !find(&table, "js_registry", "services:s1")
            .attrs
            .contains_key("dependencies")
    );
}

/// Long member chains and names read as expressions past a bound, so no binding or name repeats
/// a long text: the output stays linear in the input.
#[test]
fn long_chains_stay_linear() {
    let k = 20_000;
    let keys: Vec<String> = (0..k).map(|i| format!("a{i}")).collect();
    let chain = format!("x{}", ".b".repeat(k));
    let destructured = format!("const {{ {} }} = {chain};\nexport {{ a0, a1 }};\n", keys.join(", "));
    let adds: String = (0..k).map(|i| format!(".add('k{i}', 1)")).collect();
    for src in [
        destructured.clone(),
        format!("import {{ x }} from 'y';\n{destructured}"),
        format!("import {{ x }} from 'y';\nexport const a = {chain};\n"),
        format!("import core from 'web.core';\ncore{}.registry{adds};\n", ".b".repeat(k)),
        format!("registry{}{adds};\n", ".category('c')".repeat(k)),
        format!("patch({chain}, {{ a() {{}} }});\n"),
    ] {
        assert_linear(&src);
    }
}

/// `k` references to one text of `len` bytes, in every place a binding, a scope or a name
/// stores text: a source, an imported or destructured name, a define name, a first export, a
/// category path.
fn long_text(kind: &str, k: usize, len: usize) -> String {
    let long = "L".repeat(len);
    let lines = |f: &dyn Fn(usize) -> String| (0..k).map(f).collect::<Vec<_>>().join("\n");
    let names = (0..k).map(|i| format!("n{i}")).collect::<Vec<_>>().join(", ");
    let adds = |name: &str| lines(&|i| format!("registry.category(\"a\").add(\"k{i}\", {name});"));
    match kind {
        "import_source" => format!("import {{ x }} from \"{long}\";\n{}", adds("x")),
        "import_name" => format!("import {{ {long} as a }} from \"y\";\n{}", adds("a")),
        "define_name" => format!(
            "odoo.define(\"{long}\", function (require) {{\n{}\n}});",
            lines(&|i| format!("var W{i} = Base.extend({{}});"))
        ),
        "define_refs" => format!(
            "odoo.define(\"{long}\", function (require) {{ var A = 1;\n{}\nvar reg = require(\"@web/core/registry\").registry; }});",
            lines(&|i| format!("reg.category(\"a\").add(\"k{i}\", A);"))
        ),
        "legacy_registry" => format!(
            "import fooRegistry from \"{long}.registry\";\n{}",
            lines(&|i| format!("fooRegistry.add(\"k{i}\", 1);"))
        ),
        "patch_target" => format!(
            "import {{ X }} from \"{long}\";\n{}",
            lines(&|i| format!("patch(X.prototype, {{ m{i}() {{}} }});"))
        ),
        "global_key" => format!("const {{ \"{long}\": a }} = owl;\n{}", adds("a")),
        "header_alias" => format!("/** @odoo-module alias={long} */"),
        "extends_long" => format!(
            "import {{ {long} as B }} from \"y\";\n{}",
            lines(&|i| format!("export class C{i} extends B {{}}"))
        ),
        "imports_many" => lines(&|i| format!("import {{ n{i} }} from \"{long}\";")),
        "named_imports" => format!("import {{ {names} }} from \"{long}\";"),
        "reexports" => format!("export {{ {names} }} from \"{long}\";"),
        "define_widgets" => format!(
            "odoo.define(\"{long}\", function (require) {{\n{}\n}});",
            lines(&|i| format!("var W{i} = Base.extend({{a: 1}});"))
        ),
        "require_many" => format!(
            "odoo.define(\"d\", function (require) {{ var X = require(\"{long}\");\n{}\n}});",
            lines(&|i| format!("X.include({{m{i}() {{}}}});"))
        ),
        "first_export_long" => format!(
            "class A {{}}\nexport {{ A as {long} }};\nexport {{ {} }};",
            (0..k).map(|i| format!("A as a{i}")).collect::<Vec<_>>().join(", ")
        ),
        "category_vars" => format!(
            "import {{ registry }} from \"@web/core/registry\";\nconst r = registry{};\n{}",
            format!(".category(\"{}\")", "c".repeat(250)).repeat(64),
            lines(&|i| format!("r.add(\"k{i}\", 1);"))
        ),
        kind => panic!("unknown kind {kind}"),
    }
}

#[test]
fn long_texts_stay_linear() {
    for kind in [
        "import_source",
        "import_name",
        "define_name",
        "define_refs",
        "legacy_registry",
        "patch_target",
        "global_key",
        "header_alias",
        "extends_long",
        "imports_many",
        "named_imports",
        "reexports",
        "define_widgets",
        "require_many",
        "first_export_long",
        "category_vars",
    ] {
        assert_linear(&long_text(kind, 2000, 5000));
    }
}

/// Output under 30 bytes per input byte, no warning, and short names.
fn assert_linear(src: &str) {
    let table = extract(src);
    let size = table.to_bytes().len();
    let head = &src[..60.min(src.len())];
    assert!(size < 30 * src.len(), "{size} bytes from {} for {head}", src.len());
    assert!(table.warnings().is_empty(), "{:?} for {head}", table.warnings());
    assert!(table.iter().all(|s| s.name.len() <= 512), "{head}");
}

#[test]
fn duplicate_warnings_are_capped() {
    let keys = vec!["a: A"; 50].join(", ");
    let table = extract(&format!(
        "odoo.define('d', function () {{ var A = 1; return {{ {keys} }}; }});\n"
    ));
    let warnings = table.warnings();
    assert_eq!(warnings.len(), 11, "{warnings:?}");
    assert_eq!(warnings[0], "line 1: duplicate js_export d#a dropped");
    assert_eq!(warnings[10], "39 more duplicates");
}

#[test]
fn a_long_legacy_source_keeps_its_registry_entries() {
    let src = format!(
        "import fooRegistry from \"{}.registry\";\nfooRegistry.add(\"k\", 1);\n",
        "L".repeat(300)
    );
    let table = extract(&src);
    let entry = table.iter().find(|s| s.kind == "js_registry").unwrap();
    assert!(
        entry.name.starts_with('~') && entry.name.ends_with(".registry:k"),
        "{}",
        entry.name
    );
    assert_eq!(entry.name.len(), "~".len() + 32 + ".registry:k".len());
}

#[test]
fn a_block_local_patch_is_not_odoos() {
    let table = extract("if (x) {\n    const patch = make();\n    patch(A, { a() {} });\n}\npatch(B, { b() {} });\n");
    let patches: Vec<&str> = table
        .iter()
        .filter(|s| s.kind == "js_patch")
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(patches, ["B"]);
}

#[test]
fn legacy_default_imports_read_as_requires() {
    let src = "import FormController from 'web.FormController';\nimport core from 'web.core';\nimport { x } from 'web.x';\nFormController.include({ a() {} });\ncore.action_registry.add('k', 1);\n";
    let table = extract(src);
    find(&table, "js_patch", "web.FormController");
    find(&table, "js_registry", "web.core#action_registry:k");
    let imports = &find(&table, "js_module", "").attrs["imports"];
    assert_eq!(
        imports,
        &json!([{"ref": "web.FormController"}, {"ref": "web.core"}, {"ref": "web.x#x"}])
    );
}

#[test]
fn block_names_shadow_the_scope() {
    let src = "import { Foo } from './foo';\ntry { x(); } catch (registry) { registry.category('a').add('caught', 1); }\nfor (const Foo of xs) { registry.category('a').add('loop', Foo); }\nregistry.category('a').add('top', Foo);\n";
    let table = extract(src);
    assert!(!table.contains("js_registry", "a:caught"));
    assert_eq!(
        find(&table, "js_registry", "a:loop").attrs["value"],
        json!({"expr": "Foo"})
    );
    assert_eq!(
        find(&table, "js_registry", "a:top").attrs["value"],
        json!({"ref": "./foo#Foo"})
    );
}

#[test]
fn every_symbol_spans_its_name() {
    for file in fixtures("js", "js") {
        let blob = std::fs::read(&file).unwrap();
        for symbol in Js.extract(&blob).unwrap().iter() {
            let spanned = String::from_utf8_lossy(&blob[symbol.span.range()]).into_owned();
            let name = symbol.name.as_str();
            let member = name.rsplit_once('#').map_or(name, |(_, m)| m);
            let ok = match symbol.kind.as_str() {
                "js_registry" => {
                    let key = name.split_once(':').unwrap().1;
                    key == spanned || key == format!("[{spanned}]")
                }
                "js_export" if member == "default" => matches!(spanned.as_str(), "default" | "return"),
                "js_export" | "js_component" => member == spanned,
                "js_mail_model" => name == spanned,
                "js_widget" => member.rsplit('.').next() == Some(spanned.as_str()),
                "js_module" => name == spanned || (name.is_empty() && matches!(spanned.as_str(), "@odoo-module" | "")),
                "js_patch" => {
                    let target = name.split('[').next().unwrap();
                    let written = spanned.split_once('.').map_or(spanned.as_str(), |(_, rest)| rest);
                    target.ends_with(written)
                }
                kind => panic!("unknown kind {kind}"),
            };
            assert!(ok, "{}: {spanned:?} for {symbol:?}", file.display());
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
fn builtin_registry_gives_js_files_one_claimant() {
    let registry = Registry::builtin();
    let claimants =
        |path: &str| -> Vec<&str> { registry.iter().filter(|e| e.claims(path)).map(|e| e.name()).collect() };
    for path in ["addons/sale/static/src/x.js", "addons/sale/static/tests/helpers/x.js"] {
        assert_eq!(claimants(path), ["js"], "{path}");
    }
    for path in ["addons/sale/static/tests/x_tests.js", "addons/sale/static/lib/x.js"] {
        assert!(claimants(path).is_empty(), "{path}");
    }
}

#[test]
fn test_helpers_keep_their_modules_and_exports_only() {
    let src = "export const a = 1;\nregistry.category('services').add('x', {});\npatch(Foo, {});\n";
    let table = extract(src);
    let kinds = |path: &str| -> Vec<&str> {
        let kept = table.iter().filter(|s| Js.keeps(path, s));
        kept.map(|s| s.kind.as_str()).collect()
    };
    assert_eq!(
        kinds("addons/x/static/src/a.js"),
        ["js_export", "js_module", "js_patch", "js_registry"]
    );
    assert_eq!(kinds("addons/x/static/tests/a.js"), ["js_export", "js_module"]);
}

/// Blobs of the community clone, pinned so that the facts hold as the branches move: 17.0 and
/// 16.0 heads on 2026-10-20, and 12.0's.
const MANY2ONE_FIELD_17: &str = "296245967d02f056fc2386fbce911d77cd914b2a";
const WIDGET_12: &str = "969ad5bf30d13c48e4c238a7ffe228fe13259313";
const FIELD_REGISTRY_12: &str = "4406231868f2243f2d1f4fb004534cd5840be074";
const BARCODE_FORM_VIEW_12: &str = "15e27498f943761ec7051778ef77d8d38a8db98a";
const SALE_PRODUCT_FIELD_17: &str = "dd5eb33a8b58c514ea9a394795b278306395ef97";
const SALE_PRODUCT_FIELD_16: &str = "55afecd224c1fd30d4011471a55e24e8c5d9649b";
const MAIL_THREAD_16: &str = "5013146917488234d44a7ff739dafa76767a19c4";
const WEB_TEST_UTILS_16: &str = "28732493f6c6b64918f39cf2ed3ed93a6f045ad2";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn real_js_files() {
    let read = |oid: &str| {
        let blob = odoo_blob("odoo", oid);
        let table = Js.extract(&blob).unwrap();
        assert!(table.warnings().is_empty(), "{oid}: {:?}", table.warnings());
        (blob, table)
    };
    let refs = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["ref"].as_str().unwrap().to_string())
            .collect()
    };

    let path = "addons/web/static/src/views/fields/many2one/many2one_field.js";
    let (blob, m2o) = read(MANY2ONE_FIELD_17);
    let module = find(&m2o, "js_module", "");
    assert_eq!(Js.qualify(path, module), "@web/views/fields/many2one/many2one_field");
    let a = attrs(path, module);
    assert_eq!(a["header"], true);
    let imports = refs(&a["imports"]);
    for import in [
        "@web/core/registry#registry",
        "@web/views/fields/standard_field_props#standardFieldProps",
    ] {
        assert!(imports.iter().any(|i| i == import), "{imports:?}");
    }
    for (key, line) in [("many2one", 386), ("list.many2one", 388), ("kanban.many2one", 389)] {
        let entry = find(&m2o, "js_registry", &format!("fields:{key}"));
        assert_eq!(&blob[entry.span.range()], key.as_bytes());
        assert_eq!(
            (entry.line, Js.qualify(path, entry)),
            (line, format!("fields:{key}@web"))
        );
        assert_eq!(
            attrs(path, entry)["value"],
            json!({"ref": "@web/views/fields/many2one/many2one_field#many2OneField"})
        );
    }
    let field = attrs(path, find(&m2o, "js_component", "#Many2OneField"));
    assert_eq!(field["template"], "web.Many2OneField");
    assert_eq!(
        field["components"]["Many2XAutocomplete"],
        json!({"ref": "@web/views/fields/relational_utils#Many2XAutocomplete"})
    );
    assert!(field["props"].get("placeholder").is_some() && field["props"].get("...standardFieldProps").is_some());
    assert_eq!(field["static"]["SEARCH_MORE_LIMIT"], 320);
    let dialog = attrs(path, find(&m2o, "js_component", "#CreateConfirmationDialog"));
    assert_eq!(dialog["template"], "web.Many2OneField.CreateConfirmationDialog");
    let class = attrs(path, find(&m2o, "js_export", "#Many2OneField"));
    assert_eq!(
        (&class["kind"], &class["extends"]),
        (&json!("class"), &json!({"ref": "@odoo/owl#Component"}))
    );
    for member in ["setup", "get value", "onClick"] {
        assert!(class["members"].get(member).is_some(), "{member}");
    }
    assert_eq!(find(&m2o, "js_export", "#many2OneField").attrs["kind"], "const");
    assert_eq!(find(&m2o, "js_export", "#m2oTupleFromData").attrs["kind"], "function");

    let path = "addons/web/static/src/js/core/widget.js";
    let (_, widget) = read(WIDGET_12);
    assert_eq!(
        find(&widget, "js_module", "web.Widget").attrs["requires"],
        json!(["web.ServicesMixin", "web.ajax", "web.core", "web.mixins"])
    );
    let w = attrs(path, find(&widget, "js_widget", "web.Widget#Widget"));
    assert_eq!(w["extends"], json!({"ref": "web.core#Class"}));
    assert_eq!(refs(&w["mixins"]), ["web.mixins#PropertiesMixin", "web.ServicesMixin"]);
    assert_eq!(w["template"], Value::Null);
    for member in ["init", "start", "destroy"] {
        assert_eq!(w["members"][member]["fn"].as_str().unwrap().len(), 32, "{member}");
    }
    let default = find(&widget, "js_export", "web.Widget#default");
    assert_eq!(
        (&default.attrs["kind"], &default.attrs["local"]),
        (&json!("widget"), &json!("Widget"))
    );

    let path = "addons/web/static/src/js/fields/field_registry.js";
    let (blob, registry) = read(FIELD_REGISTRY_12);
    find(&registry, "js_module", "web.field_registry");
    find(&registry, "js_module", "web._field_registry");
    let entry = find(&registry, "js_registry", "web.field_registry:many2one");
    assert_eq!(
        entry.attrs["value"],
        json!({"ref": "web.relational_fields#FieldMany2One"})
    );
    assert_eq!((&blob[entry.span.range()], entry.line), (&b"many2one"[..], 68));
    assert_eq!(Js.qualify(path, entry), "web.field_registry:many2one@web");

    let path = "addons/barcodes/static/src/js/barcode_form_view.js";
    let (_, barcodes) = read(BARCODE_FORM_VIEW_12);
    for target in ["web.FormController", "web.FormRenderer"] {
        let patch = find(&barcodes, "js_patch", target);
        assert_eq!(Js.qualify(path, patch), format!("{target}@barcodes"));
        assert_eq!(patch.attrs["declared"], "include");
    }
    let controller = &find(&barcodes, "js_patch", "web.FormController").attrs["members"];
    for member in ["init", "destroy", "_barcodeScanned", "custom_events"] {
        assert!(controller.get(member).is_some(), "{member}");
    }

    let path = "addons/sale_product_configurator/static/src/js/sale_product_field.js";
    let target = "@sale/js/sale_product_field#SaleOrderLineProductField.prototype";
    let (_, patch_17) = read(SALE_PRODUCT_FIELD_17);
    let (_, patch_16) = read(SALE_PRODUCT_FIELD_16);
    let (p17, p16) = (find(&patch_17, "js_patch", target), find(&patch_16, "js_patch", target));
    for patch in [p17, p16] {
        assert_eq!(Js.qualify(path, patch), format!("{target}@sale_product_configurator"));
    }
    let members: Vec<&String> = p17.attrs["members"].as_object().unwrap().keys().collect();
    assert_eq!(
        members,
        [
            "_editProductConfiguration",
            "_onProductTemplateUpdate",
            "_openProductConfigurator",
            "get isConfigurableTemplate",
            "getProductConfiguratorDialogProps",
            "setup",
        ]
    );
    assert_eq!(
        (p16.attrs.get("name"), p17.attrs.get("name")),
        (Some(&json!("sale_product_configurator")), None)
    );

    let path = "addons/mail/static/src/models/thread.js";
    let (_, thread) = read(MAIL_THREAD_16);
    let model = find(&thread, "js_mail_model", "Thread");
    assert_eq!(Js.qualify(path, model), "Thread@mail");
    let fields = model.attrs["fields"].as_object().unwrap();
    assert_eq!(fields["activities"]["type"], "many");
    assert!(fields.len() > 100, "{}", fields.len());
    assert!(
        model.attrs["recordMethods"]
            .as_object()
            .unwrap()
            .contains_key("changeDescription")
    );

    let path = "addons/web/static/tests/helpers/utils.js";
    let (_, utils) = read(WEB_TEST_UTILS_16);
    let kept: Vec<&Symbol> = utils.iter().filter(|s| Js.keeps(path, s)).collect();
    assert!(
        kept.iter()
            .all(|s| matches!(s.kind.as_str(), "js_module" | "js_export"))
    );
    let fixture = find(&utils, "js_export", "#getFixture");
    assert_eq!(Js.qualify(path, fixture), "@web/../tests/helpers/utils#getFixture");
    assert_eq!(kept.iter().filter(|s| s.kind == "js_export").count(), 40);
}

/// Every claimed JS file at the tips reads, deterministically and with spans inside the blob.
/// Prints the symbols per kind and the first files with warnings, without asserting on them.
#[test]
#[ignore = "reads every Odoo clone in config/odoo-changelog.toml"]
fn every_claimed_js_file_at_the_tips_reads() {
    let mut read = 0;
    for (repo, clone) in clones() {
        let refs = git(
            &clone,
            &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin/"],
        );
        let refs = String::from_utf8(refs).unwrap();
        let mut blobs = BTreeMap::new();
        for branch in ["12.0", "14.0", "15.0", "16.0", "17.0", "master"] {
            let rev = format!("origin/{branch}");
            if !refs.lines().any(|r| r == rev) {
                continue;
            }
            let tree = git(&clone, &["ls-tree", "-r", "-z", &rev]);
            for line in String::from_utf8(tree).unwrap().split_terminator('\0') {
                let (meta, path) = line.split_once('\t').unwrap();
                if Js.claims(path) {
                    let oid = meta.rsplit(' ').next().unwrap().to_string();
                    blobs.entry(oid).or_insert_with(|| format!("{rev}:{path}"));
                }
            }
        }
        let oids: Vec<&str> = blobs.keys().map(String::as_str).collect();
        let (mut kinds, mut warned) = (BTreeMap::<String, usize>::new(), Vec::new());
        for (blob, (oid, at)) in batch(&clone, &oids).into_iter().zip(&blobs) {
            let table = Js.extract(&blob).unwrap();
            assert_eq!(table.to_bytes(), Js.extract(&blob).unwrap().to_bytes(), "{repo} {at}");
            let path = at.split_once(':').unwrap().1;
            for symbol in table.iter() {
                if Js.keeps(path, symbol) {
                    *kinds.entry(symbol.kind.clone()).or_default() += 1;
                }
                assert!(
                    symbol.span.start <= symbol.span.end && symbol.span.end as usize <= blob.len(),
                    "{repo} {at} {oid}"
                );
            }
            if !table.warnings().is_empty() {
                warned.push(format!("{repo} {at}: {:?}", table.warnings()));
            }
        }
        for warning in warned.iter().take(20) {
            println!("{warning}");
        }
        println!(
            "{repo}: {} files, {} with warnings, {kinds:?}",
            blobs.len(),
            warned.len()
        );
        read += blobs.len();
    }
    assert!(read > 0);
}
