//! Each `fixtures/*.py` against the events beside it in `fixtures/*.json`.

use std::path::Path;

#[test]
fn fixtures() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut scripts: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "py"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty());
    for script in scripts {
        let events = odoo_changelog_upgrade::parse(&std::fs::read(&script).unwrap()).unwrap();
        let actual = serde_json::to_string_pretty(&events).unwrap() + "\n";
        check(&script.with_extension("json"), &actual);
    }
}

/// With `ODOO_CHANGELOG_BLESS=1`, writes `actual` to `expected` instead of comparing them.
fn check(expected: &Path, actual: &str) {
    if std::env::var_os("ODOO_CHANGELOG_BLESS").is_some_and(|v| v == "1") {
        std::fs::write(expected, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(expected)
        .unwrap_or_else(|e| panic!("{}: {e}; ODOO_CHANGELOG_BLESS=1 writes it", expected.display()));
    assert!(
        want == actual,
        "{} differs; ODOO_CHANGELOG_BLESS=1 rewrites it\n--- actual\n{actual}",
        expected.display()
    );
}

/// The evaluation budget holds per loop run, so a long loop resolves to its last run.
#[test]
fn long_loops_resolve_every_run() {
    let names: Vec<String> = (0..9000).map(|i| format!("'f{i}'")).collect();
    let script = format!(
        "from odoo.upgrade import util\nP = 'shop'\nQ = P + '.order'\n\n\ndef migrate(cr, version):\n    for m in [{}]:\n        util.rename_field(cr, f'{{Q}}', f'{{m}}_a', f'{{m}}_b')\n",
        names.join(", ")
    );
    let events = odoo_changelog_upgrade::parse(script.as_bytes()).unwrap();
    assert_eq!(events.len(), 9000);
    assert!(events.iter().all(|e| !e.partial));
    assert_eq!(events[8999].new.as_deref(), Some("f8999_b"));
}
