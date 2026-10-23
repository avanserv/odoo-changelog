use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow};
use odoo_changelog_core::Config;
use odoo_changelog_git::Repo;
use odoo_changelog_store::{Store, UpgradeEventRow};
use odoo_changelog_upgrade::{Event, Script, VERSION, script};
use rayon::prelude::*;
use tracing::info;

/// Events and partial events per upgrade-util function.
pub type Counts = BTreeMap<&'static str, (usize, usize)>;

/// Reads every script under `migrations/` at the upgrade clone's `<remote>/master` and replaces
/// the stored upgrade events with theirs; `None` when the stored events already came from the
/// same commit and parser. Symlinked scripts and folders are left out: each one runs a script
/// whose own folder already records it.
pub fn run(cfg: &Config, store: &Store) -> Result<Option<Counts>> {
    let uc = cfg
        .upgrade
        .as_ref()
        .ok_or_else(|| anyhow!("no [upgrade] path in the config: point it at a clone of odoo/upgrade"))?;
    let repo = Repo::open("upgrade", &uc.path, &cfg.remote)?;
    let head = repo.master_head()?;
    let sha = head.to_string();
    if store.upgrade_run()? == Some((sha.clone(), VERSION.into())) {
        info!("upgrade: {sha} already read");
        return Ok(None);
    }
    let scripts: Vec<(String, _)> = repo
        .blobs_under(head, "migrations")?
        .ok_or_else(|| anyhow!("upgrade: no migrations/ at {sha}"))?
        .into_iter()
        .filter(|(path, _)| script(path).is_some())
        .collect();
    let shared = repo.shared();
    let parsed: Vec<(String, Vec<Event>)> = scripts
        .into_par_iter()
        .map_init(
            || shared.worker(),
            |worker, (path, oid)| {
                let events = worker
                    .read_blob(oid)
                    .and_then(|blob| odoo_changelog_upgrade::parse(&blob))
                    .with_context(|| path.clone())?;
                Ok((path, events))
            },
        )
        .collect::<Result<_>>()?;

    let located: Vec<(&str, Script, &[Event])> = parsed
        .iter()
        .filter_map(|(path, events)| Some((path.as_str(), script(path)?, events.as_slice())))
        .collect();
    let mut counts = Counts::new();
    let mut rows = Vec::new();
    for (path, s, events) in &located {
        for e in *events {
            let count = counts.entry(e.call).or_default();
            count.0 += 1;
            count.1 += usize::from(e.partial);
            rows.push(UpgradeEventRow {
                path,
                line: e.line as i64,
                col: e.col as i64,
                seq: e.seq as i64,
                module: s.module,
                folder: s.folder,
                version: s.version.as_deref(),
                phase: s.phase,
                call: e.call,
                kind: e.kind,
                op: e.op,
                model: e.model.as_deref(),
                old: e.old.as_deref(),
                new: e.new.as_deref(),
                old_module: e.old_module.as_deref(),
                new_module: e.new_module.as_deref(),
                partial: e.partial,
                source: e.source.as_deref(),
            });
        }
    }
    store.in_transaction(|| store.replace_upgrade(&sha, VERSION.into(), &rows))?;
    info!("upgrade: {} events from {} scripts at {sha}", rows.len(), parsed.len());
    Ok(Some(counts))
}
