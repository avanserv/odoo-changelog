//! Where each version branch left master.
//!
//! A version branch's first-parent chain runs back into master's first-parent chain at the
//! fork. `git merge-base` is not usable: until 13.0 stable branches were merged forward into
//! master, so the merge-base is the last merged stable commit. On a shallow clone the chain can
//! stop before the fork; the community repo then falls back to master's `release.py`, which
//! bumps `version_info` to the next series right after a fork.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use gix::ObjectId;
use odoo_changelog_core::{Config, RepoConfig, Version};
use regex::Regex;
use tracing::{info, warn};

use crate::Repo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkSource {
    FirstParent,
    ReleaseFile,
    Override,
    /// The branch shares no history with master (design-themes before 14.0).
    Unrelated,
}

impl ForkSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ForkSource::FirstParent => "first-parent",
            ForkSource::ReleaseFile => "release-file",
            ForkSource::Override => "override",
            ForkSource::Unrelated => "unrelated",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Fork {
    pub version: Version,
    pub head: ObjectId,
    /// Last master first-parent commit the branch shares with master; `None` when unrelated.
    pub fork: Option<ObjectId>,
    /// Position of `fork` in master's first-parent chain (0 = oldest walked).
    pub fp_index: Option<usize>,
    pub source: ForkSource,
}

static VERSION_INFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?m)^version_info\s*=\s*\(\s*(?:'saas~)?(\d+)'?\s*,\s*(\d+)"#).unwrap());

pub fn parse_version_info(text: &str) -> Option<(u32, u32)> {
    let c = VERSION_INFO.captures(text)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?))
}

/// Resolves the fork of every version branch from `cfg.first_version`, plus master's
/// first-parent chain starting at the oldest fork found.
pub fn resolve_forks(repo: &Repo, rc: &RepoConfig, cfg: &Config) -> Result<(Vec<Fork>, Vec<ObjectId>)> {
    let first = Version::parse_branch(&cfg.first_version).ok_or_else(|| anyhow!("bad first_version"))?;
    let master = repo.master_head()?;
    let branches = repo.version_branches(first)?;
    let full_chain = repo.first_parent_chain(master, None)?;
    let full_index: HashMap<ObjectId, usize> = full_chain.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut found = Vec::new();
    for (version, head) in &branches {
        let f = match cfg.fork_override(&repo.name, &version.branch()) {
            Some(sha) => Some((ObjectId::from_hex(sha.as_bytes())?, ForkSource::Override)),
            None => repo
                .first_parent_meeting(*head, &full_index)?
                .map(|id| (id, ForkSource::FirstParent)),
        };
        found.push(f);
    }
    let Some(base_pos) = found.iter().position(Option::is_some) else {
        return Err(anyhow!("{}: no version branch shares history with master", repo.name));
    };
    let base = found[base_pos].unwrap().0;
    let offset = *full_index
        .get(&base)
        .ok_or_else(|| anyhow!("{}: fork {base} is not on master", repo.name))?;
    let chain = full_chain[offset..].to_vec();
    let index: HashMap<ObjectId, usize> = chain.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    let mut release_cache = None;
    let mut forks = Vec::new();
    for (pos, ((version, head), f)) in branches.into_iter().zip(found).enumerate() {
        let f = match (f, &rc.release_file) {
            (Some(f), _) => Some(f),
            (None, Some(file)) if pos > base_pos => {
                let releases = release_cache.get_or_insert_with(|| release_series(repo, &chain, file));
                let releases = releases.as_ref().map_err(|e| anyhow!("{e}"))?;
                fork_from_releases(&chain, releases, version).map(|f| (f, ForkSource::ReleaseFile))
            }
            _ => None,
        };
        let fork = match f {
            Some((fork, source)) => {
                let fp = *index.get(&fork).ok_or_else(|| {
                    anyhow!(
                        "{}: fork of {version} ({fork}) is not on master's first-parent chain",
                        repo.name
                    )
                })?;
                info!(
                    "{}: {version} forked at {} (fp {fp}, {})",
                    repo.name,
                    &fork.to_hex().to_string()[..10],
                    source.as_str()
                );
                Fork {
                    version,
                    head,
                    fork: Some(fork),
                    fp_index: Some(fp),
                    source,
                }
            }
            None if pos < base_pos => {
                info!(
                    "{}: {version} shares no history with master; walked standalone",
                    repo.name
                );
                Fork {
                    version,
                    head,
                    fork: None,
                    fp_index: None,
                    source: ForkSource::Unrelated,
                }
            }
            None => {
                warn!(
                    "{}: cannot resolve fork of {version}; add a fork_overrides entry",
                    repo.name
                );
                continue;
            }
        };
        forks.push(fork);
    }
    Ok((forks, chain))
}

/// `(fp_index, series)` at every point where master's release file changed series.
fn release_series(repo: &Repo, chain: &[ObjectId], file: &str) -> Result<Vec<(usize, (u32, u32))>> {
    let mut out: Vec<(usize, (u32, u32))> = Vec::new();
    let mut last_blob = None;
    for (i, sha) in chain.iter().enumerate() {
        let Some(blob) = repo.blob_id_at(*sha, file)? else {
            continue;
        };
        if last_blob == Some(blob) {
            continue;
        }
        last_blob = Some(blob);
        let text = String::from_utf8_lossy(&repo.read_blob(blob)?).into_owned();
        if let Some(series) = parse_version_info(&text)
            && out.last().map(|(_, s)| *s) != Some(series)
        {
            out.push((i, series));
        }
    }
    Ok(out)
}

/// Parent of the first master commit whose series is the one following `version`'s fork.
/// A series announced and then reverted (16.1 → 15.5 in 2022) is skipped by taking the last
/// time master entered the target series after being below it.
fn fork_from_releases(chain: &[ObjectId], releases: &[(usize, (u32, u32))], version: Version) -> Option<ObjectId> {
    let target = version.next_master_release();
    let mut entered = None;
    for w in releases.windows(2) {
        let (prev, (i, series)) = (w[0].1, w[1]);
        if series == target && prev < target {
            entered = Some(i);
        }
    }
    entered.filter(|i| *i > 0).map(|i| chain[i - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_info_formats() {
        assert_eq!(
            parse_version_info("version_info = (17, 5, 0, ALPHA, 1, '')"),
            Some((17, 5))
        );
        assert_eq!(
            parse_version_info("version_info = ('saas~12', 3, 0, 'final', 0, '')"),
            Some((12, 3))
        );
        assert_eq!(parse_version_info("# version_info format: (MAJOR, MINOR"), None);
    }
}
