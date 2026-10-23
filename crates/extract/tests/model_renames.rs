//! Rename pairing on the community commits renaming `mail.channel` to `discuss.channel` (17.0)
//! and `stock.production.lot` to `stock.lot` (16.0). Read from the clone and checked against a
//! few facts, since a stored dump would copy Odoo's LGPL texts here.

mod common;

use common::commit_deltas;
use odoo_changelog_extract::{Delta, Op, Python};

/// The rename of `old`, its new qualname and confidence.
fn renamed<'d>(deltas: &'d [Delta], old: &str) -> (&'d str, f64) {
    let d = deltas
        .iter()
        .find(|d| d.op == Op::Renamed && d.old.as_ref().unwrap().qualname == old)
        .unwrap_or_else(|| panic!("{old} is not renamed"));
    (&d.qualname, d.rename.as_ref().unwrap().confidence)
}

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn mail_channel_renamed_to_discuss_channel() {
    let deltas = commit_deltas(&Python, "90cb44e1e1b6bad9c12f3bd14d1c9e340461e19e");
    let (new, confidence) = renamed(&deltas, "mail.channel@mail");
    assert_eq!(new, "discuss.channel@mail");
    assert!(confidence >= 0.9, "{confidence}");
    assert_eq!(
        renamed(&deltas, "mail.channel#name@mail").0,
        "discuss.channel#name@mail"
    );
    assert_eq!(renamed(&deltas, "mail.channel@hr").0, "discuss.channel@hr");
}

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn stock_production_lot_renamed_to_stock_lot() {
    let deltas = commit_deltas(&Python, "6a23b9bcd6aea529eb339640e6231e1aa4c9e207");
    let (new, confidence) = renamed(&deltas, "stock.production.lot@stock");
    assert_eq!(new, "stock.lot@stock");
    assert!(confidence >= 0.9, "{confidence}");
    assert_eq!(renamed(&deltas, "stock.production.lot@mrp").0, "stock.lot@mrp");
}
