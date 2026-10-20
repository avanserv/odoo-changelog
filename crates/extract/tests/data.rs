//! The data extractor on hand-written fixtures. Each `fixtures/data/<name>.xml` or `.csv` has
//! its expected output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{check, clones, find, fixtures, git, odoo_blob, render};
use odoo_changelog_extract::{Data, Extractor, Op, Registry, Symbol, SymbolTable, delta};
use serde_json::{Value, json};

fn extract(src: &str) -> SymbolTable {
    Data.extract(src.as_bytes()).unwrap()
}

/// The attrs of `symbol` qualified at `path`, as one JSON object.
fn attrs(path: &str, symbol: &Symbol) -> Value {
    Value::from_iter(Data.qualify_attrs(path, symbol).into_owned())
}

fn view(arch: &str) -> String {
    format!(
        r#"<odoo><record id="view_order_form" model="ir.ui.view"><field name="model">sale.order</field><field name="arch" type="xml">{arch}</field></record></odoo>"#
    )
}

#[test]
fn fixtures_match_expected_output() {
    let files = [fixtures("data", "xml"), fixtures("data", "csv")].concat();
    assert!(files.len() >= 8, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        let name = file.file_name().unwrap().to_str().unwrap();
        let dir = if name.ends_with(".csv") { "security" } else { "views" };
        let path = format!("addons/demo/{dir}/{name}");
        let expected = file.with_file_name(format!("{}.json", file.file_stem().unwrap().to_str().unwrap()));
        check(&expected, &render(&Data, &path, &blob));

        let first = Data.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Data.extract(&blob).unwrap().to_bytes(), "{name}");
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first, "{name}");
    }
}

#[test]
fn form_view_lists_its_named_nodes() {
    let src = view(r#"<form><sheet><group name="g"><field name="partner_id"/></group></sheet></form>"#);
    let path = "addons/sale/views/sale_views.xml";
    let table = extract(&src);
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());

    let node = find(&table, "view_node", "view_order_form#field[@name='partner_id']");
    assert_eq!(
        Data.qualify(path, node),
        "sale.view_order_form#field[@name='partner_id']@sale"
    );
    assert_eq!(attrs(path, node), json!({"count": 1, "path": "group[@name='g']"}));
    assert_eq!(&src.as_bytes()[node.span.range()], b"partner_id");

    let v = find(&table, "view", "view_order_form");
    assert_eq!(Data.qualify(path, v), "sale.view_order_form@sale");
    let a = attrs(path, v);
    assert_eq!(
        (&a["model"], &a["type"], &a["mode"]),
        (&json!("sale.order"), &json!("form"), &json!("primary"))
    );
    assert_eq!(a["arch"].as_str().unwrap().len(), 32);
    assert!(a.get("specs").is_none(), "{a}");
    assert_eq!(&src.as_bytes()[v.span.range()], b"view_order_form");
}

#[test]
fn inheriting_view_stores_its_xpaths_with_the_parent_xmlid() {
    let inheriting = |parent: &str| {
        format!(
            r#"<odoo><record id="view_order_form_ext" model="ir.ui.view">
                <field name="model">sale.order</field>
                <field name="inherit_id" ref="{parent}"/>
                <field name="arch" type="xml"><data>
                    <xpath expr="//field[@name='partner_id']" position="after"><field name="ext_id"/></xpath>
                    <field name="date_order" position="attributes"><attribute name="readonly">1</attribute></field>
                </data></field>
            </record></odoo>"#
        )
    };
    for (path, parent) in [
        ("addons/sale_ext/views/x.xml", "sale.view_order_form"),
        ("addons/sale/views/x.xml", "view_order_form"),
    ] {
        let table = extract(&inheriting(parent));
        let a = attrs(path, find(&table, "view", "view_order_form_ext"));
        assert_eq!(a["inherit_id"], "sale.view_order_form", "{path}");
        assert_eq!(
            a["specs"],
            json!([
                {"expr": "//field[@name='partner_id']", "position": "after"},
                {"target": "field[@name='date_order']", "position": "attributes"},
            ])
        );
        assert_eq!(a["mode"], "extension");
        assert!(a.get("type").is_none(), "{a}");
        let added = find(&table, "view_node", "view_order_form_ext#field[@name='ext_id']");
        assert_eq!(added.attrs["count"], 1);
    }
}

#[test]
fn unqualified_ids_take_the_files_module() {
    let src = r#"<odoo>
        <record id="view_x" model="ir.ui.view"><field name="inherit_id" ref="view_base"/><field name="arch" type="xml"><data/></field></record>
        <menuitem id="menu_x" parent="menu_root" action="action_x" groups="group_a,-base.group_b"/>
        <record id="rule_x" model="ir.rule"><field name="model_id" ref="model_x"/></record>
        <record id="base.main_company" model="res.company"><field name="name">Company</field></record>
    </odoo>"#;
    let path = "addons/sale/views/x.xml";
    let table = extract(src);
    let qualified = |kind: &str, name: &str| {
        let s = find(&table, kind, name);
        (Data.qualify(path, s), attrs(path, s))
    };
    let (qualname, a) = qualified("view", "view_x");
    assert_eq!(
        (qualname.as_str(), &a["inherit_id"]),
        ("sale.view_x@sale", &json!("sale.view_base"))
    );
    let (_, a) = qualified("menu", "menu_x");
    assert_eq!(a["parent"], "sale.menu_root");
    assert_eq!(a["action"], "sale.action_x");
    assert_eq!(a["groups"], json!(["sale.group_a", "-base.group_b"]));
    let (_, a) = qualified("rule", "rule_x");
    assert_eq!(a["model_id"], "sale.model_x");
    let (qualname, _) = qualified("record", "base.main_company");
    assert_eq!(qualname, "base.main_company@sale");

    let csv = "id,name,model_id:id,group_id:id,perm_read\naccess_x,x,model_x,group_x,1\nacc_y,y,base.model_y,base.group_y,1\n";
    let table = Data.extract(csv.as_bytes()).unwrap();
    let path = "addons/sale/security/ir.model.access.csv";
    let a = attrs(path, find(&table, "access", "access_x"));
    assert_eq!(
        (&a["model_id"], &a["group_id"]),
        (&json!("sale.model_x"), &json!("sale.group_x"))
    );
    let a = attrs(path, find(&table, "access", "acc_y"));
    assert_eq!(
        (&a["model_id"], &a["group_id"]),
        (&json!("base.model_y"), &json!("base.group_y"))
    );
}

#[test]
fn every_requested_tag_is_a_named_node() {
    let src = view(
        r#"<form><header><button name="b"/></header><notebook><page name="p"><group name="g"><div name="d"><t name="t"><field name="f"/></t></div></group></page></notebook><t t-name="tn"/></form>"#,
    );
    let table = extract(&src);
    let nodes: BTreeSet<&str> = table
        .iter()
        .filter(|s| s.kind == "view_node")
        .map(|s| s.name.as_str())
        .collect();
    let expected = [
        "button[@name='b']",
        "page[@name='p']",
        "group[@name='g']",
        "div[@name='d']",
        "t[@name='t']",
        "field[@name='f']",
        "t[@t-name='tn']",
    ];
    let expected: BTreeSet<String> = expected.iter().map(|k| format!("view_order_form#{k}")).collect();
    assert_eq!(nodes, expected.iter().map(String::as_str).collect());
    let f = find(&table, "view_node", "view_order_form#field[@name='f']");
    assert_eq!(
        f.attrs["path"],
        "page[@name='p']/group[@name='g']/div[@name='d']/t[@name='t']"
    );
}

/// `(op, qualname)` of each change from `old` to `new`, both at one path of module `sale`.
fn changes(old: &str, new: &str) -> Vec<(Op, String)> {
    let path = "addons/sale/views/x.xml";
    let (old, new) = (extract(old), extract(new));
    delta(&Data, Some((path, &old)), Some((path, &new)))
        .into_iter()
        .map(|d| (d.op, d.qualname))
        .collect()
}

#[test]
fn arch_hash_ignores_layout() {
    let base = view(r#"<form><field name="a" string="A &gt; B"/><p>Some text</p></form>"#);
    let relaid = view(
        "\n<form>\n  <!-- a comment -->\n  <field string=\"A > B\"\n         name=\"a\"/>\n  <p>\n    Some\n    text\n  </p>\n</form>\n",
    );
    assert_eq!(changes(&base, &relaid), []);
}

#[test]
fn arch_hash_sees_edits() {
    let base = view(r#"<form><field name="a" string="A"/><p>Some text</p></form>"#);
    let changed = (Op::Changed, "sale.view_order_form@sale".to_string());
    let word = view(r#"<form><field name="a" string="A"/><p>Other text</p></form>"#);
    assert_eq!(changes(&base, &word), std::slice::from_ref(&changed));
    let attribute = view(r#"<form><field name="a" string="B"/><p>Some text</p></form>"#);
    assert_eq!(changes(&base, &attribute), std::slice::from_ref(&changed));
    let renamed = view(r#"<form><field name="b" string="A"/><p>Some text</p></form>"#);
    assert_eq!(
        changes(&base, &renamed),
        [
            changed,
            (Op::Removed, "sale.view_order_form#field[@name='a']@sale".into()),
            (Op::Added, "sale.view_order_form#field[@name='b']@sale".into()),
        ]
    );
}

#[test]
fn records_keep_name_model_res_model_and_hash_the_rest() {
    let action = |help: &str, code: &str| {
        format!(
            r#"<odoo>
    <record id="action_x" model="ir.actions.act_window">
        <field name="name">Orders</field>
        <field name="res_model">sale.order</field>
        <field name="help" type="html"><p>{help}</p></field>
    </record>
    <record id="action_code" model="ir.actions.server">
        <field name="name">Run</field>
        <field name="model_id" ref="model_sale_order"/>
        <field name="code">{code}</field>
    </record>
</odoo>"#
        )
    };
    let code = "\nif records:\n    records.action_confirm()\n";
    let base = action("Create an order", code);
    let table = extract(&base);
    let a = attrs("addons/sale/data/x.xml", find(&table, "action", "action_x"));
    let keys: Vec<&String> = a.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["declared", "name", "record_model", "res_model", "values"]);
    assert_eq!((&a["name"], &a["res_model"]), (&json!("Orders"), &json!("sale.order")));
    assert_eq!(a["record_model"], "ir.actions.act_window");

    let changed = |name: &str| vec![(Op::Changed, format!("sale.{name}@sale"))];
    let help = action("Create a quotation", code);
    assert_eq!(changes(&base, &help), changed("action_x"));
    let new = &extract(&help);
    let before = find(&table, "action", "action_x");
    let after = find(new, "action", "action_x");
    let differing: Vec<&String> = before
        .attrs
        .keys()
        .filter(|k| before.attrs[*k] != after.attrs[*k])
        .collect();
    assert_eq!(differing, ["values"]);

    let reindented = base.replace("\n    <", "\n\t<").replace("<p>Create", "<p>\n    Create");
    assert_eq!(changes(&base, &reindented), []);
    let dedented = action("Create an order", "\nif records:\n  records.action_confirm()\n");
    assert_eq!(changes(&base, &dedented), changed("action_code"));
}

#[test]
fn nested_menus_take_their_parent() {
    let src = r#"<odoo>
        <menuitem id="root" name="Root">
            <menuitem id="child" name="Child" parent="elsewhere" sequence="3"/>
        </menuitem>
        <menuitem id="flat" parent="base.menu_x"/>
    </odoo>"#;
    let table = extract(src);
    let path = "addons/sale/views/menus.xml";
    let child = attrs(path, find(&table, "menu", "child"));
    assert_eq!(
        child,
        json!({"declared": "menuitem", "name": "Child", "parent": "sale.root", "sequence": "3"})
    );
    assert_eq!(attrs(path, find(&table, "menu", "flat"))["parent"], "base.menu_x");
    assert!(attrs(path, find(&table, "menu", "root")).get("parent").is_none());
}

#[test]
fn access_headers_with_id_suffixes_read_alike() {
    let rows = "access_x,x,model_x,base.group_user,1,0,0,0\n";
    let colon = format!("id,name,model_id:id,group_id:id,perm_read,perm_write,perm_create,perm_unlink\n{rows}");
    let slash = colon.replace(":id", "/id");
    let (colon, slash) = (extract(&colon), extract(&slash));
    let x = find(&colon, "access", "access_x");
    assert_eq!(x.attrs, find(&slash, "access", "access_x").attrs);
    assert_eq!(x.attrs["model_id"], "model_x");
}

#[test]
fn malformed_files_give_a_partial_table_and_a_warning() {
    let read = |stem: &str| {
        let file = fixtures("data", "xml")
            .into_iter()
            .find(|f| f.file_stem().unwrap() == stem)
            .unwrap();
        Data.extract(&std::fs::read(file).unwrap()).unwrap()
    };
    let malformed = read("malformed");
    let names: Vec<&str> = malformed.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["menu_demo_kept", "demo_first"]);
    assert_eq!(malformed.warnings().len(), 1, "{:?}", malformed.warnings());
    assert!(
        malformed.warnings()[0].starts_with("line 12: "),
        "{:?}",
        malformed.warnings()
    );

    let foreign = read("foreign");
    assert_eq!(foreign.iter().count(), 0);
    assert!(foreign.warnings().is_empty(), "{:?}", foreign.warnings());

    for empty in ["", "  \n", "\u{FEFF}"] {
        let table = extract(empty);
        assert_eq!(table.iter().count(), 0);
        assert!(table.warnings().is_empty(), "{empty:?}: {:?}", table.warnings());
    }
    let comment = extract("<?xml version=\"1.0\"?>\n<!-- nothing -->\n");
    assert_eq!(
        (comment.iter().count(), comment.warnings()),
        (0, &["line 1: no root element".to_string()][..])
    );
    let headerless = extract("access_x,x,model_x,base.group_user,1,0,0,0\n");
    assert_eq!(headerless.iter().count(), 0);
    assert_eq!(headerless.warnings(), ["line 1: no id column; rows ignored"]);
}

#[test]
fn builtin_registry_gives_data_files_one_claimant() {
    let registry = Registry::builtin();
    let claimants =
        |path: &str| -> Vec<&str> { registry.iter().filter(|e| e.claims(path)).map(|e| e.name()).collect() };
    for path in ["addons/sale/views/x.xml", "addons/sale/security/ir.model.access.csv"] {
        assert_eq!(claimants(path), ["data"], "{path}");
    }
    assert!(claimants("addons/sale/static/src/x.xml").is_empty());
}

/// Blobs of the community clone at the 17.0, master and 12.0 heads on 2026-10-20, pinned so that
/// the facts hold as the branches move.
const SALE_ORDER_VIEWS_17: &str = "35c9bb97f1f6e2112c7c624ab43935b2ca491757";
const SALE_STOCK_VIEWS_17: &str = "748412b92ba9a595975ca0fb134bcba3aa0c3ffb";
const SALE_MENUS_17: &str = "68e3a920b7a7babd1e8b3d0ca89df6f61e19e30e";
const SALE_ACCESS_17: &str = "58d8711886d59eff33d3fd4f4060aa25e215e5e4";
const SALE_ACCESS_MASTER: &str = "6d3f3cb51004eb135bf380b4bde080f6c35aae68";
const SALE_REPORT_12: &str = "9c9addca79b7c477d8a5c76e16ee6e8581125540";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn real_sale_data_files() {
    let read = |oid: &str| {
        let table = Data.extract(&odoo_blob("odoo", oid)).unwrap();
        assert!(table.warnings().is_empty(), "{oid}: {:?}", table.warnings());
        table
    };
    let sale = "addons/sale/views/sale_order_views.xml";

    let views = read(SALE_ORDER_VIEWS_17);
    let nodes: Vec<&Symbol> = views
        .iter()
        .filter(|s| s.kind == "view_node" && s.name.starts_with("view_order_form#"))
        .collect();
    assert_eq!(nodes.len(), 123);
    let occurrences: u64 = nodes.iter().map(|n| n.attrs["count"].as_u64().unwrap()).sum();
    assert_eq!(occurrences, 187);
    let count = |key: &str| find(&views, "view_node", &format!("view_order_form#{key}")).attrs["count"].clone();
    assert_eq!(count("field[@name='partner_id']"), 1);
    assert_eq!(count("field[@name='date_order']"), 2);
    assert_eq!(count("button[@name='action_confirm']"), 2);
    assert_eq!(count("button[@id='action_confirm']"), 1);
    assert_eq!(count("field[@name='product_id']"), 3);
    assert_eq!(count("page[@name='order_lines']"), 1);
    let form = attrs(sale, find(&views, "view", "view_order_form"));
    assert_eq!((&form["type"], &form["mode"]), (&json!("form"), &json!("primary")));

    let stock = read(SALE_STOCK_VIEWS_17);
    let ext = find(&stock, "view", "view_order_form_inherit_sale_stock");
    let path = "addons/sale_stock/views/sale_order_views.xml";
    assert_eq!(
        Data.qualify(path, ext),
        "sale_stock.view_order_form_inherit_sale_stock@sale_stock"
    );
    let a = attrs(path, ext);
    assert_eq!(a["inherit_id"], "sale.view_order_form");
    assert_eq!(
        a["specs"],
        json!([
            {"expr": "//button[@name='action_view_invoice']", "position": "before"},
            {"expr": "//group[@name='sale_shipping']", "position": "attributes"},
            {"expr": "//label[@for='commitment_date']", "position": "before"},
            {"expr": "//span[@name='expected_date_span']", "position": "attributes"},
            {"expr": "//div[@name='commitment_date_div']", "position": "replace"},
            {"target": "field[@name='effective_date']", "position": "after"},
            {"expr": "//page[@name='other_information']//field[@name='expected_date']", "position": "after"},
            {"expr": "//field[@name='order_line']/form/group/group/field[@name='analytic_distribution']", "position": "before"},
            {"expr": "//field[@name='order_line']/tree/field[@name='analytic_distribution']", "position": "after"},
        ])
    );

    let menus = read(SALE_MENUS_17);
    assert_eq!(menus.iter().filter(|s| s.kind == "menu").count(), 36);
    let path = "addons/sale/views/sale_menus.xml";
    let root = attrs(path, find(&menus, "menu", "sale_menu_root"));
    assert_eq!(root["active"], "False");
    assert!(root.get("web_icon").is_some(), "{root}");
    let quotations = find(&menus, "menu", "menu_sale_quotations");
    assert_eq!(Data.qualify(path, quotations), "sale.menu_sale_quotations@sale");
    let a = attrs(path, quotations);
    assert_eq!(a["parent"], "sale.sale_order_menu");
    assert_eq!(a["action"], "sale.action_quotations_with_onboarding");
    assert_eq!(a["groups"], json!(["sales_team.group_sale_salesman"]));
    assert_eq!(a["sequence"], "10");

    let access = read(SALE_ACCESS_17);
    let path = "addons/sale/security/ir.model.access.csv";
    let order = find(&access, "access", "access_sale_order");
    assert_eq!(order.line, 45);
    assert_eq!(Data.qualify(path, order), "sale.access_sale_order@sale");
    let a = attrs(path, order);
    assert_eq!(a["model_id"], "sale.model_sale_order");
    assert_eq!(a["group_id"], "sales_team.group_sale_salesman");
    let perms = ["perm_read", "perm_write", "perm_create", "perm_unlink"].map(|p| a[p].as_str().unwrap());
    assert_eq!(perms, ["1", "1", "1", "0"]);

    let access = read(SALE_ACCESS_MASTER);
    let path = "addons/sale/security/ir.access.csv";
    let portal = find(&access, "access", "sale_order_rule_portal");
    assert_eq!(portal.line, 28);
    let a = attrs(path, portal);
    assert_eq!(
        (&a["model"], &a["group_id"], &a["operation"]),
        (&json!("sale.order"), &json!("base.group_portal"), &json!("r"))
    );

    let report = read(SALE_REPORT_12);
    let path = "addons/sale/report/sale_report.xml";
    let order = find(&report, "action", "action_report_saleorder");
    assert_eq!(Data.qualify(path, order), "sale.action_report_saleorder@sale");
    let a = attrs(path, order);
    assert_eq!(
        (&a["declared"], &a["name"]),
        (&json!("report"), &json!("Quotation / Order"))
    );
    assert_eq!(
        report
            .iter()
            .filter(|s| s.attrs.get("declared") == Some(&json!("report")))
            .count(),
        2
    );
}

/// Files whose warnings are right: enterprise 12.0's `mrp_account` access CSV has no header, so
/// Odoo reads its first row as one.
const KNOWN_WARNINGS: [&str; 1] = ["enterprise origin/12.0:mrp_account/security/ir.model.access.csv:"];

/// Every data file reads without a warning, and every inheriting view with an arch lists its
/// specs. An empty list is legitimate: a template that only toggles with `active`, or a primary
/// copy of its parent; the sweep prints how many there are.
#[test]
#[ignore = "reads every Odoo clone in config/odoo-changelog.toml"]
fn every_data_file_at_the_tips_reads_cleanly() {
    let mut read = 0;
    for (repo, clone) in clones() {
        let refs = git(
            &clone,
            &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin/"],
        );
        let refs = String::from_utf8(refs).unwrap();
        let mut blobs = BTreeMap::new();
        for branch in ["12.0", "17.0", "master"] {
            let rev = format!("origin/{branch}");
            if !refs.lines().any(|r| r == rev) {
                continue;
            }
            let tree = git(&clone, &["ls-tree", "-r", &rev]);
            for line in String::from_utf8(tree).unwrap().lines() {
                let (meta, path) = line.split_once('\t').unwrap();
                if Data.claims(path) {
                    blobs.insert(meta.rsplit(' ').next().unwrap().to_string(), format!("{rev}:{path}"));
                }
            }
        }
        let (mut kinds, mut specless) = (BTreeMap::<String, usize>::new(), 0);
        let mut warned = Vec::new();
        for (oid, at) in &blobs {
            let blob = git(&clone, &["cat-file", "blob", oid]);
            let table = Data.extract(&blob).unwrap();
            if !table.warnings().is_empty() {
                warned.push(format!("{repo} {at}: {:?}", table.warnings()));
            }
            for symbol in table.iter() {
                *kinds.entry(symbol.kind.clone()).or_default() += 1;
                let inheriting = symbol.attrs.contains_key("inherit_id") && symbol.attrs.contains_key("arch");
                if symbol.kind == "view" && inheriting {
                    let specs = symbol.attrs.get("specs").and_then(Value::as_array);
                    assert!(specs.is_some(), "{repo} {at}: {}", symbol.name);
                    specless += specs.is_some_and(Vec::is_empty) as usize;
                }
            }
        }
        warned.retain(|w| !KNOWN_WARNINGS.iter().any(|k| w.starts_with(k)));
        for warning in &warned {
            println!("{warning}");
        }
        println!(
            "{repo}: {} files, {specless} inheriting views without specs, {kinds:?}",
            blobs.len()
        );
        assert!(warned.is_empty(), "{repo}: {} files gave warnings", warned.len());
        read += blobs.len();
    }
    assert!(read > 0);
}
