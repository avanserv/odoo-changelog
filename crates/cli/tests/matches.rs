//! Matching upgrade events to detected renames, with the real extractors on a small hand-written
//! Odoo and upgrade repository: what a match changes, and that it does not depend on whether
//! `extract` or `upgrade` runs last.

mod common;

use std::path::Path;

use common::{config, dump, git, init, odoo_changelog, setup, walk};
use rusqlite::Connection;

const MODEL: &str = "from odoo import fields, models


class ShopOrder(models.Model):
    _name = 'shop.order'
    _description = 'Shop Order'
    _order = 'date desc, id'

    cust_id = fields.Many2one('res.partner', string='Customer', required=True, index=True)
    date = fields.Datetime(string='Order Date', default=fields.Datetime.now)
";

const VIEW: &str = r#"<odoo>
    <record id="view_shop_form" model="ir.ui.view">
        <field name="name">shop.order.form</field>
        <field name="model">shop.order</field>
        <field name="arch" type="xml">
            <form><sheet><field name="cust_id"/><field name="date"/></sheet></form>
        </field>
    </record>
</odoo>
"#;

const SCRIPT: &str = r#"from odoo.upgrade import util


def migrate(cr, version):
    util.rename_field(cr, "shop.order", "cust_id", "partner_id")
    util.rename_xmlid(cr, "shop.view_shop_form", "shop.shop_order_view_form")
    util.rename_field(cr, "shop.order", "never_declared", "still_not")
"#;

fn run(config: &Path, args: &[&str]) -> String {
    let out = odoo_changelog(config, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `old -> new confidence signals` of every rename.
fn renames(db: &Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT o.qualname, s.qualname, e.confidence, e.signals FROM events e
               JOIN symbols s ON s.id = e.symbol_id JOIN symbols o ON o.id = e.old_symbol_id
              WHERE e.op = 'renamed' ORDER BY o.qualname",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok(format!(
            "{} -> {} {} {}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, String>(3)?
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn matched_renames_get_full_confidence_whatever_runs_last() {
    let (dir, mut fx, clone) = setup("matches");
    fx.git(&["checkout", "-q", "17.0"]);
    fx.commit_files(
        &[
            ("addons/shop/__manifest__.py", "{'name': 'Shop', 'depends': ['base']}\n"),
            ("addons/shop/models/shop_order.py", MODEL),
            ("addons/shop/views/shop_order_views.xml", VIEW),
        ],
        "[ADD] shop",
    );
    fx.commit_files(
        &[(
            "addons/shop/models/shop_order.py",
            &MODEL.replace("cust_id", "partner_id"),
        )],
        "[REF] shop: partner_id",
    );
    fx.commit_files(
        &[(
            "addons/shop/views/shop_order_views.xml",
            &VIEW.replace("view_shop_form", "shop_order_view_form"),
        )],
        "[REF] shop: view ids",
    );
    git(&clone, &["fetch", "-q", "origin"], 0);

    let (_, mut up) = init("matches-upgrade");
    up.commit_files(&[("migrations/shop/17.0.1.1/pre-migrate.py", SCRIPT)], "shop");
    let upgrade_clone = up.upstream.parent().unwrap().join("clone");
    git(up.upstream.parent().unwrap(), &["clone", "-q", "upstream", "clone"], 0);
    let extra = format!("\n[upgrade]\npath = {upgrade_clone:?}\n");

    let first = config(&dir, "extract-first", &clone, &extra);
    walk(&first);
    run(&first, &["extract"]);
    run(&first, &["upgrade"]);
    let last = config(&dir, "upgrade-first", &clone, &extra);
    walk(&last);
    run(&last, &["upgrade"]);
    run(&last, &["extract"]);
    let db = dir.join("extract-first.db");
    assert_eq!(dump(&db), dump(&dir.join("upgrade-first.db")));

    // The view's nodes follow it with its confidence, but no xmlid names them.
    assert_eq!(
        renames(&db),
        [
            r#"shop.order#cust_id@shop -> shop.order#partner_id@shop 1 ["args","comodel","type","upgrade"]"#,
            r#"shop.view_shop_form#field[@name='cust_id']@shop -> shop.shop_order_view_form#field[@name='cust_id']@shop 1 ["parent"]"#,
            r#"shop.view_shop_form#field[@name='date']@shop -> shop.shop_order_view_form#field[@name='date']@shop 1 ["parent"]"#,
            r#"shop.view_shop_form@shop -> shop.shop_order_view_form@shop 1 ["arch","declared","mode","model","name","record_model","type","values","members","upgrade"]"#,
        ]
    );
    let conn = Connection::open(&db).unwrap();
    let commits: Vec<(String, Option<String>)> = conn
        .prepare("SELECT new, sha FROM upgrade_events ORDER BY line")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(commits[0].1.is_some() && commits[1].1.is_some(), "{commits:?}");
    assert_eq!(commits[2], ("still_not".to_string(), None));

    let report = run(&first, &["stats", "--validate"]);
    let row = |prefix: &str| {
        report
            .lines()
            .find(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join(" ") == prefix)
            .unwrap_or_else(|| panic!("no {prefix} row in {report}"))
            .split_whitespace()
            .skip(2)
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(row("17.0 field"), "2 1 50.0% 1 1 100.0%");
    assert_eq!(row("17.0 xmlid"), "1 1 100.0% 1 1 100.0%");
    assert_eq!(row("all field"), "2 1 50.0% 1 1 100.0%");
}

#[test]
fn validate_needs_upgrade_events() {
    let (dir, _, clone) = setup("matches-none");
    let cfg = config(&dir, "none", &clone, "");
    walk(&cfg);
    let out = odoo_changelog(&cfg, &["stats", "--validate"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("run upgrade first"));
}
