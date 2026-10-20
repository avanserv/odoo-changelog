//! Two runs on the same refs must produce identical databases, and so must an incremental walk
//! and a full rebuild. The fixture is a small Odoo-shaped repository built with the git CLI:
//! version branches forked from master, forward-port merges and a branch forked between runs.

mod common;

use std::path::Path;
use std::process::Command;

use common::{config, dump, git, init, odoo_changelog, rev_parse, setup, walk, walk_output};
use rusqlite::Connection;

/// Walks `config`, expecting the walk to fail with `expected` and leave `db` as it was.
fn walk_fails(config: &Path, db: &Path, expected: &str) {
    let before = dump(db);
    let out = walk_output(config);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "walk succeeded, expected: {expected}");
    assert!(stderr.contains(expected), "unexpected error: {stderr}");
    assert_eq!(dump(db), before, "the failed walk wrote to the database");
}

/// `(name, fork_source, fork_sha)` of every version, in release order.
fn forks(db: &Path) -> Vec<(String, String, String)> {
    let conn = Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT name, fork_source, fork_sha FROM versions ORDER BY ord")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn incremental_walk_equals_full_rebuild() {
    let (dir, mut fx, clone) = setup("determinism");
    let incremental = config(&dir, "incremental", &clone, "");
    walk(&incremental);

    fx.git(&["checkout", "-q", "16.0"]);
    fx.commit("addons/sale/views.xml", "[FIX] sale: form view");
    fx.git(&["checkout", "-q", "17.0"]);
    fx.commit("addons/stock/views.xml", "[FIX] stock: picking view");
    fx.merge("16.0", "[MERGE] forward port 16.0");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("addons/account/models.py", "[IMP] account: moves");
    fx.git(&["checkout", "-q", "-b", "saas-17.1"]);
    fx.commit("addons/account/models.py", "[FIX] account: taxes");
    fx.git(&["checkout", "-q", "master"]);
    fx.merge("17.0", "[MERGE] forward port 17.0");
    git(&clone, &["fetch", "-q", "origin"], 0);
    walk(&incremental);

    let full = config(&dir, "full", &clone, "");
    walk(&full);
    let rebuilt = dump(&dir.join("full.db"));
    // Every commit but the root, which master reached before its oldest fork.
    assert_eq!(rebuilt.iter().filter(|r| r.starts_with("commits: ")).count(), 11);
    assert_eq!(dump(&dir.join("incremental.db")), rebuilt);

    walk(&full);
    assert_eq!(
        dump(&dir.join("full.db")),
        rebuilt,
        "a re-run on the same refs changed the database"
    );
}

#[test]
fn config_path_spelling_does_not_reach_the_db() {
    let (dir, _fx, _clone) = setup("path-spelling");
    // A relative clone path, which `Config::load` joins with the config's own directory.
    config(&dir, "plain", Path::new("clone"), "");
    config(&dir, "dotted", Path::new("clone"), "");
    walk(Path::new("path-spelling/plain.toml"));
    walk(Path::new("./path-spelling/dotted.toml"));
    assert_eq!(dump(&dir.join("plain.db")), dump(&dir.join("dotted.db")));
}

#[test]
fn moved_fork_asks_for_rebuild() {
    let (dir, _, clone) = setup("moved-fork");
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    // The root sits one step before 16.0's real fork, so pinning it moves master's base back.
    let root = Command::new("git")
        .args(["rev-list", "--max-parents=0", "origin/master"])
        .current_dir(&clone)
        .output()
        .unwrap();
    let root = String::from_utf8(root.stdout).unwrap().trim().to_string();
    // 17.0 pinned to 16.0's fork moves that branch's walk range but not master's base.
    let fork_16 = rev_parse(&clone, "origin/16.0~1");
    for (version, sha, expected) in [
        ("16.0", root.as_str(), "master's oldest fork moved"),
        ("17.0", fork_16.as_str(), "fork of 17.0 moved"),
    ] {
        let cfg = config(
            &dir,
            "walk",
            &clone,
            &format!("\n[fork_overrides.odoo]\n{version:?} = {sha:?}\n"),
        );
        walk_fails(&cfg, &dir.join("walk.db"), expected);
    }
}

#[test]
fn override_pins_the_fork() {
    let (dir, _, clone) = setup("override");
    let fork_16 = rev_parse(&clone, "origin/16.0~1");
    let extra = format!("\n[fork_overrides.odoo]\n\"17.0\" = {fork_16:?}\n");
    walk(&config(&dir, "walk", &clone, &extra));

    let forks = forks(&dir.join("walk.db"));
    assert_eq!(forks[0], ("16.0".into(), "first-parent".into(), fork_16.clone()));
    assert_eq!(forks[1], ("17.0".into(), "override".into(), fork_16));
}

#[test]
fn unknown_repo_is_rejected() {
    let (dir, _, clone) = setup("unknown-repo");
    let out = odoo_changelog(&config(&dir, "walk", &clone, ""), &["walk", "--repo", "foo"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "walk accepted an unknown repo");
    assert!(
        stderr.contains("no repo named foo") && stderr.contains("configured: odoo"),
        "unexpected error: {stderr}"
    );
}

#[test]
fn rewritten_branch_asks_for_rebuild() {
    let (dir, mut fx, clone) = setup("rewritten");
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    fx.git(&["checkout", "-q", "17.0"]);
    fx.clock += 60;
    fx.git(&["commit", "-q", "--amend", "-m", "[FIX] stock: quants, reworded"]);
    git(&clone, &["fetch", "-q", "origin"], 0);
    walk_fails(&cfg, &dir.join("walk.db"), "17.0 was rewritten");
}

/// A branch created from a commit master brought in through a merge reaches that commit,
/// which a full rebuild credits to the branch and an incremental walk already gave master.
#[test]
fn branch_from_merged_commit_asks_for_rebuild() {
    let (dir, mut fx, clone) = setup("merged-fork");
    fx.git(&["checkout", "-q", "-b", "side"]);
    fx.commit("addons/crm/models.py", "[ADD] crm: leads");
    fx.commit("addons/crm/models.py", "[IMP] crm: teams");
    fx.git(&["checkout", "-q", "master"]);
    fx.merge("side", "[MERGE] crm");
    git(&clone, &["fetch", "-q", "origin"], 0);
    let incremental = config(&dir, "incremental", &clone, "");
    walk(&incremental);

    fx.git(&["checkout", "-q", "-b", "saas-17.1", "side"]);
    fx.commit("addons/crm/views.xml", "[FIX] crm: form view");
    git(&clone, &["fetch", "-q", "origin"], 0);
    walk_fails(&incremental, &dir.join("incremental.db"), "credited to master");

    walk(&config(&dir, "full", &clone, ""));
    let conn = Connection::open(dir.join("full.db")).unwrap();
    for rev in ["origin/side~1", "origin/side"] {
        let branch: String = conn
            .query_row(
                "SELECT branch FROM commits WHERE sha = ?1",
                [rev_parse(&clone, rev)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(branch, "saas-17.1", "{rev}");
    }
}

#[test]
fn shallow_oldest_branch_uses_release_file() {
    let (dir, mut fx) = init("shallow");
    fx.commit("odoo/release.py", "version_info = (15, 5)");
    fx.commit("addons/sale/models.py", "[IMP] sale: order lines");
    let fork_16 = rev_parse(&fx.upstream, "HEAD");
    fx.git(&["checkout", "-q", "-b", "16.0"]);
    for i in 1..=5 {
        fx.commit("addons/sale/models.py", &format!("[FIX] sale: rounding {i}"));
    }
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("odoo/release.py", "version_info = (16, 1)");
    fx.commit("addons/stock/models.py", "[IMP] stock: moves");
    fx.git(&["checkout", "-q", "-b", "17.0"]);
    fx.commit("addons/stock/models.py", "[FIX] stock: quants");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("odoo/release.py", "version_info = (17, 1)");
    fx.commit("addons/account/models.py", "[IMP] account: moves");

    // Depth 5 keeps 16.0's five commits but cuts the oldest from its fork, which master reaches.
    let upstream = format!("file://{}", fx.upstream.display());
    let args = ["clone", "-q", "--depth", "5", "--no-single-branch", &upstream, "clone"];
    git(&dir, &args, 0);
    let cfg = config(&dir, "walk", &dir.join("clone"), "release_file = \"odoo/release.py\"\n");
    walk(&cfg);

    let forks = forks(&dir.join("walk.db"));
    assert_eq!(forks[0], ("16.0".into(), "release-file".into(), fork_16));
    assert_eq!(forks[1].1, "first-parent");
}

#[test]
fn full_clone_unrelated_branch() {
    let (dir, mut fx) = init("unrelated");
    fx.commit("odoo/release.py", "version_info = (15, 5)");
    fx.git(&["checkout", "-q", "--orphan", "16.0"]);
    fx.commit("theme/models.py", "[ADD] theme: first");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("odoo/release.py", "version_info = (16, 1)");
    fx.git(&["checkout", "-q", "-b", "17.0"]);
    fx.commit("addons/stock/models.py", "[FIX] stock: quants");
    fx.git(&["checkout", "-q", "master"]);
    fx.commit("odoo/release.py", "version_info = (17, 1)");

    git(&dir, &["clone", "-q", "upstream", "clone"], 0);
    let cfg = config(&dir, "walk", &dir.join("clone"), "release_file = \"odoo/release.py\"\n");
    walk(&cfg);

    let forks = forks(&dir.join("walk.db"));
    assert_eq!(forks[0], ("16.0".into(), "unrelated".into(), String::new()));
    assert_eq!(forks[1].1, "first-parent");
}

#[test]
fn merged_commits_follow_their_parents() {
    let (dir, mut fx, clone) = setup("merge-order");
    // A base reached from the merge by a short and a long path, which a breadth-first walk
    // visits before the long path's commits.
    fx.git(&["checkout", "-q", "-b", "side"]);
    fx.commit("addons/crm/models.py", "[ADD] crm: base");
    fx.git(&["checkout", "-q", "-b", "a"]);
    for i in 1..=3 {
        fx.commit("addons/crm/leads.py", &format!("[IMP] crm: A{i}"));
    }
    fx.git(&["checkout", "-q", "side"]);
    fx.commit("addons/crm/teams.py", "[IMP] crm: B1");
    fx.merge("a", "[MERGE] crm: a");
    fx.git(&["checkout", "-q", "master"]);
    fx.merge("side", "[MERGE] crm: side");
    git(&clone, &["fetch", "-q", "origin"], 0);
    let cfg = config(&dir, "walk", &clone, "");
    walk(&cfg);

    let conn = Connection::open(dir.join("walk.db")).unwrap();
    let (branch, fp_idx): (String, i64) = conn
        .query_row(
            "SELECT branch, fp_idx FROM commits WHERE sha = ?1",
            [rev_parse(&clone, "origin/master")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT sha, sub_idx, parents FROM commits WHERE branch = ?1 AND fp_idx = ?2 AND sub_idx >= 1")
        .unwrap();
    let merged: Vec<(String, i64, String)> = stmt
        .query_map(rusqlite::params![branch, fp_idx], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(merged.len(), 6);
    for (child, child_idx, parents) in &merged {
        for (parent, parent_idx, _) in merged.iter().filter(|(sha, ..)| parents.contains(sha.as_str())) {
            assert!(
                parent_idx < child_idx,
                "{child} at {child_idx} precedes its parent {parent} at {parent_idx}"
            );
        }
    }
}
