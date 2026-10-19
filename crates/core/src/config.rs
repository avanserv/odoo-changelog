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
}

#[derive(Debug, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    pub path: PathBuf,
    pub release_file: Option<String>,
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
        Ok(cfg)
    }

    pub fn fork_override(&self, repo: &str, version: &str) -> Option<&str> {
        self.fork_overrides.get(repo)?.get(version).map(String::as_str)
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
}
