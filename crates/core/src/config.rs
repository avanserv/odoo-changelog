use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub db: PathBuf,
    #[serde(default = "default_remote")]
    pub remote: String,
    pub first_version: String,
    pub repos: Vec<RepoConfig>,
    /// repo name → version name → fork sha
    #[serde(default)]
    pub fork_overrides: BTreeMap<String, BTreeMap<String, String>>,
    pub upgrade: Option<UpgradeConfig>,
}

#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    pub path: PathBuf,
    pub release_file: Option<String>,
}

/// The clone of `odoo/upgrade` that `upgrade` reads.
#[derive(Debug, Deserialize)]
pub struct UpgradeConfig {
    pub path: PathBuf,
}

fn default_remote() -> String {
    "origin".into()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        cfg.db = resolve(base, &cfg.db);
        for repo in &mut cfg.repos {
            repo.path = resolve(base, &repo.path);
        }
        if let Some(upgrade) = &mut cfg.upgrade {
            upgrade.path = resolve(base, &upgrade.path);
        }
        Ok(cfg)
    }

    pub fn fork_override(&self, repo: &str, version: &str) -> Option<&str> {
        self.fork_overrides.get(repo)?.get(version).map(String::as_str)
    }

    /// The repos named in `only`, in config order, or every repo when `only` is empty.
    pub fn select_repos(&self, only: &[String]) -> Result<Vec<&RepoConfig>> {
        let unknown: Vec<&str> = only
            .iter()
            .filter(|n| !self.repos.iter().any(|r| &r.name == *n))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let known: Vec<&str> = self.repos.iter().map(|r| r.name.as_str()).collect();
            anyhow::bail!(
                "no repo named {} in the config; configured: {}",
                unknown.join(", "),
                known.join(", ")
            );
        }
        Ok(self
            .repos
            .iter()
            .filter(|r| only.is_empty() || only.contains(&r.name))
            .collect())
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
}
