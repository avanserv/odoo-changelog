//! A record rule re-declared as an access right under its xmlid, as master's conversion to
//! `ir.access` does, read from the community clone and checked against a few facts.

mod common;

use common::commit_deltas;
use odoo_changelog_extract::{Data, Op};

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn sale_rules_become_access_rights() {
    let deltas = commit_deltas(&Data, "e7cc76b2907d96e11b627cd1374be5362b436279");
    let d = deltas
        .iter()
        .find(|d| d.qualname == "sale.sale_order_personal_rule@sale")
        .expect("the rule's event");
    assert_eq!((d.kind.as_str(), d.op), ("access", Op::Renamed));
    let old = d.old.as_ref().unwrap();
    assert_eq!(
        (old.kind.as_str(), old.qualname.as_str()),
        ("rule", "sale.sale_order_personal_rule@sale")
    );
    assert_eq!(d.rename.as_ref().unwrap().signals, ["kind"]);
    let added = |q: &str| deltas.iter().any(|d| d.qualname == q && d.op == Op::Added);
    assert!(added("sale.sale_order_see_all_1@sale"));
}
