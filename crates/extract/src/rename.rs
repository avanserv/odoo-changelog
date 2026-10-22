use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::delta::unscoped;
use crate::{Extractor, SymbolId};

/// Lowest confidence, in thousandths, at which a pair is recorded as a rename.
const THRESHOLD: u32 = 600;
/// Pairs scoring below this, in thousandths, are not searched for replaced references.
const REFS_FLOOR: u32 = 400;
/// Matched attr weight a pair needs for its attr score to count in full, so that two symbols
/// sharing only a type or a few flags score low however few attrs they have.
const EVIDENCE: f64 = 4.0;
/// Attrs holding a hash of a definition's content, which say more than any other attr when equal.
const HASHES: [&str; 3] = ["arch", "body", "values"];
const HASH_WEIGHT: f64 = 4.0;
/// Booleans, numbers, empty values and one-character strings, which many unrelated symbols share.
const TRIVIAL_WEIGHT: f64 = 0.5;
/// How much more an attr both sides hold with different values counts against a pair than an
/// attr only one side holds.
const CONFLICT: f64 = 2.0;
const REF_BOOST: f64 = 0.1;
const MAX_REF_BOOST: f64 = 0.3;
/// Removed × added pairs past which a scope is not scored: a mass rewrite, such as a regenerated
/// chart of accounts, is not worth the quadratic cost.
const MAX_PAIRS: usize = 100_000;

/// The symbol a member belongs to, such as a field's model, and the member's name in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parent<'q> {
    pub kind: &'static str,
    pub qualname: String,
    pub local: &'q str,
}

impl<'q> Parent<'q> {
    /// Splits `<parent>#<local>@<module>` at the first `#`, the parent keeping any `@<module>`.
    pub(crate) fn member(kind: &'static str, qualname: &'q str) -> Option<Self> {
        let (head, rest) = qualname.split_once('#')?;
        let (local, qualname) = match rest.rsplit_once('@') {
            Some((local, module)) => (local, format!("{head}@{module}")),
            None => (rest, head.to_string()),
        };
        Some(Parent { kind, qualname, local })
    }
}

/// Why a removal and an addition of one commit were paired as one renamed symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Rename {
    /// From 0.6 to 1, in steps of 0.001.
    pub confidence: f64,
    /// The top-level attrs equal once the old name reads as the new one, in name order, then
    /// `members`, `refs` and `parent` when they counted.
    pub signals: Vec<String>,
}

/// A symbol a commit removed or added, which no move claimed.
pub(crate) struct Item<'a> {
    pub kind: &'a str,
    pub qualname: &'a str,
    pub path: &'a str,
    pub attrs: &'a BTreeMap<String, Value>,
}

struct Node<'a> {
    kind: &'a str,
    qualname: &'a str,
    id: SymbolId,
    module: Option<&'a str>,
    unscoped: &'a str,
    parent: Option<(&'static str, String)>,
    local: &'a str,
    depth: usize,
    /// `(top-level key, path below it, canonical JSON)` of each leaf of the attrs, in order.
    leaves: Vec<(&'a str, String, String)>,
}

/// The children of each parent, by `(kind, qualname)`, as `(kind, local)`.
type Children<'n> = BTreeMap<(&'n str, &'n str), BTreeSet<(&'n str, &'n str)>>;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Scope<'n> {
    Module(&'n str, Option<&'n str>),
    Parent(&'n str, &'static str, &'n str),
}

/// The `(removed, added)` index pairs that are renames, each with its evidence. Parents pair
/// before their members; members of a renamed parent pair by name first, then are scored with
/// the new parent's other members. `refs` are the `(attrs_before, attrs_after)` of the commit's
/// other changes, where a replaced reference to the old name adds to a pair's confidence.
pub(crate) fn pairs(
    ex: &dyn Extractor,
    removed: &[Item],
    added: &[Item],
    refs: &[(&str, &str)],
) -> Vec<(usize, usize, Rename)> {
    if removed.is_empty() || added.is_empty() {
        return Vec::new();
    }
    let old: Vec<Node> = removed.iter().map(|i| node(ex, i)).collect();
    let new: Vec<Node> = added.iter().map(|i| node(ex, i)).collect();
    let (old_children, new_children) = (children(&old), children(&new));
    let mut taken = Taken {
        old: vec![false; old.len()],
        new: vec![false; new.len()],
        renamed: BTreeMap::new(),
        out: Vec::new(),
    };

    let depth = old.iter().chain(&new).map(|n| n.depth).max().unwrap_or(0);
    for depth in 0..=depth {
        let by_name: BTreeMap<(&str, &str, &str, &str), usize> = (0..new.len())
            .filter(|&j| new[j].depth == depth)
            .filter_map(|j| {
                let (kind, qualname) = new[j].parent.as_ref()?;
                Some(((new[j].kind, *kind, qualname.as_str(), new[j].local), j))
            })
            .collect();
        for i in (0..old.len()).filter(|&i| old[i].depth == depth) {
            let Some((kind, qualname)) = &old[i].parent else {
                continue;
            };
            let Some(&(to, milli)) = taken.renamed.get(&(*kind, qualname.as_str())) else {
                continue;
            };
            if let Some(&j) = by_name.get(&(old[i].kind, *kind, to, old[i].local))
                && !taken.new[j]
            {
                taken.take(&old[i], &new[j], (i, j), milli, vec!["parent".into()]);
            }
        }

        let mut scopes: BTreeMap<Scope, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for i in (0..old.len()).filter(|&i| old[i].depth == depth && !taken.old[i]) {
            let scope = match &old[i].parent {
                None => Scope::Module(old[i].kind, old[i].module),
                Some((kind, qualname)) => {
                    let to = taken.renamed.get(&(*kind, qualname.as_str()));
                    Scope::Parent(old[i].kind, kind, to.map_or(qualname.as_str(), |r| r.0))
                }
            };
            scopes.entry(scope).or_default().0.push(i);
        }
        for j in (0..new.len()).filter(|&j| new[j].depth == depth && !taken.new[j]) {
            let scope = match &new[j].parent {
                None => Scope::Module(new[j].kind, new[j].module),
                Some((kind, qualname)) => Scope::Parent(new[j].kind, kind, qualname.as_str()),
            };
            scopes.entry(scope).or_default().1.push(j);
        }
        for (olds, news) in scopes.into_values() {
            if olds.is_empty() || news.is_empty() || olds.len() * news.len() > MAX_PAIRS {
                continue;
            }
            let mut candidates = Vec::new();
            for &i in &olds {
                // One name on both sides is a move the move pass found ambiguous, never a rename.
                for &j in news.iter().filter(|&&j| new[j].unscoped != old[i].unscoped) {
                    let (milli, signals) = score(&old[i], &new[j], &old_children, &new_children, refs);
                    if milli >= THRESHOLD {
                        candidates.push((milli, i, j, signals));
                    }
                }
            }
            candidates.sort_by(|a, b| {
                (b.0.cmp(&a.0))
                    .then(old[a.1].id.cmp(&old[b.1].id))
                    .then(new[a.2].id.cmp(&new[b.2].id))
            });
            for (milli, i, j, signals) in candidates {
                if !taken.old[i] && !taken.new[j] {
                    taken.take(&old[i], &new[j], (i, j), milli, signals);
                }
            }
        }
    }
    taken.out
}

struct Taken<'n> {
    old: Vec<bool>,
    new: Vec<bool>,
    /// Old `(kind, qualname)` to the new qualname and the confidence in thousandths.
    renamed: BTreeMap<(&'n str, &'n str), (&'n str, u32)>,
    out: Vec<(usize, usize, Rename)>,
}

impl<'n> Taken<'n> {
    fn take(&mut self, o: &Node<'n>, n: &Node<'n>, (i, j): (usize, usize), milli: u32, signals: Vec<String>) {
        self.old[i] = true;
        self.new[j] = true;
        self.renamed.insert((o.kind, o.qualname), (n.qualname, milli));
        let confidence = f64::from(milli) / 1000.0;
        self.out.push((i, j, Rename { confidence, signals }));
    }
}

fn node<'a>(ex: &dyn Extractor, item: &Item<'a>) -> Node<'a> {
    let parent = ex.parent(item.kind, item.qualname);
    let mut depth = 0;
    let mut up = parent.as_ref().map(|p| (p.kind, p.qualname.clone()));
    // Bounded, so that an extractor naming a symbol its own ancestor cannot hang the walk.
    while let Some((kind, qualname)) = up.filter(|_| depth < 16) {
        depth += 1;
        up = ex.parent(kind, &qualname).map(|p| (p.kind, p.qualname));
    }
    let mut leaves = Vec::new();
    for (key, value) in item.attrs {
        flatten(key, String::new(), value, &mut leaves);
    }
    // `attr_score` merge-joins on this order, which depth-first output breaks: `a` before `a.b`.
    leaves.sort_unstable();
    Node {
        kind: item.kind,
        qualname: item.qualname,
        id: SymbolId::new(item.kind, item.qualname),
        module: ex.module(item.path),
        unscoped: unscoped(ex, item.path, item.qualname),
        local: parent
            .as_ref()
            .map_or_else(|| unscoped(ex, item.path, item.qualname), |p| p.local),
        parent: parent.map(|p| (p.kind, p.qualname)),
        depth,
        leaves,
    }
}

fn flatten<'a>(top: &'a str, path: String, value: &Value, out: &mut Vec<(&'a str, String, String)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                flatten(top, format!("{path}/{key}"), value, out);
            }
        }
        _ => out.push((top, path, value.to_string())),
    }
}

fn children<'n>(nodes: &'n [Node]) -> Children<'n> {
    let mut out: Children = BTreeMap::new();
    for n in nodes {
        if let Some((kind, qualname)) = &n.parent {
            out.entry((kind, qualname)).or_default().insert((n.kind, n.local));
        }
    }
    out
}

/// The pair's confidence in thousandths, and its signals.
fn score(
    o: &Node,
    n: &Node,
    old_children: &Children,
    new_children: &Children,
    refs: &[(&str, &str)],
) -> (u32, Vec<String>) {
    let (mut score, mut signals) = attr_score(o, n);
    let kids = (
        old_children.get(&(o.kind, o.qualname)),
        new_children.get(&(n.kind, n.qualname)),
    );
    if kids.0.is_some() || kids.1.is_some() {
        let none = BTreeSet::new();
        let (a, b) = (kids.0.unwrap_or(&none), kids.1.unwrap_or(&none));
        let shared = a.intersection(b).count();
        score = 0.5 * score + 0.5 * shared as f64 / a.union(b).count() as f64;
        if shared > 0 {
            signals.push("members".into());
        }
    }
    if milli(score) >= REFS_FLOOR {
        let replaced = refs.iter().filter(|(b, a)| replaces(b, a, o.local, n.local)).count();
        if replaced > 0 {
            score += (replaced as f64 * REF_BOOST).min(MAX_REF_BOOST);
            signals.push("refs".into());
        }
    }
    (milli(score.min(1.0)), signals)
}

/// Matched weight over total weight, scaled down while the matched weight is below `EVIDENCE`,
/// and the top-level keys whose every leaf matched.
fn attr_score(o: &Node, n: &Node) -> (f64, Vec<String>) {
    let (mut matched, mut total) = (0.0, 0.0);
    let (mut keys, mut unequal) = (BTreeSet::new(), BTreeSet::new());
    let (mut a, mut b) = (o.leaves.iter().peekable(), n.leaves.iter().peekable());
    loop {
        // The leaf's top-level key, its weight and whether it matched.
        let (top, w, equal) = match (a.peek().copied(), b.peek().copied()) {
            (None, None) => break,
            (Some(x), Some(y)) if (x.0, &x.1) == (y.0, &y.1) => {
                a.next();
                b.next();
                if x.2 == y.2 || replace(&x.2, o.local, n.local) == y.2 {
                    (x.0, weight(x.0, &x.2), true)
                } else {
                    (x.0, CONFLICT * weight(x.0, &x.2), false)
                }
            }
            (Some(x), Some(y)) if (x.0, &x.1) < (y.0, &y.1) => {
                a.next();
                (x.0, weight(x.0, &x.2), false)
            }
            (_, Some(y)) => {
                b.next();
                (y.0, weight(y.0, &y.2), false)
            }
            (Some(x), None) => {
                a.next();
                (x.0, weight(x.0, &x.2), false)
            }
        };
        total += w;
        keys.insert(top);
        if equal {
            matched += w;
        } else {
            unequal.insert(top);
        }
    }
    let score = if matched == 0.0 {
        0.0
    } else {
        matched / total * (matched / EVIDENCE).min(1.0)
    };
    (score, keys.difference(&unequal).map(|k| k.to_string()).collect())
}

fn weight(top: &str, json: &str) -> f64 {
    let trivial = matches!(json, "true" | "false" | "null" | "[]" | "{}")
        || json.starts_with(|c: char| c.is_ascii_digit() || c == '-')
        || (json.starts_with('"') && json.chars().count() <= 3);
    if HASHES.contains(&top) {
        HASH_WEIGHT
    } else if trivial {
        TRIVIAL_WEIGHT
    } else {
        1.0
    }
}

fn milli(score: f64) -> u32 {
    (score * 1000.0).round() as u32
}

/// Whether a change replaced a reference to `old` with one to `new`: the old side names `old`
/// and not `new`, the new side the reverse.
fn replaces(before: &str, after: &str, old: &str, new: &str) -> bool {
    let names = |text: &str, name: &str| occurrences(text, name).next().is_some();
    names(before, old) && !names(before, new) && names(after, new) && !names(after, old)
}

/// `text` with each whole-name occurrence of `old` read as `new`.
fn replace(text: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for at in occurrences(text, old) {
        out.push_str(&text[last..at]);
        out.push_str(new);
        last = at + old.len();
    }
    out.push_str(&text[last..]);
    out
}

/// Where `name` occurs in `text` as a whole name: between characters that cannot continue an
/// identifier and, for a dotted name such as a model's, not followed by a dot, so that
/// `mail.channel` is not found in `mail.channel.member`.
fn occurrences<'t>(text: &'t str, name: &'t str) -> impl Iterator<Item = usize> + 't {
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let bytes = text.as_bytes();
    text.match_indices(name).map(|(at, _)| at).filter(move |&at| {
        let before = at.checked_sub(1).map(|k| bytes[k]);
        let after = bytes.get(at + name.len()).copied();
        !name.is_empty()
            && !before.is_some_and(word)
            && !after.is_some_and(word)
            && !(name.contains('.') && after == Some(b'.'))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Data, Delta, Op, Python, Side, Span, Symbol, SymbolTable, delta};

    fn table(symbols: &[(&str, &str, Value)]) -> SymbolTable {
        let mut t = SymbolTable::default();
        for (kind, name, attrs) in symbols {
            let Value::Object(attrs) = attrs else {
                panic!("attrs are an object");
            };
            t.insert(Symbol {
                kind: kind.to_string(),
                name: name.to_string(),
                line: 1,
                span: Span::default(),
                attrs: attrs.clone().into_iter().collect(),
            });
        }
        t
    }

    /// `(old qualname, new qualname, confidence, signals)` of each rename.
    fn renames(deltas: &[Delta]) -> Vec<(&str, &str, f64, Vec<&str>)> {
        deltas
            .iter()
            .filter(|d| d.op == Op::Renamed)
            .map(|d| {
                let r = d.rename.as_ref().unwrap();
                let signals = r.signals.iter().map(String::as_str).collect();
                (
                    d.old.as_ref().unwrap().qualname.as_str(),
                    d.qualname.as_str(),
                    r.confidence,
                    signals,
                )
            })
            .collect()
    }

    fn ops(deltas: &[Delta]) -> Vec<(&str, &str)> {
        deltas.iter().map(|d| (d.qualname.as_str(), d.op.as_str())).collect()
    }

    const SALE: &str = "addons/sale/models/sale_order.py";

    fn one(ex: &dyn Extractor, path: &str, old: &SymbolTable, new: &SymbolTable) -> Vec<Delta> {
        let files: [(Side, Side); 1] = [(Some((path, old)), Some((path, new)))];
        delta(ex, &files)
    }

    #[test]
    fn a_field_renamed_with_its_attrs_is_one_rename() {
        let attrs = json!({"type": "Many2one", "comodel": "res.partner", "args": {"compute": "_compute_partner"}});
        let old = table(&[("field", "sale.order#partner_id", attrs.clone())]);
        let new = table(&[("field", "sale.order#customer_id", attrs)]);
        let d = one(&Python, SALE, &old, &new);
        assert_eq!(
            renames(&d),
            [(
                "sale.order#partner_id@sale",
                "sale.order#customer_id@sale",
                0.75,
                vec!["args", "comodel", "type"]
            )]
        );
        assert_eq!((d.len(), d[0].attrs_before.as_deref()), (1, None));
    }

    #[test]
    fn an_unrelated_field_of_another_type_is_no_rename() {
        let old = table(&[(
            "field",
            "sale.order#note",
            json!({"type": "Char", "args": {"string": "Note"}}),
        )]);
        let new = table(&[(
            "field",
            "sale.order#count",
            json!({"type": "Integer", "args": {"string": "Count"}}),
        )]);
        let d = one(&Python, SALE, &old, &new);
        assert_eq!(
            ops(&d),
            [("sale.order#count@sale", "added"), ("sale.order#note@sale", "removed")]
        );
    }

    #[test]
    fn a_shared_type_alone_is_no_rename() {
        let old = table(&[("field", "sale.order#note", json!({"type": "Char"}))]);
        let new = table(&[("field", "sale.order#memo", json!({"type": "Char"}))]);
        assert!(renames(&one(&Python, SALE, &old, &new)).is_empty());
    }

    #[test]
    fn a_method_renamed_with_its_body_is_one_rename() {
        let old = table(&[(
            "method",
            "sale.order#_compute_a",
            json!({"body": "h1", "args": ["self"]}),
        )]);
        let new = table(&[(
            "method",
            "sale.order#_compute_b",
            json!({"body": "h1", "args": ["self"]}),
        )]);
        let d = one(&Python, SALE, &old, &new);
        assert_eq!(renames(&d)[0].2, 1.0);
        assert_eq!(renames(&d)[0].3, ["args", "body"]);
    }

    #[test]
    fn members_of_other_models_or_modules_never_pair() {
        let attrs = json!({"type": "Many2one", "comodel": "res.partner", "args": {"compute": "_c"}});
        let old = table(&[("field", "sale.order#partner_id", attrs.clone())]);
        let new = table(&[("field", "sale.order.line#customer_id", attrs.clone())]);
        assert!(renames(&one(&Python, SALE, &old, &new)).is_empty());

        let other = table(&[("field", "sale.order#customer_id", attrs)]);
        let files: [(Side, Side); 2] = [
            (Some((SALE, &old)), None),
            (None, Some(("addons/sale_stock/models/sale_order.py", &other))),
        ];
        assert!(renames(&delta(&Python, &files)).is_empty());
    }

    /// The models of one `_name` rename commit, before or after, with a few members each.
    fn channels(prefix: &str) -> SymbolTable {
        let model = |name: &str, description: &str| json!({"_name": name, "_description": description, "_inherit": ["mail.thread"], "bases": ["models.Model"]});
        let (channel, member, session) = (
            format!("{prefix}.channel"),
            format!("{prefix}.channel.member"),
            format!("{prefix}.channel.rtc.session"),
        );
        table(&[
            ("model", &channel, model(&channel, "Discussion Channel")),
            (
                "field",
                &format!("{channel}#name"),
                json!({"type": "Char", "args": {"required": true}}),
            ),
            (
                "field",
                &format!("{channel}#channel_type"),
                json!({"type": "Selection"}),
            ),
            (
                "field",
                &format!("{channel}#channel_member_ids"),
                json!({"type": "One2many", "comodel": member}),
            ),
            (
                "method",
                &format!("{channel}#_compute_is_member"),
                json!({"body": "h1"}),
            ),
            ("model", &member, model(&member, "Channel Member")),
            (
                "field",
                &format!("{member}#channel_id"),
                json!({"type": "Many2one", "comodel": channel}),
            ),
            (
                "field",
                &format!("{member}#partner_id"),
                json!({"type": "Many2one", "comodel": "res.partner"}),
            ),
            ("field", &format!("{member}#fold_state"), json!({"type": "Selection"})),
            ("model", &session, model(&session, "Mail RTC session")),
            (
                "field",
                &format!("{session}#channel_member_id"),
                json!({"type": "Many2one", "comodel": member}),
            ),
            ("field", &format!("{session}#is_muted"), json!({"type": "Boolean"})),
        ])
    }

    #[test]
    fn models_renamed_together_are_told_apart_by_their_members() {
        let partner = |comodel: &str| {
            table(&[(
                "field",
                "res.partner#channel_ids",
                json!({"type": "Many2many", "comodel": comodel}),
            )])
        };
        let (old, new) = (channels("mail"), channels("discuss"));
        let (old_partner, new_partner) = (partner("mail.channel"), partner("discuss.channel"));
        let files: [(Side, Side); 3] = [
            (Some(("addons/mail/models/mail_channel.py", &old)), None),
            (None, Some(("addons/mail/models/discuss/discuss_channel.py", &new))),
            (
                Some(("addons/mail/models/res_partner.py", &old_partner)),
                Some(("addons/mail/models/res_partner.py", &new_partner)),
            ),
        ];
        let d = delta(&Python, &files);
        let r = renames(&d);
        let models: Vec<_> = r.iter().filter(|r| !r.0.contains('#')).collect();
        assert_eq!(models.len(), 3, "{r:?}");
        for (old, new, confidence, signals) in models {
            assert_eq!(old.replacen("mail.", "discuss.", 1), *new);
            assert!(*confidence >= 0.9, "{old}: {confidence}");
            assert!(signals.contains(&"members"), "{old}: {signals:?}");
        }
        assert!(r.iter().any(|r| r.0 == "mail.channel@mail" && r.3.contains(&"refs")));
        let members: Vec<_> = r.iter().filter(|r| r.0.contains('#')).collect();
        assert_eq!(members.len(), 9);
        assert!(
            members
                .iter()
                .all(|r| r.3 == ["parent"] && r.0.replacen("mail.", "discuss.", 1) == r.1)
        );
        assert_eq!(
            ops(&d).iter().filter(|(_, op)| *op != "renamed").collect::<Vec<_>>(),
            [&("res.partner#channel_ids@mail", "changed")]
        );
    }

    #[test]
    fn stock_production_lot_renamed_to_stock_lot_with_its_extensions() {
        let base = |name: &str| {
            table(&[
                (
                    "model",
                    name,
                    json!({"_name": name, "_description": "Lot/Serial", "_order": "name, id"}),
                ),
                (
                    "field",
                    &format!("{name}#product_id"),
                    json!({"type": "Many2one", "comodel": "product.product"}),
                ),
                ("field", &format!("{name}#ref"), json!({"type": "Char"})),
            ])
        };
        let extension = |name: &str| {
            table(&[
                ("model", name, json!({"_inherit": [name], "bases": ["models.Model"]})),
                (
                    "field",
                    &format!("{name}#use_expiration_date"),
                    json!({"type": "Boolean"}),
                ),
            ])
        };
        let (old, new) = (base("stock.production.lot"), base("stock.lot"));
        let (old_ext, new_ext) = (extension("stock.production.lot"), extension("stock.lot"));
        let line = |comodel: &str| {
            table(&[(
                "field",
                "stock.move.line#lot_id",
                json!({"type": "Many2one", "comodel": comodel}),
            )])
        };
        let (old_line, new_line) = (line("stock.production.lot"), line("stock.lot"));
        let files: [(Side, Side); 5] = [
            (Some(("addons/stock/models/stock_production_lot.py", &old)), None),
            (None, Some(("addons/stock/models/stock_lot.py", &new))),
            (Some(("addons/product_expiry/models/production_lot.py", &old_ext)), None),
            (None, Some(("addons/product_expiry/models/stock_lot.py", &new_ext))),
            (
                Some(("addons/stock/models/stock_move_line.py", &old_line)),
                Some(("addons/stock/models/stock_move_line.py", &new_line)),
            ),
        ];
        let d = delta(&Python, &files);
        let r = renames(&d);
        assert_eq!(r.len(), d.len() - 1, "{:?}", ops(&d));
        let model = |old: &str| r.iter().find(|r| r.0 == old).unwrap();
        assert_eq!(model("stock.production.lot@stock").1, "stock.lot@stock");
        assert!(model("stock.production.lot@stock").2 >= 0.9);
        assert_eq!(
            model("stock.production.lot@product_expiry").1,
            "stock.lot@product_expiry"
        );
        assert_eq!(
            model("stock.production.lot#use_expiration_date@product_expiry").1,
            "stock.lot#use_expiration_date@product_expiry"
        );
    }

    #[test]
    fn a_member_renamed_with_its_model_is_scored_under_the_new_model() {
        let side = |model: &str, field: &str| {
            table(&[
                (
                    "model",
                    model,
                    json!({"_name": model, "_description": "Lot", "_order": "name"}),
                ),
                ("field", &format!("{model}#name"), json!({"type": "Char"})),
                (
                    "field",
                    &format!("{model}#{field}"),
                    json!({"type": "Many2one", "comodel": "res.partner", "args": {"compute": "_compute_owner", "store": true}}),
                ),
                ("field", &format!("{model}#state"), json!({"type": "Selection"})),
                ("selection", &format!("{model}#state:draft"), json!({"label": "Draft"})),
                ("selection", &format!("{model}#state:done"), json!({"label": "Done"})),
            ])
        };
        let (old, new) = (
            side("stock.production.lot", "owner_id"),
            side("stock.lot", "partner_id"),
        );
        let d = one(&Python, "addons/stock/models/stock_lot.py", &old, &new);
        let r = renames(&d);
        assert_eq!(r.len(), d.len(), "{:?}", ops(&d));
        let of = |old: &str| r.iter().find(|r| r.0 == old).unwrap();
        let owner = of("stock.production.lot#owner_id@stock");
        assert_eq!(owner.1, "stock.lot#partner_id@stock");
        assert_ne!(owner.3, ["parent"]);
        let draft = of("stock.production.lot#state:draft@stock");
        assert_eq!((draft.1, &draft.3), ("stock.lot#state:draft@stock", &vec!["parent"]));
    }

    #[test]
    fn nested_and_dotted_keys_compare_leaf_by_leaf() {
        let attrs = json!({"a": {"x": 1, "y": "same"}, "a.b": "same too", "type": "T", "z": "also"});
        let old = table(&[("field", "sale.order#one", attrs.clone())]);
        let new = table(&[("field", "sale.order#two", attrs)]);
        assert_eq!(renames(&one(&Python, SALE, &old, &new))[0].2, 1.0);
    }

    #[test]
    fn a_renamed_view_takes_its_nodes_along() {
        let view = |id: &str| {
            table(&[
                ("view", id, json!({"model": "sale.order", "type": "form", "arch": "a1"})),
                (
                    "view_node",
                    &format!("{id}#field[@name='partner_id']"),
                    json!({"count": 1, "path": []}),
                ),
                (
                    "view_node",
                    &format!("{id}#button[@name='action_confirm']"),
                    json!({"count": 1, "path": []}),
                ),
            ])
        };
        let (old, new) = (view("view_order_form"), view("sale_order_view_form"));
        let d = one(&Data, "addons/sale/views/sale_views.xml", &old, &new);
        let r = renames(&d);
        assert_eq!(r.len(), 3, "{:?}", ops(&d));
        assert_eq!(
            r.iter().find(|r| r.3 != ["parent"]).unwrap().0,
            "sale.view_order_form@sale"
        );
        assert!(
            r.iter()
                .any(|r| r.1 == "sale.sale_order_view_form#field[@name='partner_id']@sale")
        );
    }

    #[test]
    fn an_ambiguous_move_is_no_rename() {
        let t = table(&[(
            "field",
            "sale.order#foo",
            json!({"type": "Char", "comodel": "x", "args": {"compute": "_c"}}),
        )]);
        let files: [(Side, Side); 3] = [
            (Some(("addons/sale/models/a.py", &t)), None),
            (Some(("addons/sale/models/b.py", &t)), None),
            (None, Some(("addons/sale/models/c.py", &t))),
        ];
        let d = delta(&Python, &files);
        assert_eq!(
            d.iter().map(|d| d.op).collect::<Vec<_>>(),
            [Op::Removed, Op::Removed, Op::Added]
        );
    }

    #[test]
    fn pairs_are_one_to_one_and_ties_go_to_the_lowest_id() {
        let body = json!({"body": "h1"});
        let old = table(&[
            ("method", "sale.order#_a", body.clone()),
            ("method", "sale.order#_b", body.clone()),
        ]);
        let new = table(&[("method", "sale.order#_c", body)]);
        let d = one(&Python, SALE, &old, &new);
        let lowest = ["sale.order#_a@sale", "sale.order#_b@sale"]
            .into_iter()
            .min_by_key(|q| SymbolId::new("method", q))
            .unwrap();
        assert_eq!(renames(&d).iter().map(|r| r.0).collect::<Vec<_>>(), [lowest]);
        assert_eq!(d.iter().filter(|d| d.op == Op::Removed).count(), 1);
    }

    #[test]
    fn replaced_references_lift_a_weak_pair() {
        let field = |name: &str, label: &str| {
            (
                "field",
                format!("sale.order#{name}"),
                json!({"type": "Many2one", "comodel": "res.partner", "args": {"string": label, "index": "btree_not_null"}}),
            )
        };
        let related = |n: usize, target: &str| {
            (
                "field",
                format!("sale.order#rel{n}"),
                json!({"type": "Char", "args": {"related": format!("{target}.name")}}),
            )
        };
        let build = |symbols: Vec<(&str, String, Value)>| {
            let refs: Vec<(&str, &str, Value)> = symbols.iter().map(|(k, n, a)| (*k, n.as_str(), a.clone())).collect();
            table(&refs)
        };
        let old = build(vec![
            field("partner_id", "Customer"),
            related(1, "partner_id"),
            related(2, "partner_id"),
        ]);
        let new = build(vec![
            field("buyer_id", "Buyer"),
            related(1, "buyer_id"),
            related(2, "buyer_id"),
        ]);
        let d = one(&Python, SALE, &old, &new);
        assert_eq!(
            renames(&d),
            [(
                "sale.order#partner_id@sale",
                "sale.order#buyer_id@sale",
                0.65,
                vec!["comodel", "type", "refs"]
            )]
        );

        let old = build(vec![field("partner_id", "Customer")]);
        let new = build(vec![field("buyer_id", "Buyer")]);
        assert!(renames(&one(&Python, SALE, &old, &new)).is_empty());
    }

    #[test]
    fn a_scope_past_the_pair_limit_is_not_scored() {
        let side = |prefix: &str| {
            let symbols: Vec<(String, Value)> = (0..317)
                .map(|i| (format!("sale.order#{prefix}{i}"), json!({"body": format!("h{i}")})))
                .collect();
            let refs: Vec<(&str, &str, Value)> =
                symbols.iter().map(|(n, a)| ("method", n.as_str(), a.clone())).collect();
            table(&refs)
        };
        let (old, new) = (side("old"), side("new"));
        assert!(renames(&one(&Python, SALE, &old, &new)).is_empty());
    }

    #[test]
    fn names_match_whole() {
        let found = |text: &str, name: &str| occurrences(text, name).collect::<Vec<_>>();
        assert_eq!(found(r#"{"comodel":"mail.channel"}"#, "mail.channel"), [12]);
        assert!(found(r#"["mail.channel.member"]"#, "mail.channel").is_empty());
        assert_eq!(found("partner_id.name", "partner_id"), [0]);
        assert!(found("partner_id_x", "partner_id").is_empty());
        assert_eq!(replace("a.b a.bc a.b.c", "a.b", "x"), "x a.bc a.b.c");
    }

    #[test]
    fn parents_of_python_data_and_js_symbols() {
        let p =
            |ex: &dyn Extractor, kind: &str, q: &'static str| ex.parent(kind, q).map(|p| (p.kind, p.qualname, p.local));
        let own = |kind: &'static str, q: &str, local: &'static str| Some((kind, q.to_string(), local));
        assert_eq!(
            p(&Python, "field", "sale.order#partner_id@sale"),
            own("model", "sale.order@sale", "partner_id")
        );
        assert_eq!(
            p(&Python, "selection", "sale.order#state:draft@sale"),
            own("field", "sale.order#state@sale", "draft")
        );
        assert_eq!(p(&Python, "model", "sale.order@sale"), None);
        assert_eq!(
            p(&Data, "view_node", "sale.view#field[@name='x']@sale"),
            own("view", "sale.view@sale", "field[@name='x']")
        );
        assert_eq!(p(&Data, "data_function", "res.partner#write@sale"), None);
        assert_eq!(
            p(&crate::Js, "js_export", "@web/core/registry#registry"),
            own("js_module", "@web/core/registry", "registry")
        );
    }
}
