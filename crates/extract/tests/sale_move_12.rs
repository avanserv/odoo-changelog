//! Move pairing on community commit 34c47ee (12.0), which moves `require_signature` and
//! `require_payment` of `sale.order` from `sale_management` to `sale`. Read from the clone and
//! checked against a few facts, since a stored dump would copy Odoo's LGPL texts here.

mod common;

use common::commit_deltas;
use odoo_changelog_extract::{Op, Python};

const COMMIT: &str = "34c47eecd30bdf66bc057bf285a51cff956cd0e8";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn sale_management_fields_move_to_sale() {
    let deltas = commit_deltas(&Python, COMMIT);

    for name in ["require_signature", "require_payment"] {
        let of = |qualname: String| deltas.iter().filter(move |d| d.qualname == qualname);
        let moved: Vec<_> = of(format!("sale.order#{name}@sale")).collect();
        assert_eq!(moved.len(), 1, "{name}: {moved:?}");
        let (d, old) = (moved[0], moved[0].old.as_ref().unwrap());
        assert_eq!(
            (d.op, d.path.as_str(), d.module.as_deref()),
            (Op::Moved, "addons/sale/models/sale.py", Some("sale"))
        );
        assert_eq!(
            (old.qualname.as_str(), old.path.as_str(), old.module.as_deref()),
            (
                format!("sale.order#{name}@sale_management").as_str(),
                "addons/sale_management/models/sale_order.py",
                Some("sale_management")
            )
        );
        assert!(d.attrs_after.is_some(), "{name}: the default changed with the move");
        assert_eq!(of(format!("sale.order#{name}@sale_management")).count(), 0, "{name}");
    }
}
