use std::time::Instant;

use anyhow::Result;
use odoo_changelog_core::{Config, RepoConfig};
use odoo_changelog_git::{Fork, Repo, resolve_forks};
use odoo_changelog_store::{CommitRow, Store, VersionRow};
use tracing::{info, warn};

pub fn run(cfg: &Config, store: &Store, only: &[String]) -> Result<()> {
    let unknown: Vec<&str> = only
        .iter()
        .filter(|n| !cfg.repos.iter().any(|r| &r.name == *n))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = cfg.repos.iter().map(|r| r.name.as_str()).collect();
        anyhow::bail!(
            "no repo named {} in the config; configured: {}",
            unknown.join(", "),
            known.join(", ")
        );
    }
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
    store.in_transaction(|| {
        let repo_id = store.upsert_repo(&rc.name, shallow)?;
        let (forks, master_chain) = resolve_forks(&repo, rc, cfg)?;
        check_forks_unchanged(store, repo_id, &repo.name, &forks, &master_chain)?;

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
        store.assign_versions(repo_id)
    })
}

/// Stored master `fp_idx` values count from the oldest fork, and each branch's rows start at
/// its own fork, so a fork that moved since the last walk renumbers rows a resume never revisits.
fn check_forks_unchanged(
    store: &Store,
    repo_id: i64,
    name: &str,
    forks: &[Fork],
    master_chain: &[gix::ObjectId],
) -> Result<()> {
    if let Some((sha, fp)) = store.run_state(repo_id, "master")?
        && master_chain.get(fp as usize).map(ToString::to_string) != Some(sha)
    {
        anyhow::bail!(
            "{name}: master's oldest fork moved or master was rewritten since the last walk; rebuild the database"
        );
    }
    let stored = store.version_forks(repo_id)?;
    for f in forks {
        let version = f.version.branch();
        let new = f.fork.map(|s| s.to_string()).unwrap_or_default();
        if let Some(old) = stored.get(&version)
            && *old != new
        {
            anyhow::bail!(
                "{name}: fork of {version} moved from {} to {} since the last walk; rebuild the database",
                short(old),
                short(&new)
            );
        }
    }
    Ok(())
}

fn short(sha: &str) -> &str {
    if sha.is_empty() { "none" } else { &sha[..10] }
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
    let sha = sha.to_string();
    let parents: Vec<String> = c.parents.iter().map(ToString::to_string).collect();
    let new = store.insert_commit(
        repo_id,
        &CommitRow {
            sha: &sha,
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
    )?;
    // A full rebuild walks master after every branch, so this branch would claim the commit.
    if !new && branch != "master" && store.commit_branch(repo_id, &sha)?.as_deref() == Some("master") {
        anyhow::bail!(
            "{}: {branch} reaches {sha}, which an earlier walk credited to master; rebuild the database",
            repo.name
        );
    }
    Ok(new)
}
