use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gix::ObjectId;
use odoo_changelog_core::{Config, RepoConfig, Version};
use odoo_changelog_extract::{Delta, Extractor, Registry, SymbolTable, delta};
use odoo_changelog_git::{FileChange, FileSide, Repo, SharedRepo};
use odoo_changelog_store::{CachedBlob, EventRow, Store};
use rayon::prelude::*;
use tracing::{info, warn};

/// Commits per transaction: a crash loses at most one batch, and it bounds the tables in memory.
const BATCH: usize = 512;

pub struct Options {
    pub repos: Vec<String>,
    /// First and last version to extract, both inclusive. Master sorts after every version.
    pub from: Option<Version>,
    pub to: Option<Version>,
    /// One per core when unset.
    pub threads: Option<NonZeroUsize>,
}

/// Wall time per phase; the phases never overlap and together cover the run.
#[derive(Debug, Default, Clone, Copy)]
pub struct Timings {
    /// Syncing extractor versions, opening repos and loading the commits to extract.
    pub setup: Duration,
    pub diff: Duration,
    pub parse: Duration,
    pub cache: Duration,
    pub write: Duration,
}

#[derive(Default)]
struct Counts {
    commits: usize,
    parsed: usize,
    hits: usize,
    events: usize,
    shallow: usize,
    failures: usize,
}

struct Commit {
    sha: String,
    oid: ObjectId,
    /// Indexes into the extractors, in name order, of those that have not processed it.
    pending: Vec<usize>,
}

/// A cached parse failure is `Err` with the extractor's message.
type Parsed = std::result::Result<SymbolTable, String>;

struct Ctx<'a> {
    store: &'a Store,
    shared: &'a SharedRepo,
    pool: &'a rayon::ThreadPool,
    extractors: &'a [&'a dyn Extractor],
    repo_id: i64,
    name: &'a str,
}

/// Records the symbol events of every walked, non-merge commit that some extractor has not
/// processed yet. Each commit is diffed against its only parent and renames are paired per
/// extractor, so an extractor's events depend on the commit and that extractor alone, and an
/// incremental run equals a full rebuild.
pub fn run(cfg: &Config, store: &Store, registry: &Registry, opts: &Options) -> Result<Timings> {
    let repos = cfg.select_repos(&opts.repos)?;
    let mut timings = Timings::default();
    if registry.is_empty() {
        info!("no extractor is registered; nothing to extract");
        return Ok(timings);
    }
    let start = Instant::now();
    let extractors: Vec<&dyn Extractor> = registry.iter().collect();
    let current: Vec<(&str, i64)> = extractors.iter().map(|e| (e.name(), e.version().into())).collect();
    let purged = store.in_transaction(|| store.sync_extractors(&current))?;
    if !purged.is_empty() {
        info!("{}: version changed; dropped their events and cache", purged.join(", "));
    }
    // A local pool, so that one process can run at several thread counts.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.map_or(0, NonZeroUsize::get))
        .build()?;
    timings.setup += start.elapsed();
    for rc in repos {
        let start = Instant::now();
        extract_repo(cfg, store, &pool, &extractors, rc, opts, &mut timings)?;
        info!("{}: extracted in {:.1?}", rc.name, start.elapsed());
    }
    Ok(timings)
}

fn extract_repo(
    cfg: &Config,
    store: &Store,
    pool: &rayon::ThreadPool,
    extractors: &[&dyn Extractor],
    rc: &RepoConfig,
    opts: &Options,
    timings: &mut Timings,
) -> Result<()> {
    let start = Instant::now();
    let repo_id = store
        .repo_id(&rc.name)?
        .ok_or_else(|| anyhow!("{}: no commits recorded; run walk first", rc.name))?;
    let repo = Repo::open(&rc.name, &rc.path, &cfg.remote)?;
    let shared = repo.shared();
    let mut commits = Vec::new();
    for c in store.extract_candidates(repo_id)? {
        if !in_range(c.version.as_deref(), opts.from, opts.to) {
            continue;
        }
        let pending: Vec<usize> = (0..extractors.len())
            .filter(|&i| !c.done.iter().any(|d| d == extractors[i].name()))
            .collect();
        if !pending.is_empty() {
            commits.push(Commit {
                oid: ObjectId::from_hex(c.sha.as_bytes())?,
                sha: c.sha,
                pending,
            });
        }
    }

    let ctx = Ctx {
        store,
        shared: &shared,
        pool,
        extractors,
        repo_id,
        name: &rc.name,
    };
    timings.setup += start.elapsed();
    let mut counts = Counts::default();
    for batch in commits.chunks(BATCH) {
        store.in_transaction(|| extract_batch(&ctx, batch, &mut counts, timings))?;
    }
    info!(
        "{}: {} commits extracted, {} blobs parsed, {} cache hits, {} events",
        rc.name, counts.commits, counts.parsed, counts.hits, counts.events
    );
    if counts.shallow > 0 {
        warn!(
            "{}: {} commits left for later: their parent is beyond the shallow boundary",
            rc.name, counts.shallow
        );
    }
    if counts.failures > 0 {
        warn!(
            "{}: {} file sides failed to parse and gave no events",
            rc.name, counts.failures
        );
    }
    Ok(())
}

/// Bounds are inclusive; a version that does not parse (master) is past every bound.
fn in_range(version: Option<&str>, from: Option<Version>, to: Option<Version>) -> bool {
    match version.and_then(Version::parse_branch) {
        Some(v) => from.is_none_or(|f| v >= f) && to.is_none_or(|t| v <= t),
        None => to.is_none(),
    }
}

fn extract_batch(ctx: &Ctx, batch: &[Commit], counts: &mut Counts, timings: &mut Timings) -> Result<()> {
    // The store is not `Sync`, so the parallel phases capture only these.
    let (ex, shared, name) = (ctx.extractors, ctx.shared, ctx.name);

    let start = Instant::now();
    // Per commit, the changes each pending extractor sees, in `pending` order.
    let diffs: Vec<Result<Option<Vec<Vec<FileChange>>>>> = ctx.pool.install(|| {
        batch
            .par_iter()
            .map_init(
                || shared.worker(),
                |w, c| {
                    let claimed = |p: &str| c.pending.iter().any(|&i| ex[i].claims(p));
                    let Some(changes) = w.changed_files(c.oid, &claimed)? else {
                        return Ok(None);
                    };
                    c.pending
                        .iter()
                        .map(|&i| w.pair_renames(&changes, &|p| ex[i].claims(p)))
                        .collect::<Result<Vec<_>>>()
                        .map(Some)
                },
            )
            .collect()
    });
    // The first error in commit order, not whichever thread failed first.
    let diffs = diffs
        .into_iter()
        .zip(batch)
        .map(|(d, c)| d.with_context(|| format!("{name}: diffing {}", c.sha)))
        .collect::<Result<Vec<_>>>()?;
    timings.diff += start.elapsed();

    let start = Instant::now();
    let mut keys = BTreeSet::new();
    for (c, per_extractor) in batch.iter().zip(&diffs) {
        for (&i, changes) in c.pending.iter().zip(per_extractor.iter().flatten()) {
            for change in changes {
                keys.extend([&change.old, &change.new].into_iter().flatten().map(|s| (s.blob, i)));
            }
        }
    }
    // Lookups only: never iterated, so its order cannot reach the database.
    let mut tables: HashMap<(ObjectId, usize), Parsed> = HashMap::with_capacity(keys.len());
    let mut misses = Vec::new();
    for key in keys {
        let (oid, i) = key;
        match ctx
            .store
            .cached_blob(&oid.to_string(), ex[i].name(), ex[i].version().into())?
        {
            Some(hit) => {
                tables.insert(key, decode(hit).with_context(|| format!("decoding cached {oid}"))?);
                counts.hits += 1;
            }
            None => misses.push(key),
        }
    }
    timings.cache += start.elapsed();

    let start = Instant::now();
    let parsed: Vec<Result<Parsed>> = ctx.pool.install(|| {
        misses
            .par_iter()
            .map_init(
                || shared.worker(),
                |w, &(oid, i)| {
                    let blob = w.read_blob(oid).with_context(|| format!("{name}: reading {oid}"))?;
                    Ok(ex[i].extract(&blob).map_err(|e| format!("{e:#}")))
                },
            )
            .collect()
    });
    let parsed = parsed.into_iter().collect::<Result<Vec<_>>>()?;
    counts.parsed += parsed.len();
    timings.parse += start.elapsed();

    let start = Instant::now();
    for (&(oid, i), table) in misses.iter().zip(parsed) {
        let (symbols, error) = match &table {
            Ok(t) => (Some(t.to_bytes()), None),
            Err(e) => (None, Some(e.as_str())),
        };
        ctx.store.insert_cached_blob(
            &oid.to_string(),
            ex[i].name(),
            ex[i].version().into(),
            symbols.as_deref(),
            error,
        )?;
        tables.insert((oid, i), table);
    }
    timings.cache += start.elapsed();

    let start = Instant::now();
    let mut events: Vec<(&str, &str, Option<&str>, &str, Delta)> = Vec::new();
    let mut done = Vec::new();
    for (c, per_extractor) in batch.iter().zip(&diffs) {
        let Some(per_extractor) = per_extractor else {
            counts.shallow += 1;
            continue;
        };
        for (&i, changes) in c.pending.iter().zip(per_extractor) {
            for change in changes {
                let (Some(old), Some(new)) = (
                    parsed_side(&tables, i, &change.old),
                    parsed_side(&tables, i, &change.new),
                ) else {
                    counts.failures += 1;
                    continue;
                };
                let old_path = change
                    .old
                    .as_ref()
                    .filter(|o| change.new.as_ref().is_some_and(|n| n.path != o.path))
                    .map(|o| o.path.as_str());
                for d in delta(ex[i], old, new) {
                    events.push((&c.sha, change.path(), old_path, ex[i].name(), d));
                }
            }
        }
        done.push(c);
    }
    let rows: Vec<EventRow> = events
        .iter()
        .map(|(sha, path, old_path, extractor, d)| EventRow {
            sha,
            path,
            extractor,
            symbol_id: d.id.as_bytes(),
            kind: &d.kind,
            qualname: &d.qualname,
            op: d.op.as_str(),
            old_path: *old_path,
            line: d.line.into(),
            attrs_before: d.attrs_before.as_deref(),
            attrs_after: d.attrs_after.as_deref(),
        })
        .collect();
    ctx.store.insert_events(ctx.repo_id, &rows)?;
    for c in &done {
        let names: Vec<&str> = c.pending.iter().map(|&i| ex[i].name()).collect();
        ctx.store.mark_extracted(ctx.repo_id, &c.sha, &names)?;
    }
    counts.events += rows.len();
    counts.commits += done.len();
    timings.write += start.elapsed();
    Ok(())
}

/// `None` when the side failed to parse; `Some(None)` when the change has no such side.
fn parsed_side<'a>(
    tables: &'a HashMap<(ObjectId, usize), Parsed>,
    i: usize,
    side: &'a Option<FileSide>,
) -> Option<Option<(&'a str, &'a SymbolTable)>> {
    match side {
        None => Some(None),
        Some(s) => tables[&(s.blob, i)].as_ref().ok().map(|t| Some((s.path.as_str(), t))),
    }
}

fn decode(hit: CachedBlob) -> Result<Parsed> {
    match (hit.symbols, hit.error) {
        (Some(bytes), _) => Ok(Ok(SymbolTable::from_bytes(&bytes)?)),
        (None, error) => Ok(Err(error.unwrap_or_default())),
    }
}
