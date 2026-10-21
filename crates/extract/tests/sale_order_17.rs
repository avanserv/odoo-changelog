//! The Python extractor on the real `addons/sale/models/sale_order.py` of 17.0. Read from the
//! clone and checked against a few facts, since a stored dump would copy Odoo's LGPL texts here.

mod common;

use common::{find, odoo_blob};
use odoo_changelog_extract::{Extractor, Python};
use serde_json::json;

/// 17.0 head 4b3c29d on 2026-10-20; pinned so that the facts hold as 17.0 moves.
const BLOB: &str = "1f6a8290846b4fe5668333b0b96fcb2394c1a289";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn sale_order_17() {
    let blob = odoo_blob("odoo", BLOB);
    let table = Python.extract(&blob).unwrap();

    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
    let file = "addons/sale/models/sale_order.py";
    let fields = table.iter().filter(|s| s.kind == "field").count();
    assert_eq!(fields, 59);

    let model = find(&table, "model", "sale.order");
    assert_eq!(Python.qualify(file, model), "sale.order@sale");
    assert_eq!(
        model.attrs["_inherit"],
        json!([
            "portal.mixin",
            "product.catalog.mixin",
            "mail.thread",
            "mail.activity.mixin",
            "utm.mixin"
        ])
    );

    let partner = find(&table, "field", "sale.order#partner_id");
    assert_eq!(partner.attrs["type"], "Many2one");
    assert_eq!(partner.attrs["comodel"], "res.partner");

    let states: Vec<&str> = table
        .iter()
        .filter(|s| s.kind == "selection" && s.name.starts_with("sale.order#state:"))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        states,
        [
            "sale.order#state:cancel",
            "sale.order#state:draft",
            "sale.order#state:sale",
            "sale.order#state:sent"
        ]
    );
    assert_eq!(
        find(&table, "field", "sale.order#state").attrs["args"]["selection"],
        json!({"expr": "SALE_ORDER_STATE"})
    );

    let amounts = find(&table, "method", "sale.order#_compute_amounts");
    assert_eq!(
        amounts.attrs["decorators"],
        json!([{"name": "api.depends", "args": ["order_line.price_subtotal", "order_line.price_tax", "order_line.price_total"]}])
    );

    let constraint = find(&table, "constraint", "sale.order#date_order_conditional_required");
    assert_eq!(constraint.attrs["declared"], "_sql_constraints");
    assert!(!model.attrs.contains_key("_sql_constraints"));

    let status = find(&table, "constant", "INVOICE_STATUS");
    assert_eq!(
        Python.qualify(file, status),
        "odoo.addons.sale.models.sale_order.INVOICE_STATUS"
    );
    assert_eq!(
        status.attrs["value"],
        json!([
            ["upselling", "Upselling Opportunity"],
            ["invoiced", "Fully Invoiced"],
            ["to invoice", "To Invoice"],
            ["no", "Nothing to Invoice"]
        ])
    );
    assert!(table.iter().all(|s| s.kind != "constant" || !s.name.starts_with('_')));

    for symbol in table.iter() {
        let spanned = String::from_utf8_lossy(&blob[symbol.span.range()]);
        let name = symbol.name.rsplit(['#', ':']).next().unwrap();
        assert_eq!(spanned, name, "{symbol:?}");
    }
}
