//! The OWL template extractor on hand-written fixtures. Each `fixtures/owl/<name>.xml` has its
//! expected output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{batch, check, clones, edit, find, fixtures, git, odoo_blob, render};
use odoo_changelog_extract::{Extractor, Op, Owl, Registry, Symbol, SymbolTable, delta};
use serde_json::{Value, json};

const PATH: &str = "addons/demo/static/src/x.xml";

/// Where a fixture is qualified: the `views` directory of the `demo` addon.
fn fixture_path(file: &Path) -> String {
    format!(
        "addons/demo/static/src/views/{}",
        file.file_name().unwrap().to_str().unwrap()
    )
}

fn fixture(stem: &str) -> Vec<u8> {
    let file = fixtures("owl", "xml")
        .into_iter()
        .find(|f| f.file_stem().unwrap() == stem)
        .unwrap();
    std::fs::read(file).unwrap()
}

fn extract(src: &str) -> SymbolTable {
    Owl.extract(src.as_bytes()).unwrap()
}

fn attrs(symbol: &Symbol) -> Value {
    Value::from_iter(symbol.attrs.clone())
}

#[test]
fn fixtures_match_expected_output() {
    let files = fixtures("owl", "xml");
    assert!(files.len() >= 8, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        check(&file.with_extension("json"), &render(&Owl, &fixture_path(&file), &blob));

        let first = Owl.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Owl.extract(&blob).unwrap().to_bytes(), "{}", file.display());
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first);
    }
}

#[test]
fn template_inherit_stores_parent_and_mode() {
    let src = r#"<templates><t t-name="demo.Picking" t-inherit="web.FormView" t-inherit-mode="primary"><xpath expr="//div[@t-ref='root']" position="inside"><span t-ref="extra"/></xpath></t><t t-inherit="web.FormView" t-inherit-mode="extension"><xpath expr="//t[@t-set-slot='layout-buttons']" position="replace"/></t><t t-name="demo.Default" t-inherit="web.FormView"><xpath expr="//div" position="after"/></t></templates>"#;
    let table = extract(src);
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());

    let picking = find(&table, "owl_template", "demo.Picking");
    assert_eq!(Owl.qualify(PATH, picking), "demo.Picking@demo");
    let a = attrs(picking);
    assert_eq!((&a["inherit"], &a["mode"]), (&json!("web.FormView"), &json!("primary")));
    assert_eq!(
        a["specs"],
        json!([{"expr": "//div[@t-ref='root']", "position": "inside"}])
    );
    assert_eq!(&src.as_bytes()[picking.span.range()], b"demo.Picking");
    let node = find(&table, "owl_node", "demo.Picking#span[@t-ref='extra']");
    assert_eq!(Owl.qualify(PATH, node), "demo.Picking#span[@t-ref='extra']@demo");

    let extension = find(&table, "owl_extension", "web.FormView");
    assert_eq!(Owl.qualify(PATH, extension), "web.FormView@demo");
    assert_eq!(attrs(extension)["mode"], "extension");
    assert_eq!(&src.as_bytes()[extension.span.range()], b"web.FormView");

    assert_eq!(attrs(find(&table, "owl_template", "demo.Default"))["mode"], "primary");
}

#[test]
fn named_nodes_cover_t_ref_and_slots() {
    let src = r#"<templates><t t-name="demo.Keys">
        <div t-ref="root" class="o_root">
            <t t-set-slot="body"><button name="go" id="go_btn"/></t>
            <t t-set-slot="body"/>
            <t t-name="demo.Nested"/>
        </div>
    </t></templates>"#;
    let table = extract(src);
    let nodes: Vec<(String, Value)> = table
        .iter()
        .filter(|s| s.kind == "owl_node")
        .map(|s| (s.name.clone(), attrs(s)))
        .collect();
    let root = "div[@t-ref='root']";
    assert_eq!(
        nodes,
        [
            (
                "demo.Keys#button[@id='go_btn']".into(),
                json!({"count": 1, "path": format!("{root}/t[@t-set-slot='body']")})
            ),
            (
                "demo.Keys#button[@name='go']".into(),
                json!({"count": 1, "path": format!("{root}/t[@t-set-slot='body']")})
            ),
            (format!("demo.Keys#{root}"), json!({"count": 1})),
            (
                "demo.Keys#t[@t-name='demo.Nested']".into(),
                json!({"count": 1, "path": root})
            ),
            (
                "demo.Keys#t[@t-set-slot='body']".into(),
                json!({"count": 2, "path": root})
            ),
        ]
    );
    let slot = find(&table, "owl_node", "demo.Keys#t[@t-set-slot='body']");
    assert_eq!(&src.as_bytes()[slot.span.range()], b"body");
    assert_eq!(slot.line, 3);
}

#[test]
fn t_extend_forms_read_as_template_and_extension() {
    let table = Owl.extract(&fixture("legacy")).unwrap();
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
    let clone = attrs(find(&table, "owl_template", "DemoBills.buttons"));
    assert_eq!(
        (&clone["declared"], &clone["mode"], &clone["inherit"]),
        (&json!("t-extend"), &json!("primary"), &json!("DemoList.buttons"))
    );
    assert_eq!(
        clone["specs"],
        json!([{"jquery": "button.o_list_button_add", "operation": "after"}])
    );
    let bare = attrs(find(&table, "owl_extension", "DemoOrderline"));
    assert_eq!(
        (&bare["declared"], &bare["mode"]),
        (&json!("t-extend"), &json!("extension"))
    );
    assert_eq!(
        bare["specs"],
        json!([
            {"jquery": ".info-list", "operation": "append"},
            {"jquery": ".product-name", "operation": "attributes"},
            {"jquery": ".price"},
        ])
    );
    find(&table, "owl_node", "DemoOrderline#li[@name='note']");
    find(&table, "owl_node", "DemoBills.buttons#button[@id='demo_upload']");
    assert!(!table.iter().any(|s| s.name.contains("attribute")));
}

#[test]
fn reformatting_templates_gives_no_event() {
    let src = String::from_utf8(fixture("primary")).unwrap();
    let reformatted = edit(
        &src,
        &[
            ("    ", "  "),
            ("<!-- The thing itself. -->", ""),
            ("<li>item</li>", "<!-- one --><li>\n item\n </li>"),
            (
                r#"class="o_demo_thing" t-ref="root""#,
                r#"t-ref="root"   class="o_demo_thing""#,
            ),
            (
                r#"t-name="demo.Plain" t-inherit="demo.Thing""#,
                r#"t-inherit="demo.Thing" t-inherit-mode="primary" t-name="demo.Plain""#,
            ),
            ("Count &gt; 0", "Count > 0"),
        ],
    );
    let (old, new) = (extract(&src), extract(&reformatted));
    assert!(new.warnings().is_empty(), "{:?}", new.warnings());
    assert_eq!(delta(&Owl, &[(Some((PATH, &old)), Some((PATH, &new)))]), []);

    let removed = extract(&edit(&src, &[(r#"<ul t-ref="list"><li>item</li></ul>"#, "")]));
    let changes: Vec<(String, String, Op)> = delta(&Owl, &[(Some((PATH, &old)), Some((PATH, &removed)))])
        .into_iter()
        .map(|d| (d.kind, d.qualname, d.op))
        .collect();
    assert_eq!(
        changes,
        [
            (
                "owl_node".into(),
                "demo.ThingList#ul[@t-ref='list']@demo".into(),
                Op::Removed
            ),
            ("owl_template".into(), "demo.ThingList@demo".into(), Op::Changed),
        ]
    );
}

#[test]
fn broken_templates_give_a_partial_table_and_a_warning() {
    let malformed = Owl.extract(&fixture("malformed")).unwrap();
    let templates: Vec<&str> = malformed
        .iter()
        .filter(|s| s.kind == "owl_template")
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(templates, ["demo.First", "demo.Second"]);
    assert_eq!(malformed.warnings().len(), 1, "{:?}", malformed.warnings());
    assert!(
        malformed.warnings()[0].starts_with("line 10: "),
        "{:?}",
        malformed.warnings()
    );

    let odd = Owl.extract(&fixture("odd")).unwrap();
    assert_eq!(
        odd.warnings(),
        [
            "line 3: <div> without t-name ignored",
            "line 5: duplicate owl_template demo.Twice dropped",
            "line 6: t-inherit-mode override is not primary or extension; ignored",
            "line 7: primary template without t-name ignored",
        ]
    );
    find(&odd, "owl_node", "demo.Twice#span[@name='first']");
    assert!(!odd.contains("owl_node", "demo.Twice#span[@name='second']"));

    let foreign = Owl.extract(&fixture("foreign")).unwrap();
    assert_eq!((foreign.iter().count(), foreign.warnings().len()), (0, 0));

    for empty in ["", "  \n", "\u{FEFF}"] {
        let table = extract(empty);
        assert_eq!((table.iter().count(), table.warnings().len()), (0, 0), "{empty:?}");
    }
    assert_eq!(extract("<!-- nothing -->").warnings(), ["line 1: no root element"]);
    let mut noise = 0x2545_f491_4f6c_dd1du64;
    let random: Vec<u8> = (0..4096)
        .map(|_| {
            noise ^= noise << 13;
            noise ^= noise >> 7;
            noise ^= noise << 17;
            (noise >> 24) as u8
        })
        .collect();
    assert!(Owl.extract(&random).is_ok());
}

#[test]
fn a_templates_own_element_names_nodes_and_an_empty_parent_is_none() {
    let table = extract(
        r#"<templates><div t-name="demo.Root" t-ref="root" class="o"><b name="c"/></div><t t-inherit="" t-inherit-mode="extension"/><t t-name="demo.Empty" t-inherit=""/></templates>"#,
    );
    assert_eq!(
        attrs(find(&table, "owl_node", "demo.Root#div[@t-ref='root']")),
        json!({"count": 1})
    );
    assert_eq!(
        attrs(find(&table, "owl_node", "demo.Root#b[@name='c']")),
        json!({"count": 1, "path": "div[@t-ref='root']"})
    );
    assert!(!table.iter().any(|s| s.name.contains("t-name='demo.Root'")));
    assert!(!table.iter().any(|s| s.kind == "owl_extension"));
    assert!(!find(&table, "owl_template", "demo.Empty").attrs.contains_key("inherit"));
    assert_eq!(table.warnings(), ["line 1: <t> without t-name ignored"]);
}

#[test]
fn many_warnings_are_summed_up() {
    let src = format!("<templates>{}</templates>", "<t t-name=\"a\"/>\n".repeat(50));
    let table = extract(&src);
    assert_eq!(table.warnings().len(), 11, "{:?}", table.warnings());
    assert_eq!(table.warnings()[0], "line 2: duplicate owl_template a dropped");
    assert_eq!(table.warnings()[10], "39 more warnings");
}

/// Every walk stays within a rayon worker's 2 MiB stack: complete documents just under the
/// DOM's depth limit go through every walk, and deeper ones stop with a warning.
#[test]
fn deep_nesting_reads_within_a_worker_stack() {
    let nest = |n: usize, open: &dyn Fn(usize) -> String, close: &str| {
        format!("{}{}", (0..n).map(open).collect::<String>(), close.repeat(n))
    };
    let complete = [
        (
            format!(
                "<templates><t t-name=\"a\">{}</t></templates>",
                nest(254, &|i| format!("<div t-ref=\"r{i}\">"), "</div>")
            ),
            "a#div[@t-ref='r253']",
        ),
        (
            format!(
                "<templates><t t-inherit=\"a\" t-inherit-mode=\"extension\"><xpath expr=\"//b\">{}</xpath></t></templates>",
                nest(253, &|i| format!("<div name=\"d{i}\">"), "</div>")
            ),
            "a#div[@name='d252']",
        ),
        (
            format!(
                "<templates><t t-extend=\"a\"><t t-jquery=\"b\" t-operation=\"append\">{}</t></t></templates>",
                nest(253, &|i| format!("<li id=\"i{i}\">"), "</li>")
            ),
            "a#li[@id='i252']",
        ),
        (
            format!(
                "<t t-name=\"a\">{}</t>",
                nest(255, &|i| format!("<div t-ref=\"r{i}\">"), "</div>")
            ),
            "a#div[@t-ref='r254']",
        ),
    ];
    let n = 20_000;
    let deep = |open: &str, close: &str| format!("{}{}", open.repeat(n), close.repeat(n));
    let too_deep = [
        format!(
            "<templates><t t-name=\"a\">{}</t></templates>",
            deep("<div t-ref=\"r\">", "</div>")
        ),
        format!("<t t-name=\"a\">{}</t>", deep("<data>", "</data>")),
        deep("<templates>", "</templates>"),
    ];
    let (complete, too_deep) = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let read = |doc: &String| Owl.extract(doc.as_bytes()).unwrap();
            let complete: Vec<_> = complete.iter().map(|(doc, deepest)| (read(doc), *deepest)).collect();
            (complete, too_deep.map(|doc| read(&doc).warnings().to_vec()))
        })
        .unwrap()
        .join()
        .unwrap();
    for (table, deepest) in complete {
        assert!(table.warnings().is_empty(), "{:?}", table.warnings());
        find(&table, "owl_node", deepest);
    }
    for warnings in too_deep {
        assert_eq!(warnings, ["line 1: nesting deeper than 256"]);
    }
}

/// Inputs that repeat a long text in every symbol they give, each over 1 MiB. Bounding keeps
/// most well under the budget's 24 bytes per input byte, so they give no warning; only the one
/// that grows faster by design reaches the budget, which caps it.
#[test]
fn long_names_stay_linear() {
    let long = "L".repeat(5000);
    let k = 300;
    let lines = |f: &dyn Fn(usize) -> String| (0..k).map(f).collect::<String>();
    let shapes = [
        (
            format!(
                "<templates>{}</templates>",
                lines(&|i| format!(
                    "<t t-inherit=\"{long}\" t-inherit-mode=\"extension\"><xpath expr=\"//a\"><b name=\"n{i}\"/><b id=\"n{i}\"/></xpath></t>\n"
                ))
            ),
            false,
        ),
        (
            format!(
                "<templates><t t-name=\"{long}\">{}</t></templates>",
                (0..80_000).map(|i| format!("<a id=\"{i}\"/>")).collect::<String>()
            ),
            false,
        ),
        (
            format!(
                "<templates><t t-name=\"a\">{}{}{}</t></templates>",
                (0..250).map(|i| format!("<g name=\"{long}{i}\">")).collect::<String>(),
                (0..80_000).map(|i| format!("<a id=\"{i}\"/>")).collect::<String>(),
                "</g>".repeat(250)
            ),
            false,
        ),
        (
            format!(
                "<templates><t t-name=\"{}\"><g name=\"{}\">{}</g></t></templates>",
                "T".repeat(250),
                "G".repeat(240),
                (0..16_000)
                    .map(|i| format!("<a name=\"{i}\" id=\"{i}\" t-ref=\"{i}\" t-name=\"{i}\" t-set-slot=\"{i}\"/>"))
                    .collect::<String>()
            ),
            true,
        ),
        (
            format!(
                "<templates>{}</templates>",
                lines(&|i| format!(
                    "<t t-extend=\"{long}\"><t t-jquery=\"a\" t-operation=\"append\"><b name=\"n{i}\"/></t></t>\n"
                ))
            ),
            false,
        ),
    ];
    // JSON escapes a control character in 6 bytes and a quote in 2, in the owner and path every
    // node repeats.
    let escaped = |text: &str| {
        let leaves: String = (0..100_000).map(|i| format!("<b name=\"{i}\"/>")).collect();
        let (owner, ancestor) = (text.repeat(255), text.repeat(240));
        let src = format!("<templates><t t-name=\"{owner}\"><g name=\"{ancestor}\">{leaves}</g></t></templates>");
        (src, true)
    };
    let shapes = [shapes.to_vec(), vec![escaped("\u{1}"), escaped("&quot;")]].concat();
    for (src, budgeted) in shapes {
        let table = extract(&src);
        let size = table.to_bytes().len();
        let head = &src[..80];
        assert!(src.len() > 1 << 20, "{} bytes for {head}", src.len());
        assert!(size < 30 * src.len(), "{size} bytes from {} for {head}", src.len());
        let warnings = table.warnings();
        match budgeted {
            false => assert!(warnings.is_empty(), "{warnings:?} for {head}"),
            true => assert!(
                warnings.len() == 1 && warnings[0].contains("output budget exceeded"),
                "{warnings:?} for {head}"
            ),
        }
    }
}

#[test]
fn every_symbol_spans_its_name() {
    for file in fixtures("owl", "xml") {
        let blob = std::fs::read(&file).unwrap();
        for symbol in Owl.extract(&blob).unwrap().iter() {
            let spanned = String::from_utf8_lossy(&blob[symbol.span.range()]).into_owned();
            let name = symbol.name.as_str();
            let ok = match symbol.kind.as_str() {
                "owl_template" => name == spanned,
                "owl_extension" => name.split('[').next() == Some(spanned.as_str()),
                "owl_node" => name.contains(&format!("='{spanned}']")),
                kind => panic!("unknown kind {kind}"),
            };
            assert!(ok, "{}: {spanned:?} for {symbol:?}", file.display());
        }
    }
}

#[test]
fn builtin_registry_gives_templates_and_stylesheets_one_claimant() {
    let registry = Registry::builtin();
    let claimants =
        |path: &str| -> Vec<&str> { registry.iter().filter(|e| e.claims(path)).map(|e| e.name()).collect() };
    assert_eq!(claimants("addons/sale/static/src/x.xml"), ["owl"]);
    assert_eq!(claimants("addons/sale/views/x.xml"), ["data"]);
    for path in [
        "addons/sale/static/src/x.scss",
        "addons/web/static/lib/bootstrap/scss/_variables.scss",
    ] {
        assert_eq!(claimants(path), ["scss"], "{path}");
    }
    for path in ["addons/sale/static/tests/x.xml", "addons/web/static/lib/qweb/a.xml"] {
        assert!(claimants(path).is_empty(), "{path}");
    }
}

/// Blobs of the community clone, pinned so that the facts hold as the branches move: the 17.0
/// head on 2026-10-20, and 12.0's.
const FORM_CONTROLLER_17: &str = "7f02391ff4e670d0f7b9cb106fa570666487b8d5";
const MAIL_FORM_CONTROLLER_17: &str = "10ae03b7656dfe5b35c2c3661181047dc6a56f1a";
const SUBCONTRACTING_PICKING_17: &str = "6df983bd8c7e387229aab0d962b50fdc586a8959";
const LOYALTY_TEMPLATES_17: &str = "52503255b91cc0a06b5c527b8531cfde6895fa28";
const POS_RESTAURANT_NOTES_12: &str = "63bd9d8a1c3076a3e6c610d6314046549f94d9ab";
const BILLS_TREE_UPLOAD_12: &str = "449f3da8c385f5f648308b0991d3b57bf0703204";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn real_templates() {
    let read = |oid: &str| {
        let blob = odoo_blob("odoo", oid);
        let table = Owl.extract(&blob).unwrap();
        assert!(table.warnings().is_empty(), "{oid}: {:?}", table.warnings());
        (blob, table)
    };

    let path = "addons/web/static/src/views/form/form_controller.xml";
    let (blob, form) = read(FORM_CONTROLLER_17);
    for name in ["web.FormView", "web.FormView.Buttons"] {
        let template = find(&form, "owl_template", name);
        assert!(!template.attrs.contains_key("inherit"), "{name}");
        assert_eq!(&blob[template.span.range()], name.as_bytes());
    }
    assert_eq!(
        Owl.qualify(path, find(&form, "owl_template", "web.FormView")),
        "web.FormView@web"
    );
    let root = find(&form, "owl_node", "web.FormView#div[@t-ref='root']");
    assert_eq!(attrs(root), json!({"count": 1}));
    let slot = find(
        &form,
        "owl_node",
        "web.FormView#t[@t-set-slot='control-panel-create-button']",
    );
    assert_eq!(attrs(slot), json!({"count": 2, "path": "div[@t-ref='root']"}));
    find(&form, "owl_node", "web.FormView.Buttons#div[@t-ref='cpButtons']");

    let path = "addons/mail/static/src/views/web/form/form_controller.xml";
    let (_, mail) = read(MAIL_FORM_CONTROLLER_17);
    let extension = find(&mail, "owl_extension", "web.FormView");
    assert_eq!(Owl.qualify(path, extension), "web.FormView@mail");
    let a = attrs(extension);
    assert_eq!(
        (&a["inherit"], &a["mode"]),
        (&json!("web.FormView"), &json!("extension"))
    );
    assert_eq!(
        a["specs"],
        json!([{"expr": "//Layout/t[@t-component='props.Renderer']", "position": "attributes"}])
    );
    assert!(!mail.iter().any(|s| s.kind == "owl_node"));

    let path = "addons/mrp_subcontracting/static/src/subcontracting_portal/picking_form_controller.xml";
    let (_, picking) = read(SUBCONTRACTING_PICKING_17);
    let template = find(&picking, "owl_template", "mrp_subcontracting.PickingFormController");
    assert_eq!(
        Owl.qualify(path, template),
        "mrp_subcontracting.PickingFormController@mrp_subcontracting"
    );
    let a = attrs(template);
    assert_eq!((&a["inherit"], &a["mode"]), (&json!("web.FormView"), &json!("primary")));
    assert_eq!(
        a["specs"],
        json!([{"expr": "//t[@t-set-slot='control-panel-additional-actions']", "position": "replace"}])
    );

    let (_, loyalty) = read(LOYALTY_TEMPLATES_17);
    let list = attrs(find(&loyalty, "owl_template", "loyalty.LoyaltyListRenderer"));
    assert_eq!(
        (&list["inherit"], &list["mode"]),
        (&json!("web.ListRenderer"), &json!("primary"))
    );
    assert_eq!(
        list["specs"][0],
        json!({"target": "t[@t-call='web.ActionHelper']", "position": "replace"})
    );
    let helper = find(&loyalty, "owl_template", "loyalty.LoyaltyActionHelper");
    assert!(!helper.attrs.contains_key("inherit"));

    let path = "addons/pos_restaurant/static/src/xml/notes.xml";
    let (_, notes) = read(POS_RESTAURANT_NOTES_12);
    let orderline = find(&notes, "owl_extension", "Orderline");
    assert_eq!(Owl.qualify(path, orderline), "Orderline@pos_restaurant");
    let a = attrs(orderline);
    assert_eq!(
        (&a["declared"], &a["mode"], &a["specs"]),
        (
            &json!("t-extend"),
            &json!("extension"),
            &json!([{"jquery": ".info-list", "operation": "append"}])
        )
    );
    find(&notes, "owl_template", "OrderlineNoteButton");

    let path = "addons/account/static/src/xml/bills_tree_upload_views.xml";
    let (_, bills) = read(BILLS_TREE_UPLOAD_12);
    let buttons = find(&bills, "owl_template", "BillsListView.buttons");
    assert_eq!(Owl.qualify(path, buttons), "BillsListView.buttons@account");
    let a = attrs(buttons);
    assert_eq!(
        (&a["declared"], &a["inherit"], &a["mode"], &a["specs"]),
        (
            &json!("t-extend"),
            &json!("ListView.buttons"),
            &json!("primary"),
            &json!([{"jquery": "button.o_list_button_add", "operation": "after"}])
        )
    );
    find(&bills, "owl_template", "BillsListView.upload_button");
}

/// Every claimed template file at the tips reads, deterministically and with spans inside the
/// blob. Prints the symbols per kind and the first files with warnings, without asserting on
/// them: three 16.0 files are known to be malformed.
#[test]
#[ignore = "reads every Odoo clone in config/odoo-changelog.toml"]
fn every_claimed_template_at_the_tips_reads() {
    let mut read = 0;
    for (repo, clone) in clones() {
        let refs = git(
            &clone,
            &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin/"],
        );
        let refs = String::from_utf8(refs).unwrap();
        let mut blobs = BTreeMap::new();
        for branch in ["12.0", "14.0", "16.0", "17.0", "master"] {
            let rev = format!("origin/{branch}");
            if !refs.lines().any(|r| r == rev) {
                continue;
            }
            let tree = git(&clone, &["ls-tree", "-r", "-z", &rev]);
            for line in String::from_utf8(tree).unwrap().split_terminator('\0') {
                let (meta, path) = line.split_once('\t').unwrap();
                if Owl.claims(path) {
                    let oid = meta.rsplit(' ').next().unwrap().to_string();
                    blobs.entry(oid).or_insert_with(|| format!("{rev}:{path}"));
                }
            }
        }
        let oids: Vec<&str> = blobs.keys().map(String::as_str).collect();
        let (mut kinds, mut warned) = (BTreeMap::<String, usize>::new(), Vec::new());
        for (blob, (oid, at)) in batch(&clone, &oids).into_iter().zip(&blobs) {
            let table = Owl.extract(&blob).unwrap();
            assert_eq!(table.to_bytes(), Owl.extract(&blob).unwrap().to_bytes(), "{repo} {at}");
            for symbol in table.iter() {
                *kinds.entry(symbol.kind.clone()).or_default() += 1;
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
