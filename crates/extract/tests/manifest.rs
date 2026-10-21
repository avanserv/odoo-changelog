//! The manifest extractor on hand-written fixtures. Each `fixtures/manifest/<name>.py` has its
//! expected output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::collections::BTreeSet;

use common::{check, clones, fixtures, git, odoo_blob, render};
use odoo_changelog_extract::{Extractor, Manifest, Op, Registry, Symbol, SymbolTable, delta};
use serde_json::{Value, json};

const SALE: &str = "addons/sale/__manifest__.py";

fn extract(src: &str) -> SymbolTable {
    Manifest.extract(src.as_bytes()).unwrap()
}

fn module(table: &SymbolTable) -> &Symbol {
    let symbols: Vec<&Symbol> = table.iter().collect();
    assert_eq!(symbols.len(), 1, "{symbols:?}");
    assert_eq!(symbols[0].kind, "module");
    symbols[0]
}

#[test]
fn fixtures_match_expected_output() {
    let files = fixtures("manifest", "py");
    assert!(files.len() >= 6, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        let stem = file.file_stem().unwrap().to_str().unwrap();
        let path = format!("addons/{stem}/__manifest__.py");
        check(&file.with_extension("json"), &render(&Manifest, &path, &blob));

        let first = Manifest.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Manifest.extract(&blob).unwrap().to_bytes(), "{stem}");
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first, "{stem}");
    }
}

#[test]
fn sale_module_lists_its_depends_in_order() {
    let src = "{\n    'name': 'Sales',\n    'depends': [\n        'sales_team',\n        'account_payment',  # -> account, payment\n        'utm',\n    ],\n}\n";
    let table = extract(src);
    let sale = module(&table);
    assert_eq!(Manifest.qualify(SALE, sale), "sale");
    assert_eq!(sale.attrs["depends"], json!(["sales_team", "account_payment", "utm"]));
    assert_eq!(sale.line, 1);
    assert_eq!(&src.as_bytes()[sale.span.range()], src.trim_end().as_bytes());
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
}

#[test]
fn non_literal_values_are_kept_as_source() {
    let src = "{'name': 'x', 'data': __import__('os').listdir('.'), 'version': VERSION}";
    let table = extract(src);
    let attrs = &module(&table).attrs;
    assert_eq!(attrs["data"], json!({"expr": "__import__('os').listdir('.')"}));
    assert_eq!(attrs["version"], json!({"expr": "VERSION"}));
    assert_eq!(attrs["name"], "x");
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
}

#[test]
fn assets_keep_each_bundle_in_order_with_directives_as_tuples() {
    let src = r#"{
    'assets': {
        'web.z_last': ['z/static/a.js'],
        'web.assets_backend': [
            'demo/static/src/**/*',
            ('include', 'web._assets_helpers'),
            ("remove", "demo/static/src/legacy/*"),
            ('prepend', 'demo/static/src/first.scss'),
            ('after', 'web/static/src/main.js', 'demo/static/src/after.js'),
            ('before', 'web/static/src/main.js', 'demo/static/src/before.js'),
            ('replace', 'web/static/src/old.js', 'demo/static/src/new.js'),
            'demo/static/src/last.xml',
        ],
    },
}"#;
    let table = extract(src);
    let assets = &module(&table).attrs["assets"];
    assert_eq!(
        assets["web.assets_backend"],
        json!([
            "demo/static/src/**/*",
            ["include", "web._assets_helpers"],
            ["remove", "demo/static/src/legacy/*"],
            ["prepend", "demo/static/src/first.scss"],
            ["after", "web/static/src/main.js", "demo/static/src/after.js"],
            ["before", "web/static/src/main.js", "demo/static/src/before.js"],
            ["replace", "web/static/src/old.js", "demo/static/src/new.js"],
            "demo/static/src/last.xml",
        ])
    );
    let bundles: Vec<&String> = assets.as_object().unwrap().keys().collect();
    assert_eq!(bundles, ["web.assets_backend", "web.z_last"]);
}

#[test]
fn malformed_manifests_give_a_partial_table_and_a_warning() {
    for (stem, kept) in [
        ("syntax_error", &["name", "depends"][..]),
        ("unclosed", &["name", "depends", "data", "installable"]),
        ("not_literal", &["name", "depends", "license"]),
        ("not_a_dict", &[]),
    ] {
        let file = fixtures("manifest", "py")
            .into_iter()
            .find(|f| f.file_stem().unwrap() == stem)
            .unwrap();
        let table = Manifest.extract(&std::fs::read(file).unwrap()).unwrap();
        assert!(!table.warnings().is_empty(), "{stem}");
        if kept.is_empty() {
            assert_eq!(table.iter().count(), 0, "{stem}");
            assert_eq!(table.warnings(), ["line 1: no manifest dictionary"]);
            continue;
        }
        let attrs = &module(&table).attrs;
        for key in kept {
            assert!(attrs.contains_key(*key), "{stem}: no {key} in {attrs:?}");
        }
    }
}

#[test]
fn a_repeated_key_keeps_the_last_value_silently() {
    let table = extract("{'a': 1, 'a': 2}");
    assert_eq!(module(&table).attrs["a"], 2);
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
}

#[test]
fn empty_or_garbage_manifests_give_no_symbol() {
    for src in ["", "# nothing\n", "garbage ]]] {", "dict(name='x')"] {
        let table = extract(src);
        assert_eq!(table.iter().count(), 0, "{src:?}");
        assert!(
            table.warnings().iter().any(|w| w == "line 1: no manifest dictionary"),
            "{src:?}: {:?}",
            table.warnings()
        );
    }
}

/// `(op, qualname)` of each change from `old` at `old_path` to `new` at `new_path`.
fn changes(old_path: &str, old: &str, new_path: &str, new: &str) -> Vec<(Op, String)> {
    let (old, new) = (extract(old), extract(new));
    delta(&Manifest, Some((old_path, &old)), Some((new_path, &new)))
        .into_iter()
        .map(|d| (d.op, d.qualname))
        .collect()
}

#[test]
fn renaming_openerp_to_manifest_is_a_move() {
    let openerp = "addons/sale/__openerp__.py";
    let base = "{'name': 'Sales', 'depends': ['sales_team', 'payment']}\n";
    let sale = || "sale".to_string();
    assert_eq!(changes(openerp, base, SALE, base), [(Op::Moved, sale())]);

    let edited = base.replace("'payment'", "'account_payment'");
    assert_eq!(changes(openerp, base, SALE, &edited), [(Op::Changed, sale())]);

    let moved = changes("addons/a/__manifest__.py", base, "addons/b/__manifest__.py", base);
    assert_eq!(moved, [(Op::Removed, "a".into()), (Op::Added, "b".into())]);

    for reformatted in [
        "{\n    'name': 'Sales',\n    'depends': [\n        'sales_team',\n        'payment',\n    ],\n}\n",
        "# Sales.\n{'name': 'Sales', 'depends': ['sales_team', 'payment']}\n",
        "{\"name\": \"Sales\", \"depends\": [\"sales_team\", \"payment\"]}\n",
        "{'depends': ['sales_team', 'payment',], 'name': 'Sal' 'es',}\n",
    ] {
        assert_eq!(changes(SALE, base, SALE, reformatted), [], "{reformatted}");
    }
}

#[test]
fn builtin_registry_gives_manifests_one_claimant() {
    let registry = Registry::builtin();
    for path in [SALE, "odoo/addons/base/__openerp__.py", "theme_demo/__manifest__.py"] {
        let claimants: Vec<&str> = registry.iter().filter(|e| e.claims(path)).map(|e| e.name()).collect();
        assert_eq!(claimants, ["manifest"], "{path}");
    }
    assert!(!Manifest.claims("addons/sale/models/sale_order.py"));
}

/// The `addons/sale/__manifest__.py` of each branch head on 2026-10-20, pinned so that the facts
/// hold as the branches move.
const SALE_12: &str = "7f1a212c3b8681c6a10e726ce6f6bda5838794c6";
const SALE_17: &str = "5958231a843d358ef198321d0e70b9ca5a172e9a";
const SALE_19: &str = "5af4e9bc100119d2c5e92d67a66001a712d2918c";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn real_sale_manifests() {
    let read = |oid: &str| {
        let table = Manifest.extract(&odoo_blob("odoo", oid)).unwrap();
        assert!(table.warnings().is_empty(), "{oid}: {:?}", table.warnings());
        let sale = module(&table).clone();
        assert_eq!(Manifest.qualify(SALE, &sale), "sale");
        sale.attrs
    };

    let v12 = read(SALE_12);
    assert_eq!(v12["depends"], json!(["sales_team", "payment", "portal"]));
    assert_eq!(v12["data"].as_array().unwrap().len(), 23);
    assert!(!v12.contains_key("assets"));
    assert_eq!(v12["uninstall_hook"], "uninstall_hook");

    let v17 = read(SALE_17);
    assert_eq!(v17["depends"], json!(["sales_team", "account_payment", "utm"]));
    assert_eq!(
        v17["assets"]["web.qunit_suite_tests"],
        json!(["sale/static/tests/**/*", ["remove", "sale/static/tests/tours/**/*"]])
    );

    let v19 = read(SALE_19);
    assert_eq!(v19["depends"], json!(["sales_team", "account_payment", "utm"]));
    assert_eq!(v19["data"].as_array().unwrap().len(), 34);
    assert_eq!(v19["post_init_hook"], "_post_init_hook");
    assert_eq!(v19["license"], "LGPL-3");
    let bundles: Vec<(&str, usize)> = v19["assets"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_array().unwrap().len()))
        .collect();
    assert_eq!(
        bundles,
        [
            ("web.assets_backend", 20),
            ("web.assets_frontend", 2),
            ("web.assets_tests", 4),
            ("web.assets_unit_tests", 3),
            ("web.report_assets_common", 1),
        ]
    );
    assert_eq!(
        v19["assets"]["web.assets_backend"][0],
        "sale/static/src/scss/sale_onboarding.scss"
    );
    for attrs in [v12, v17, v19] {
        let (attrs, mut found) = (Value::from_iter(attrs), Vec::new());
        exprs(&attrs, &mut found);
        assert!(found.is_empty(), "{found:?}");
    }
}

/// The text of every `{"expr"}` in `value`.
fn exprs<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => match map.get("expr") {
            Some(Value::String(text)) => out.push(text),
            _ => map.values().for_each(|v| exprs(v, out)),
        },
        Value::Array(items) => items.iter().for_each(|v| exprs(v, out)),
        _ => {}
    }
}

const DIRECTIVES: [&str; 7] = ["append", "prepend", "before", "after", "include", "remove", "replace"];

/// Real manifests are all Python literals, but a set (`{'a'}`, once a bundle in error) has no
/// JSON form and is the one literal kept as `{"expr"}`.
#[test]
#[ignore = "reads every Odoo clone in config/odoo-changelog.toml"]
fn every_manifest_at_the_tips_reads_cleanly() {
    let mut read = 0;
    for (repo, clone) in clones() {
        let refs = git(
            &clone,
            &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin/"],
        );
        let refs = String::from_utf8(refs).unwrap();
        let mut blobs = BTreeSet::new();
        for branch in ["12.0", "17.0", "19.0", "master"] {
            let rev = format!("origin/{branch}");
            if !refs.lines().any(|r| r == rev) {
                continue;
            }
            let tree = git(&clone, &["ls-tree", "-r", &rev]);
            for line in String::from_utf8(tree).unwrap().lines() {
                let (meta, path) = line.split_once('\t').unwrap();
                if Manifest.claims(path) {
                    blobs.insert(meta.rsplit(' ').next().unwrap().to_string());
                }
            }
        }
        for oid in &blobs {
            let blob = git(&clone, &["cat-file", "blob", oid]);
            let table = Manifest.extract(&blob).unwrap();
            assert!(table.warnings().is_empty(), "{repo} {oid}: {:?}", table.warnings());
            let attrs = Value::from_iter(module(&table).attrs.clone());
            let mut found = Vec::new();
            exprs(&attrs, &mut found);
            for text in found {
                println!("{repo} {oid}: {text}");
                assert!(text.starts_with('{') && !text.contains(':'), "{repo} {oid}: {text}");
            }
            if let Some(depends) = attrs.get("depends") {
                assert!(depends.as_array().unwrap().iter().all(Value::is_string), "{repo} {oid}");
            }
            let bundles = attrs
                .get("assets")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|a| a.values());
            for item in bundles.filter_map(Value::as_array).flatten() {
                let directive = item.as_array().is_some_and(|tuple| {
                    (2..=3).contains(&tuple.len())
                        && tuple.iter().all(Value::is_string)
                        && DIRECTIVES.contains(&tuple[0].as_str().unwrap())
                });
                assert!(item.is_string() || directive, "{repo} {oid}: {item}");
            }
        }
        println!("{repo}: {} manifests", blobs.len());
        read += blobs.len();
    }
    assert!(read > 0);
}
