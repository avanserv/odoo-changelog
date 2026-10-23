//! Golden cases of matching against the real database, so every test here is `#[ignore]`: run
//! `walk`, `extract` and `upgrade` on the configured clones first, then
//! `cargo test -p odoo-changelog --test golden -- --ignored`. `ODOO_CHANGELOG_CONFIG` overrides
//! the config path. Samples are drawn at run time, so no upgrade script content is kept here.

use std::path::PathBuf;

use odoo_changelog::matches;
use odoo_changelog_core::Config;
use odoo_changelog_store::Store;
use rusqlite::{Connection, OptionalExtension};

fn config() -> Config {
    let path = std::env::var_os("ODOO_CHANGELOG_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/odoo-changelog.toml")));
    Config::load(&path).unwrap()
}

fn db() -> Connection {
    Connection::open_with_flags(&config().db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// The confidence and signals of the rename of `old` into `new`, by qualname.
fn rename(conn: &Connection, old: &str, new: &str) -> Option<(f64, String)> {
    conn.query_row(
        "SELECT e.confidence, e.signals FROM events e
           JOIN symbols s ON s.id = e.symbol_id JOIN symbols o ON o.id = e.old_symbol_id
          WHERE e.op = 'renamed' AND o.qualname = ?1 AND s.qualname = ?2",
        [old, new],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .unwrap()
}

fn assert_model_rename_matched(old: &str, new: &str, module: &str, version: &str) {
    let conn = db();
    let sha: Option<String> = conn
        .query_row(
            "SELECT sha FROM upgrade_events
              WHERE call = 'rename_model' AND old = ?1 AND new = ?2 AND version = ?3",
            [old, new, version],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        sha.is_some(),
        "the upgrade event of {old} at {version} matched no commit"
    );
    let (confidence, signals) = rename(&conn, &format!("{old}@{module}"), &format!("{new}@{module}"))
        .unwrap_or_else(|| panic!("{old} is not detected as renamed into {new}"));
    assert_eq!(confidence, 1.0);
    assert!(signals.ends_with(",\"upgrade\"]"), "{signals}");
}

#[test]
#[ignore = "reads the configured database"]
fn mail_channel_became_discuss_channel() {
    assert_model_rename_matched("mail.channel", "discuss.channel", "mail", "saas-16.3");
}

#[test]
#[ignore = "reads the configured database"]
fn stock_production_lot_became_stock_lot() {
    assert_model_rename_matched("stock.production.lot", "stock.lot", "stock", "saas-15.2");
}

/// No resolved upgrade event merges `account.invoice` into `account.move`, and the detector sees
/// the model removed, not renamed: neither side records the merge, so nothing can match it.
#[test]
#[ignore = "reads the configured database"]
fn account_invoice_into_account_move_is_neither_resolved_nor_detected() {
    let conn = db();
    let declared: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM upgrade_events
              WHERE old = 'account.invoice' AND new = 'account.move' AND NOT partial",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(declared, 0);
    let removed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events e
               JOIN commits c ON c.repo_id = e.repo_id AND c.sha = e.sha
               JOIN symbols s ON s.id = e.symbol_id
              WHERE e.op = 'removed' AND s.qualname = 'account.invoice@account' AND c.version = 'saas-12.4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(removed > 0, "account.invoice is not detected as removed in saas-12.4");
}

/// The acceptance target: every version's recall on field, model and xmlid renames is at least
/// 90%.
#[test]
#[ignore = "reads the configured database"]
fn recall_is_at_least_90_percent_per_version() {
    let store = Store::open(&config().db).unwrap();
    let low: Vec<String> = matches::validate(&store)
        .unwrap()
        .into_iter()
        .filter(|((_, _), t)| t.upgrade > 0 && (t.matched as f64) < 0.9 * t.upgrade as f64)
        .map(|((version, kind), t)| format!("{version} {kind} {}/{}", t.matched, t.upgrade))
        .collect();
    assert!(low.is_empty(), "recall under 90%: {low:?}");
}

/// 50 renamed fields and xmlids, drawn with a fixed seed: at least 90% match, as the detector's
/// recall target asks. Misses are printed for the by-hand check.
#[test]
#[ignore = "reads the configured database"]
fn sampled_field_and_xmlid_renames_mostly_match() {
    let conn = db();
    let events: Vec<(String, String, Option<String>, String, String, bool)> = conn
        .prepare(
            "SELECT version, kind, model, old, new, sha IS NOT NULL FROM upgrade_events
              WHERE op = 'renamed' AND kind IN ('field', 'xmlid') AND NOT partial
                AND version IN (SELECT name FROM versions)
              ORDER BY path, line, col, seq",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(events.len() >= 50, "only {} renamed fields and xmlids", events.len());
    // A 64-bit LCG (Knuth's MMIX constants), so the sample is the same on every machine.
    let mut state: u64 = 1;
    let mut picked = std::collections::BTreeSet::new();
    while picked.len() < 50 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        picked.insert((state >> 33) as usize % events.len());
    }
    let misses: Vec<_> = picked.iter().map(|&i| &events[i]).filter(|e| !e.5).collect();
    for (version, kind, model, old, new, _) in &misses {
        println!(
            "miss: {version} {kind} {} {old} -> {new}",
            model.as_deref().unwrap_or("-")
        );
    }
    assert!(
        misses.len() <= 5,
        "{} of 50 sampled renames matched no commit",
        misses.len()
    );
}
