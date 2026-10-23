//! `upgrade` on a small hand-written upgrade repository: which scripts it reads, the version
//! each folder maps to, and that a rerun is a no-op until master moves.

mod common;

use std::path::Path;

use common::{config, dump, git, init, odoo_changelog};
use rusqlite::Connection;

const RENAME: &str = "from odoo.upgrade import util\n\n\ndef migrate(cr, version):\n    util.rename_field(cr, \"shop.order\", \"cust_id\", \"partner_id\")\n";

fn upgrade(config: &Path) -> String {
    let out = odoo_changelog(config, &["upgrade"]);
    assert!(
        out.status.success(),
        "upgrade failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `(path, version, phase, call, model, old, new, partial)` of every event, by path.
fn events(db: &Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT path, version, phase, call, model, old, new, partial FROM upgrade_events
              ORDER BY path, line, col, seq",
        )
        .unwrap();
    stmt.query_map([], |r| {
        let s = |i: usize| r.get::<_, Option<String>>(i).map(|v| v.unwrap_or_else(|| "-".into()));
        Ok(format!(
            "{} {} {} {} {} {} {} {}",
            s(0)?,
            s(1)?,
            s(2)?,
            s(3)?,
            s(4)?,
            s(5)?,
            s(6)?,
            r.get::<_, bool>(7)?
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn reads_version_folders_once_per_commit() {
    let (dir, mut fx) = init("upgrade");
    fx.commit_files(
        &[
            ("migrations/shop/saas~17.2.1.1/pre-migrate.py", RENAME),
            (
                "migrations/shop/0.0.0/end-cleanup.py",
                "from odoo.upgrade import util\n\n\ndef migrate(cr, version):\n    util.remove_view(cr, \"shop.old_view\")\n",
            ),
            (
                "migrations/shop/16.0.1.0/post-models.py",
                "from odoo.upgrade import util\n\n\ndef migrate(cr, version):\n    for m in (\"shop.a\", \"shop.b\"):\n        util.remove_model(cr, m)\n",
            ),
            ("migrations/shop/tests/test_rename.py", RENAME),
            ("migrations/shop/17.0.1.0/notes.txt", "util.remove_module(cr, 'shop')\n"),
        ],
        "scripts",
    );
    // A symlinked script and a symlinked folder run a script its own folder already records.
    let again = fx.upstream.join("migrations/shop/saas~17.4.1.0");
    std::fs::create_dir_all(&again).unwrap();
    std::os::unix::fs::symlink("../saas~17.2.1.1/pre-migrate.py", again.join("pre-again.py")).unwrap();
    let plus = fx.upstream.join("migrations/shop_plus");
    std::fs::create_dir_all(&plus).unwrap();
    std::os::unix::fs::symlink("../shop/saas~17.2.1.1", plus.join("saas~17.2.1.1")).unwrap();
    fx.git(&["add", "migrations"]);
    fx.git(&["commit", "-q", "-m", "symlink"]);
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", "upstream", "clone"], 0);

    let extra = format!("\n[upgrade]\npath = {clone:?}\n");
    let cfg = config(&dir, "upgrade", &clone, &extra);
    let db = dir.join("upgrade.db");
    let printed = upgrade(&cfg);
    assert!(printed.contains("rename_field"), "{printed}");
    assert_eq!(
        events(&db),
        [
            "migrations/shop/0.0.0/end-cleanup.py - end remove_view - shop.old_view - false",
            "migrations/shop/16.0.1.0/post-models.py 16.0 post remove_model - shop.a - false",
            "migrations/shop/16.0.1.0/post-models.py 16.0 post remove_model - shop.b - false",
            "migrations/shop/saas~17.2.1.1/pre-migrate.py saas-17.2 pre rename_field shop.order cust_id partner_id false",
        ]
    );

    let before = dump(&db);
    assert_eq!(upgrade(&cfg), "", "a rerun on the same commit reads nothing");
    assert_eq!(dump(&db), before);

    fx.commit_files(
        &[(
            "migrations/shop/saas~17.2.1.1/pre-migrate.py",
            &RENAME.replace("cust_id", "customer_id"),
        )],
        "rename",
    );
    git(&clone, &["fetch", "-q", "origin"], 0);
    upgrade(&cfg);
    let after = events(&db);
    assert_eq!(after.len(), 4);
    assert!(after[3].contains(" customer_id partner_id "), "{after:?}");
}

#[test]
fn needs_an_upgrade_clone() {
    let (dir, _) = init("upgrade-unset");
    let cfg = config(&dir, "unset", &dir.join("upstream"), "");
    let out = odoo_changelog(&cfg, &["upgrade"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no [upgrade] path"));
}

#[test]
fn fails_without_migrations() {
    let (dir, mut fx) = init("upgrade-empty");
    fx.commit("README.md", "not an upgrade repository");
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", "upstream", "clone"], 0);
    let cfg = config(&dir, "empty", &clone, &format!("\n[upgrade]\npath = {clone:?}\n"));
    let out = odoo_changelog(&cfg, &["upgrade"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no migrations/"));
}
