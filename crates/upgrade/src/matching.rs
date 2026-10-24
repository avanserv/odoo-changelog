//! Joins upgrade events to the renames `extract` detected, by kind, names and version, and counts
//! how many of each side the other confirms. Works on plain rows: qualnames follow the formats
//! `docs/extractors/` defines.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The kinds of the `data` extractor that an xmlid names; `view_node` is part of a view.
const XMLID_KINDS: &[&str] = &["view", "menu", "action", "group", "rule", "access", "record"];

/// What matching reads of an `upgrade_events` row.
pub struct UpgradeRef<'a> {
    pub version: Option<&'a str>,
    pub kind: &'a str,
    pub op: &'a str,
    pub model: Option<&'a str>,
    pub old: Option<&'a str>,
    pub new: Option<&'a str>,
    pub partial: bool,
}

/// A `renamed` event, with the version its commit ships in.
pub struct DetectedRename<'a> {
    pub version: &'a str,
    pub old_kind: &'a str,
    pub old_qualname: &'a str,
    pub kind: &'a str,
    pub qualname: &'a str,
}

/// A symbol one commit removed, added or renamed, as one side of a rename that may have happened
/// there.
pub struct Sighting<'a> {
    pub version: &'a str,
    pub repo_id: i64,
    pub sha: &'a str,
    pub extractor: &'a str,
    /// A removed symbol or a rename's old side, rather than an added one or a rename's new side.
    pub old: bool,
    pub kind: &'a str,
    pub qualname: &'a str,
}

/// The `(kind, name)` pairs that renamed upgrade events give as an old or a new name, the field,
/// model and xmlid kinds grouped as `tally` groups them.
pub struct Mentioned<'a>(HashSet<(&'static str, &'a str)>);

impl<'a> Mentioned<'a> {
    /// The symbol kinds an upgrade event can name.
    pub fn kinds() -> impl Iterator<Item = &'static str> {
        ["field", "model"].into_iter().chain(XMLID_KINDS.iter().copied())
    }

    pub fn new(upgrade: &[UpgradeRef<'a>]) -> Self {
        let keys = upgrade.iter().filter(|e| e.op == "renamed").filter_map(upgrade_key);
        Mentioned(keys.flat_map(|k| [(k.kind, k.old), (k.kind, k.new)]).collect())
    }

    /// Whether a symbol of `kind` named `qualname` bears a name some event gives.
    pub fn contains(&self, kind: &str, qualname: &str) -> bool {
        name(kind, qualname).is_some_and(|(kind, _, name)| self.0.contains(&(kind, name)))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Matches {
    /// `(upgrade, rename)` indexes into the inputs, sorted.
    pub pairs: Vec<(usize, usize)>,
    /// Renames of a field under its own name that follow a model rename some upgrade event
    /// matched: the scripts confirm them without naming them. Sorted.
    pub followers: Vec<usize>,
}

/// One row of the validation report. `upgrade`, `matched` and `reachable` count renamed upgrade
/// events, not merged ones; `detected` and `confirmed` count renames.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub upgrade: usize,
    pub matched: usize,
    /// Those `reachable` returns.
    pub reachable: usize,
    pub detected: usize,
    pub confirmed: usize,
}

/// The names a rename is looked up by.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key<'a> {
    version: &'a str,
    kind: &'static str,
    /// A field's model, either side's; `None` for any other kind.
    model: Option<&'a str>,
    old: &'a str,
    new: &'a str,
}

/// Pairs each upgrade event that renames or merges a field, model or xmlid with every detected
/// rename of the same kind and names whose commit ships in the event's version.
pub fn match_renames(upgrade: &[UpgradeRef], renames: &[DetectedRename]) -> Matches {
    let mut index: HashMap<Key, Vec<usize>> = HashMap::new();
    for (i, r) in renames.iter().enumerate() {
        for key in keys(r) {
            index.entry(key).or_default().push(i);
        }
    }
    let mut pairs = BTreeSet::new();
    // (version, old model, new model) of the model renames an upgrade event matched.
    let mut models = BTreeSet::new();
    for (u, e) in upgrade.iter().enumerate() {
        let Some(key) = upgrade_key(e) else { continue };
        for &r in index.get(&key).into_iter().flatten() {
            pairs.insert((u, r));
            if key.kind == "model" {
                models.insert((key.version, key.old, key.new));
            }
        }
    }
    let matched: BTreeSet<usize> = pairs.iter().map(|&(_, r)| r).collect();
    let followers = renames
        .iter()
        .enumerate()
        .filter(|(i, r)| !matched.contains(i) && r.kind == "field" && r.old_kind == "field")
        .filter_map(|(i, r)| {
            let (old_model, old_field) = member(r.old_qualname)?;
            let (model, field) = member(r.qualname)?;
            (old_field == field && models.contains(&(r.version, old_model, model))).then_some(i)
        })
        .collect();
    Matches {
        pairs: pairs.into_iter().collect(),
        followers,
    }
}

/// The renamed upgrade events the detector could have matched: those matched, and those whose old
/// name one commit and extractor of their version removed or renamed away while adding or renaming
/// to the new one.
pub fn reachable(upgrade: &[UpgradeRef], matches: &Matches, sightings: &[Sighting]) -> BTreeSet<usize> {
    let seen = Seen::new(sightings);
    let matched = matches.pairs.iter().map(|&(u, _)| u);
    let witnessed = (upgrade.iter().enumerate())
        .filter(|(_, e)| upgrade_key(e).is_some_and(|k| seen.witnessed(&k)))
        .map(|(u, _)| u);
    let renamed = |&u: &usize| upgrade[u].op == "renamed";
    matched.chain(witnessed).filter(renamed).collect()
}

/// Per `(version, kind)`, for the field, model and xmlid kinds.
pub fn tally(
    upgrade: &[UpgradeRef],
    renames: &[DetectedRename],
    matches: &Matches,
    reachable: &BTreeSet<usize>,
) -> BTreeMap<(String, &'static str), Tally> {
    let matched: BTreeSet<usize> = matches.pairs.iter().map(|&(u, _)| u).collect();
    let confirmed: BTreeSet<usize> = matches
        .pairs
        .iter()
        .map(|&(_, r)| r)
        .chain(matches.followers.iter().copied())
        .collect();
    let mut out: BTreeMap<(String, &'static str), Tally> = BTreeMap::new();
    for (i, e) in upgrade.iter().enumerate() {
        if e.op != "renamed" || e.partial {
            continue;
        }
        let (Some(version), Some(kind)) = (e.version, group(e.kind)) else {
            continue;
        };
        let t = out.entry((version.to_string(), kind)).or_default();
        t.upgrade += 1;
        t.matched += usize::from(matched.contains(&i));
        t.reachable += usize::from(reachable.contains(&i));
    }
    for (i, r) in renames.iter().enumerate() {
        let Some(kind) = rename_group(r) else { continue };
        let t = out.entry((r.version.to_string(), kind)).or_default();
        t.detected += 1;
        t.confirmed += usize::from(confirmed.contains(&i));
    }
    out
}

/// A commit, as `(repo_id, sha, extractor)`.
type Commit<'a> = (i64, &'a str, &'a str);

/// The commits that hold each side of a name: by `(version, kind, model, name, old)`, the model
/// being a field's, and `None` for any model.
struct Seen<'a>(HashMap<SeenKey<'a>, BTreeSet<Commit<'a>>>);

/// `(version, kind, model, name, old)`.
type SeenKey<'a> = (&'a str, &'static str, Option<&'a str>, &'a str, bool);

impl<'a> Seen<'a> {
    fn new(sightings: &[Sighting<'a>]) -> Self {
        let mut out: HashMap<_, BTreeSet<_>> = HashMap::new();
        for s in sightings {
            let Some((kind, model, name)) = name(s.kind, s.qualname) else {
                continue;
            };
            let commit = (s.repo_id, s.sha, s.extractor);
            out.entry((s.version, kind, None, name, s.old))
                .or_default()
                .insert(commit);
            if model.is_some() {
                out.entry((s.version, kind, model, name, s.old))
                    .or_default()
                    .insert(commit);
            }
        }
        Seen(out)
    }

    /// Whether one commit holds both sides of the event's rename, a field's on the event's model
    /// on at least one side, since the model may have been renamed with it.
    fn witnessed(&self, k: &Key) -> bool {
        let none = BTreeSet::new();
        let get = |model, name, old| self.0.get(&(k.version, k.kind, model, name, old)).unwrap_or(&none);
        let (old, new) = (get(None, k.old, true), get(None, k.new, false));
        match k.model {
            None => !old.is_disjoint(new),
            Some(model) => {
                !get(Some(model), k.old, true).is_disjoint(new) || !get(Some(model), k.new, false).is_disjoint(old)
            }
        }
    }
}

/// The kind group, field model and name an upgrade event would give a symbol by.
fn name<'q>(kind: &str, qualname: &'q str) -> Option<(&'static str, Option<&'q str>, &'q str)> {
    match kind {
        "field" => member(qualname).map(|(model, name)| ("field", Some(model), name)),
        "model" => Some(("model", None, model(qualname))),
        _ if XMLID_KINDS.contains(&kind) => Some(("xmlid", None, model(qualname))),
        _ => None,
    }
}

fn group(kind: &str) -> Option<&'static str> {
    ["field", "model", "xmlid"].into_iter().find(|k| *k == kind)
}

fn rename_group(r: &DetectedRename) -> Option<&'static str> {
    match (r.old_kind, r.kind) {
        ("field", "field") => Some("field"),
        ("model", "model") => Some("model"),
        (a, b) if XMLID_KINDS.contains(&a) && XMLID_KINDS.contains(&b) => Some("xmlid"),
        _ => None,
    }
}

fn upgrade_key<'a>(e: &UpgradeRef<'a>) -> Option<Key<'a>> {
    if e.partial || !matches!(e.op, "renamed" | "merged") {
        return None;
    }
    let kind = group(e.kind)?;
    let model = if kind == "field" { Some(e.model?) } else { None };
    Some(Key {
        version: e.version?,
        kind,
        model,
        old: e.old?,
        new: e.new?,
    })
}

/// The keys an upgrade event may look a rename up by: a field rename is found by either side's
/// model, since a script may name the model before or after renaming it.
fn keys<'a>(r: &DetectedRename<'a>) -> Vec<Key<'a>> {
    match rename_group(r) {
        Some("field") => {
            let (Some((old_model, old)), Some((model, new))) = (member(r.old_qualname), member(r.qualname)) else {
                return Vec::new();
            };
            let key = |model| Key {
                version: r.version,
                kind: "field",
                model: Some(model),
                old,
                new,
            };
            let mut keys = vec![key(old_model)];
            if model != old_model {
                keys.push(key(model));
            }
            keys
        }
        Some(kind) => vec![Key {
            version: r.version,
            kind,
            model: None,
            old: model(r.old_qualname),
            new: model(r.qualname),
        }],
        None => Vec::new(),
    }
}

/// `sale.order@sale` and `res.users[Users]@base` name `sale.order` and `res.users`; an xmlid
/// qualname loses its module suffix the same way.
fn model(qualname: &str) -> &str {
    let name = unscoped(qualname);
    match name.strip_suffix(']').and_then(|n| n.rsplit_once('[')) {
        Some((model, _)) => model,
        None => name,
    }
}

/// `sale.order#partner_id@sale` is `(sale.order, partner_id)`.
fn member(qualname: &str) -> Option<(&str, &str)> {
    let (parent, name) = unscoped(qualname).split_once('#')?;
    Some((model(parent), name))
}

fn unscoped(qualname: &str) -> &str {
    qualname.rsplit_once('@').map_or(qualname, |(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up<'a>(version: &'a str, kind: &'a str, model: Option<&'a str>, old: &'a str, new: &'a str) -> UpgradeRef<'a> {
        UpgradeRef {
            version: Some(version),
            kind,
            op: "renamed",
            model,
            old: Some(old),
            new: Some(new),
            partial: false,
        }
    }

    fn det<'a>(version: &'a str, kind: &'a str, old: &'a str, new: &'a str) -> DetectedRename<'a> {
        DetectedRename {
            version,
            old_kind: kind,
            old_qualname: old,
            kind,
            qualname: new,
        }
    }

    #[test]
    fn a_model_rename_matches_it_in_every_module() {
        let upgrade = [up("saas-16.3", "model", None, "mail.channel", "discuss.channel")];
        let renames = [
            det("saas-16.3", "model", "mail.channel@mail", "discuss.channel@mail"),
            det(
                "saas-16.3",
                "model",
                "mail.channel[Channel]@im_livechat",
                "discuss.channel@im_livechat",
            ),
            det(
                "saas-16.3",
                "field",
                "mail.channel#name@mail",
                "discuss.channel#name@mail",
            ),
            det(
                "saas-16.3",
                "field",
                "mail.channel#uuid@mail",
                "discuss.channel#token@mail",
            ),
        ];
        let m = match_renames(&upgrade, &renames);
        assert_eq!(m.pairs, [(0, 0), (0, 1)]);
        assert_eq!(m.followers, [2]);
    }

    #[test]
    fn a_field_is_found_by_the_model_before_or_after_its_rename() {
        let upgrade = [
            up("17.0", "field", Some("shop.order"), "cust_id", "partner_id"),
            up("17.0", "field", Some("sale.order"), "ref", "reference"),
        ];
        let renames = [
            det("17.0", "field", "shop.order#cust_id@shop", "sale.order#partner_id@shop"),
            det("17.0", "field", "shop.order#ref@shop", "sale.order#reference@shop"),
        ];
        assert_eq!(match_renames(&upgrade, &renames).pairs, [(0, 0), (1, 1)]);
    }

    #[test]
    fn an_xmlid_matches_records_but_not_view_nodes() {
        let upgrade = [up("16.0", "xmlid", None, "sale.old_form", "sale.new_form")];
        let renames = [
            det("16.0", "view", "sale.old_form@sale", "sale.new_form@sale"),
            det("16.0", "view_node", "sale.old_form#x@sale", "sale.new_form#x@sale"),
            DetectedRename {
                old_kind: "rule",
                kind: "access",
                ..det("16.0", "", "sale.old_form@sale_x", "sale.new_form@sale_x")
            },
        ];
        assert_eq!(match_renames(&upgrade, &renames).pairs, [(0, 0), (0, 2)]);
    }

    #[test]
    fn another_version_partial_or_unversioned_events_never_match() {
        let renames = [det("17.0", "model", "a.b@m", "a.c@m")];
        let partial = UpgradeRef {
            partial: true,
            ..up("17.0", "model", None, "a.b", "a.c")
        };
        let unversioned = UpgradeRef {
            version: None,
            ..up("17.0", "model", None, "a.b", "a.c")
        };
        let other = up("saas-17.2", "model", None, "a.b", "a.c");
        let removed = UpgradeRef {
            op: "removed",
            ..up("17.0", "model", None, "a.b", "a.c")
        };
        let m = match_renames(&[partial, unversioned, other, removed], &renames);
        assert_eq!(m, Matches::default());
    }

    #[test]
    fn tally_counts_renamed_events_and_confirmed_renames() {
        let merged = UpgradeRef {
            op: "merged",
            ..up("13.0", "model", None, "x.a", "x.b")
        };
        // Names the rename of `p`, but a partial event never counts.
        let partial = UpgradeRef {
            partial: true,
            ..up("13.0", "field", Some("a.c"), "p", "q")
        };
        let upgrade = [
            up("13.0", "model", None, "a.b", "a.c"),
            up("13.0", "field", Some("a.c"), "f", "g"),
            merged,
            partial,
        ];
        let renames = [
            det("13.0", "model", "a.b@m", "a.c@m"),
            det("13.0", "field", "a.b#h@m", "a.c#h@m"),
            det("13.0", "field", "a.c#p@m", "a.c#q@m"),
            det("13.0", "model", "x.a@m", "x.b@m"),
            det("13.0", "method", "a.b#m1@m", "a.b#m2@m"),
        ];
        let m = match_renames(&upgrade, &renames);
        let t = tally(&upgrade, &renames, &m, &reachable(&upgrade, &m, &[]));
        let row = |kind| t[&("13.0".to_string(), kind)].clone();
        assert_eq!(
            row("model"),
            Tally {
                upgrade: 1,
                matched: 1,
                reachable: 1,
                detected: 2,
                confirmed: 2
            }
        );
        assert_eq!(
            row("field"),
            Tally {
                upgrade: 1,
                matched: 0,
                reachable: 0,
                detected: 2,
                confirmed: 1
            }
        );
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn a_rename_is_reachable_when_one_commit_removes_the_old_name_and_adds_the_new() {
        let upgrade = [
            up("17.0", "field", Some("sale.order"), "a", "b"),
            up("17.0", "field", Some("sale.order"), "c", "d"),
            up("17.0", "xmlid", None, "sale.x", "sale_stock.y"),
            up("17.0", "model", None, "p.q", "p.r"),
        ];
        let seen = |sha, old, kind, qualname| Sighting {
            version: "17.0",
            repo_id: 1,
            sha,
            extractor: if kind == "field" || kind == "model" {
                "python"
            } else {
                "data"
            },
            old,
            kind,
            qualname,
        };
        let sightings = [
            // The model was renamed along: one side on the event's model is enough.
            seen("s1", true, "field", "shop.order#a@sale"),
            seen("s1", false, "field", "sale.order#b@sale"),
            // Two commits: removed in one, added in the next.
            seen("s1", true, "field", "sale.order#c@sale"),
            seen("s2", false, "field", "sale.order#d@sale"),
            seen("s3", true, "record", "sale.x@sale"),
            seen("s3", false, "view", "sale_stock.y@sale_stock"),
            // One commit, but another extractor.
            seen("s4", true, "model", "p.q@p"),
            Sighting {
                extractor: "data",
                ..seen("s4", false, "model", "p.r@p")
            },
        ];
        assert_eq!(
            reachable(&upgrade, &Matches::default(), &sightings),
            BTreeSet::from([0, 2])
        );
        let mentioned = Mentioned::new(&upgrade);
        assert!(mentioned.contains("field", "res.partner#b@base"));
        assert!(mentioned.contains("action", "sale.x@sale"));
        assert!(!mentioned.contains("view_node", "sale.x#y@sale"));
        assert!(!mentioned.contains("field", "sale.order#e@sale"));
    }
}
