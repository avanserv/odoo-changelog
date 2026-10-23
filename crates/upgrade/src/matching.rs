//! Joins upgrade events to the renames `extract` detected, by kind, names and version, and counts
//! how many of each side the other confirms. Works on plain rows: qualnames follow the formats
//! `docs/extractors/` defines.

use std::collections::{BTreeMap, BTreeSet, HashMap};

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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Matches {
    /// `(upgrade, rename)` indexes into the inputs, sorted.
    pub pairs: Vec<(usize, usize)>,
    /// Renames of a field under its own name that follow a model rename some upgrade event
    /// matched: the scripts confirm them without naming them. Sorted.
    pub followers: Vec<usize>,
}

/// One row of the validation report. `upgrade` and `matched` count renamed upgrade events, not
/// merged ones; `detected` and `confirmed` count renames.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub upgrade: usize,
    pub matched: usize,
    pub detected: usize,
    pub confirmed: usize,
}

/// The names a rename is looked up by.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key<'a> {
    version: &'a str,
    kind: &'a str,
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

/// Per `(version, kind)`, for the field, model and xmlid kinds.
pub fn tally(
    upgrade: &[UpgradeRef],
    renames: &[DetectedRename],
    matches: &Matches,
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
    }
    for (i, r) in renames.iter().enumerate() {
        let Some(kind) = rename_group(r) else { continue };
        let t = out.entry((r.version.to_string(), kind)).or_default();
        t.detected += 1;
        t.confirmed += usize::from(confirmed.contains(&i));
    }
    out
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
        let t = tally(&upgrade, &renames, &m);
        let row = |kind| t[&("13.0".to_string(), kind)].clone();
        assert_eq!(
            row("model"),
            Tally {
                upgrade: 1,
                matched: 1,
                detected: 2,
                confirmed: 2
            }
        );
        assert_eq!(
            row("field"),
            Tally {
                upgrade: 1,
                matched: 0,
                detected: 2,
                confirmed: 1
            }
        );
        assert_eq!(t.len(), 2);
    }
}
