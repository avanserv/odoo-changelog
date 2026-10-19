use std::time::Instant;

use anyhow::Result;
use odoo_changelog_core::{Config, RepoConfig};
use odoo_changelog_git::{Repo, resolve_forks};
use odoo_changelog_store::{CommitRow, Store, VersionRow};
use tracing::{info, warn};

pub fn run(cfg: &Config, store: &Store, only: &[String]) -> Result<()> {
    for rc in cfg.repos.iter().filter(|r| only.is_empty() || only.contains(&r.name)) {
        let start = Instant::now();
        walk_repo(cfg, store, rc)?;
        info!("{}: walked in {:.1?}", rc.name, start.elapsed());
    }
    Ok(())
}

fn walk_repo(cfg: &Config, store: &Store, rc: &RepoConfig) -> Result<()> {
    let repo = Repo::open(&rc.name, &rc.path, &cfg.remote)?;
    let shallow = repo.is_shallow()?;
    if shallow {
        warn!(
            "{}: shallow clone; forks may need the release-file fallback or overrides",
            rc.name
        );
    }
    let repo_id = store.upsert_repo(&rc.name, &rc.path.to_string_lossy(), shallow)?;
    let (forks, master_chain) = resolve_forks(&repo, rc, cfg)?;

    let tx = store.conn.unchecked_transaction()?;
    let rows: Vec<_> = forks
        .iter()
        .map(|f| {
            (
                f.version.branch(),
                f.head.to_string(),
                f.fork.map(|s| s.to_string()).unwrap_or_default(),
                f,
            )
        })
        .collect();
    store.replace_versions(
        repo_id,
        &rows
            .iter()
            .enumerate()
            .map(|(i, (name, head, fork, f))| VersionRow {
                name,
                ord: i as i64,
                head_sha: head,
                fork_sha: fork,
                fork_fp_idx: f.fp_index.map_or(-1, |i| i as i64),
                fork_source: f.source.as_str(),
            })
            .collect::<Vec<_>>(),
    )?;

    // Older branches first, so a fix merged forward is attributed to the branch it landed on.
    for f in &forks {
        walk_branch(&repo, store, repo_id, &f.version.branch(), f.head, f.fork)?;
    }
    let base = master_chain[0];
    walk_branch(&repo, store, repo_id, "master", repo.master_head()?, Some(base))?;
    store.assign_versions(repo_id)?;
    tx.commit()?;
    Ok(())
}

/// Records first-parent steps of `branch` after `fork` (its whole history when unrelated),
/// resuming from the last run. On master, fp_idx 0 is the oldest fork, matching the fork table.
fn walk_branch(
    repo: &Repo,
    store: &Store,
    repo_id: i64,
    branch: &str,
    head: gix::ObjectId,
    fork: Option<gix::ObjectId>,
) -> Result<()> {
    let (stop, start_fp) = match store.run_state(repo_id, branch)? {
        Some((sha, fp)) => {
            let last = gix::ObjectId::from_hex(sha.as_bytes())?;
            if last == head {
                return Ok(());
            }
            if !repo.is_ancestor(last, head)? {
                anyhow::bail!(
                    "{}: {branch} was rewritten since {sha}; rebuild the database",
                    repo.name
                );
            }
            (Some(last), fp + 1)
        }
        None => {
            if let (Some(fork), "master") = (fork, branch) {
                insert(repo, store, repo_id, branch, fork, 0, 0)?;
            }
            (fork, 1)
        }
    };
    let steps = repo.walk_steps(head, stop)?;
    let (mut new, total) = (0usize, steps.len());
    for (i, step) in steps.iter().enumerate() {
        let fp = start_fp + i as i64;
        new += insert(repo, store, repo_id, branch, step.commit, fp, 0)? as usize;
        for (j, sha) in step.merged.iter().enumerate() {
            new += insert(repo, store, repo_id, branch, *sha, fp, j as i64 + 1)? as usize;
        }
    }
    if total > 0 {
        store.set_run_state(repo_id, branch, &head.to_string(), start_fp + total as i64 - 1)?;
    }
    info!("{}: {branch}: {total} steps, {new} new commits", repo.name);
    Ok(())
}

fn insert(
    repo: &Repo,
    store: &Store,
    repo_id: i64,
    branch: &str,
    sha: gix::ObjectId,
    fp: i64,
    sub: i64,
) -> Result<bool> {
    let c = repo.commit_info(sha)?;
    let parents: Vec<String> = c.parents.iter().map(ToString::to_string).collect();
    store.insert_commit(
        repo_id,
        &CommitRow {
            sha: &sha.to_string(),
            branch,
            fp_idx: fp,
            sub_idx: sub,
            parents: &parents,
            author_name: &c.author_name,
            author_email: &c.author_email,
            authored_at: c.authored_at,
            committed_at: c.committed_at,
            message: &c.message,
        },
    )
}
