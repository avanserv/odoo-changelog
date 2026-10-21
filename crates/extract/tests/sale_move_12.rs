//! Move pairing on community commit 34c47ee (12.0), which moves `require_signature` and
//! `require_payment` of `sale.order` from `sale_management` to `sale`. Read from the clone and
//! checked against a few facts, since a stored dump would copy Odoo's LGPL texts here.

mod common;

use common::{clones, git};
use odoo_changelog_extract::{Extractor, Op, Python, Side, SymbolTable, delta};

const COMMIT: &str = "34c47eecd30bdf66bc057bf285a51cff956cd0e8";

#[test]
#[ignore = "reads the Odoo community clone in config/odoo-changelog.toml"]
fn sale_management_fields_move_to_sale() {
    let (_, clone) = clones().into_iter().find(|(name, _)| name == "odoo").unwrap();
    let parent = format!("{COMMIT}^");
    let listing = git(
        &clone,
        &["diff-tree", "-r", "--no-renames", "--name-status", &parent, COMMIT],
    );
    let changed: Vec<(String, String)> = String::from_utf8(listing)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(status, path)| (status.to_string(), path.to_string()))
        .filter(|(_, path)| Python.claims(path))
        .collect();
    let table = |rev: &str, path: &str| {
        Python
            .extract(&git(&clone, &["show", &format!("{rev}:{path}")]))
            .unwrap()
    };
    let tables: Vec<(Option<SymbolTable>, Option<SymbolTable>)> = changed
        .iter()
        .map(|(status, path)| {
            let old = (status != "A").then(|| table(&parent, path));
            let new = (status != "D").then(|| table(COMMIT, path));
            (old, new)
        })
        .collect();
    let files: Vec<(Side, Side)> = changed
        .iter()
        .zip(&tables)
        .map(|((_, path), (old, new))| {
            (
                old.as_ref().map(|t| (path.as_str(), t)),
                new.as_ref().map(|t| (path.as_str(), t)),
            )
        })
        .collect();
    let deltas = delta(&Python, &files);

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
