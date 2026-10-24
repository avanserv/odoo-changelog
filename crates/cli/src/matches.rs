use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use odoo_changelog_core::Version;
use odoo_changelog_store::{MatchRow, Store, StoredRename, StoredSighting, StoredUpgradeEvent};
use odoo_changelog_upgrade::matching::{self, DetectedRename, Matches, Mentioned, Sighting, Tally, UpgradeRef};
use tracing::info;

/// Rematches every stored upgrade event against every detected rename, so that the result
/// depends on the stored rows alone and not on whether `extract` or `upgrade` ran last.
pub fn run(store: &Store) -> Result<()> {
    let upgrade = store.upgrade_events()?;
    let renames = store.detected_renames()?;
    let m = find(&upgrade, &renames);
    let rows: Vec<MatchRow> = m
        .pairs
        .iter()
        .map(|&(u, r)| MatchRow {
            upgrade: &upgrade[u],
            rename: &renames[r],
        })
        .collect();
    store.in_transaction(|| store.replace_matches(&rows))?;
    if !upgrade.is_empty() {
        let events: BTreeSet<usize> = m.pairs.iter().map(|&(u, _)| u).collect();
        info!(
            "matches: {} upgrade events confirm {} renames",
            events.len(),
            rows.len()
        );
    }
    Ok(())
}

/// Recall and precision per walked version, oldest first, and kind, then an `all` row per kind.
pub fn validate(store: &Store) -> Result<Vec<((String, &'static str), Tally)>> {
    let upgrade = store.upgrade_events()?;
    if upgrade.is_empty() {
        bail!("no upgrade events stored: run upgrade first");
    }
    let renames = store.detected_renames()?;
    let m = find(&upgrade, &renames);
    let (u, r) = (refs(&upgrade), detected(&renames));
    let reachable = reachable(store, &u, &m)?;
    let walked = store.walked_versions()?;
    // An upgrade folder for a version no repo walked has no commit to match.
    let mut rows: Vec<_> = matching::tally(&u, &r, &m, &reachable)
        .into_iter()
        .filter(|((version, _), _)| walked.contains(version))
        .collect();
    // Master sorts after every version; the sort is stable, so kinds keep their order.
    rows.sort_by_key(|((version, _), _)| Version::parse_branch(version).map_or((1, None), |v| (0, Some(v))));
    let mut totals: BTreeMap<&'static str, Tally> = BTreeMap::new();
    for ((_, kind), t) in &rows {
        let total = totals.entry(kind).or_default();
        total.upgrade += t.upgrade;
        total.matched += t.matched;
        total.reachable += t.reachable;
        total.detected += t.detected;
        total.confirmed += t.confirmed;
    }
    rows.extend(totals.into_iter().map(|(kind, t)| (("all".to_string(), kind), t)));
    Ok(rows)
}

/// The stored upgrade events the detector could have matched, in key order, each with whether it
/// did.
pub fn reachable_events(store: &Store) -> Result<Vec<(StoredUpgradeEvent, bool)>> {
    let upgrade = store.upgrade_events()?;
    let renames = store.detected_renames()?;
    let m = find(&upgrade, &renames);
    let reachable = reachable(store, &refs(&upgrade), &m)?;
    let matched: BTreeSet<usize> = m.pairs.iter().map(|&(u, _)| u).collect();
    Ok(upgrade
        .into_iter()
        .enumerate()
        .filter(|(i, _)| reachable.contains(i))
        .map(|(i, e)| (e, matched.contains(&i)))
        .collect())
}

fn reachable(store: &Store, upgrade: &[UpgradeRef], m: &Matches) -> Result<BTreeSet<usize>> {
    let mentioned = Mentioned::new(upgrade);
    let kinds: Vec<&str> = Mentioned::kinds().collect();
    let sightings = store.sightings(&kinds, |kind, qualname| mentioned.contains(kind, qualname))?;
    Ok(matching::reachable(upgrade, m, &seen(&sightings)))
}

fn find(upgrade: &[StoredUpgradeEvent], renames: &[StoredRename]) -> Matches {
    matching::match_renames(&refs(upgrade), &detected(renames))
}

fn refs(upgrade: &[StoredUpgradeEvent]) -> Vec<UpgradeRef<'_>> {
    upgrade
        .iter()
        .map(|e| UpgradeRef {
            version: e.version.as_deref(),
            kind: &e.kind,
            op: &e.op,
            model: e.model.as_deref(),
            old: e.old.as_deref(),
            new: e.new.as_deref(),
            partial: e.partial,
        })
        .collect()
}

fn detected(renames: &[StoredRename]) -> Vec<DetectedRename<'_>> {
    renames
        .iter()
        .map(|r| DetectedRename {
            version: &r.version,
            old_kind: &r.old_kind,
            old_qualname: &r.old_qualname,
            kind: &r.kind,
            qualname: &r.qualname,
        })
        .collect()
}

fn seen(sightings: &[StoredSighting]) -> Vec<Sighting<'_>> {
    sightings
        .iter()
        .map(|s| Sighting {
            version: &s.version,
            repo_id: s.repo_id,
            sha: &s.sha,
            extractor: &s.extractor,
            old: s.old,
            kind: &s.kind,
            qualname: &s.qualname,
        })
        .collect()
}
