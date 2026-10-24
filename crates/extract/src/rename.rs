use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::delta::unscoped;
use crate::sketch;
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
/// Lowest similarity of two sketches at which differing hashes count, in part, for a pair.
const SIMILAR: f64 = 0.5;
/// Booleans, numbers, empty values and one-character strings, which many unrelated symbols share.
const TRIVIAL_WEIGHT: f64 = 0.5;
/// How much more a hash or a trivial attr both sides hold with different values counts against a
/// pair than an attr only one side holds.
const CONFLICT: f64 = 2.0;
/// Weight of the share of words the old and new names have in common, matched as if it were one
/// more attr, and never counted against a pair.
const NAME_WEIGHT: f64 = 2.0;
/// How far, in thousandths, a pair must score above any other its removal or its addition could
/// still take, so that near-equal candidates, as in a renumbered chart of accounts, pair with none.
const MARGIN: u32 = 60;
/// Share of words from which the names count among a pair's signals.
const NAME_SIGNAL: f64 = 0.5;
const REF_BOOST: f64 = 0.1;
const MAX_REF_BOOST: f64 = 0.3;
/// Removed × added pairs past which a scope is not scored: a mass rewrite, such as a regenerated
/// chart of accounts, is not worth the quadratic cost.
const MAX_PAIRS: usize = 250_000;
/// Shortest old name also read as the new one inside an identifier, as in `_compute_<name>`: a
/// shorter one, such as `id`, is part of too many.
const SEGMENT_MIN: usize = 4;

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
    /// `similar`, `words`, `members`, `refs` and `parent` when they counted.
    pub signals: Vec<String>,
    /// The qualnames whose change in the same commit replaced the old name with the new one,
    /// sorted.
    pub referrers: Vec<String>,
    /// How alike the sketches are, in steps of 0.001, when a hash differs and counted as `similar`.
    pub similarity: Option<f64>,
}

/// A symbol the commit changed or moved, whose attrs may name a renamed one.
pub(crate) struct Change<'a> {
    pub qualname: &'a str,
    pub before: &'a str,
    pub after: &'a str,
}

/// What a pair's score rests on: its signals and the similarity of its bodies.
struct Evidence {
    signals: Vec<String>,
    similarity: Option<f64>,
}

/// A symbol a commit removed or added, which no move claimed.
pub(crate) struct Item<'a> {
    pub kind: &'a str,
    pub qualname: &'a str,
    pub path: &'a str,
    pub attrs: &'a BTreeMap<String, Value>,
    pub sketch: Option<&'a str>,
}

struct Node<'a> {
    kind: &'a str,
    qualname: &'a str,
    id: SymbolId,
    module: Option<&'a str>,
    unscoped: &'a str,
    parent: Option<(&'static str, String)>,
    /// The parent's qualname without its `@<module>`, which members are scored across modules by.
    family: Option<String>,
    local: &'a str,
    words: BTreeSet<String>,
    depth: usize,
    sketch: Option<&'a str>,
}

/// `(top-level key, path below it, canonical JSON)` of one leaf of a symbol's attrs.
type Leaf<'a> = (&'a str, String, String);

/// The `(score, other side)` candidates of each removal or each addition, best first.
type Candidates = BTreeMap<usize, Vec<(u32, usize)>>;

/// The children of each parent, by `(kind, qualname)`, as `(kind, local)`.
type Children<'n> = BTreeMap<(&'n str, &'n str), BTreeSet<(&'n str, &'n str)>>;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Scope<'n> {
    Module(&'n str, Option<&'n str>),
    Parent(&'n str, &'static str, &'n str),
    Kind(&'n str),
    Family(&'n str, &'static str, &'n str),
}

/// What every pass of one commit reads.
struct Sides<'n, 'r> {
    old: &'n [Node<'n>],
    new: &'n [Node<'n>],
    new_leaves: &'n [Vec<Leaf<'n>>],
    old_children: Children<'n>,
    new_children: Children<'n>,
    refs: &'r [Change<'r>],
}

/// The `(removed, added)` index pairs that are renames, each with its evidence. Parents pair
/// before their members; members of a renamed parent pair by name first, then are scored with
/// the new parent's other members. Each depth is scored in scopes across modules, or by module
/// past `MAX_PAIRS`, and once more when a symbol was renamed, its old name read as the new one in
/// what is left wherever the commit renamed that name to one other only. A change in `refs` that
/// replaces a reference to the old name adds to a pair's confidence and lists the change among its
/// referrers.
pub(crate) fn pairs(
    ex: &dyn Extractor,
    removed: &[Item],
    added: &[Item],
    refs: &[Change],
) -> Vec<(usize, usize, Rename)> {
    if removed.is_empty() || added.is_empty() {
        return Vec::new();
    }
    let old: Vec<Node> = removed.iter().map(|i| node(ex, i)).collect();
    let new: Vec<Node> = added.iter().map(|i| node(ex, i)).collect();
    let mut old_leaves: Vec<Vec<Leaf>> = removed.iter().map(|i| leaves(i.attrs)).collect();
    let new_leaves: Vec<Vec<Leaf>> = added.iter().map(|i| leaves(i.attrs)).collect();
    let sides = Sides {
        old: &old,
        new: &new,
        new_leaves: &new_leaves,
        old_children: children(&old),
        new_children: children(&new),
        refs,
    };
    let mut taken = Taken {
        old: vec![false; old.len()],
        new: vec![false; new.len()],
        renamed: BTreeMap::new(),
        names: BTreeMap::new(),
        ambiguous: BTreeSet::new(),
        learned: 0,
        refs,
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
            let Some(&(to, _, milli)) = taken.renamed.get(&(*kind, qualname.as_str())) else {
                continue;
            };
            if let Some(&j) = by_name.get(&(old[i].kind, *kind, to, old[i].local))
                && !taken.new[j]
            {
                let evidence = Evidence {
                    signals: vec!["parent".into()],
                    similarity: None,
                };
                taken.take(&old[i], &new[j], (i, j), milli, evidence);
            }
        }

        let mut read = 0;
        for round in 0..2 {
            if taken.learned > read {
                for i in (0..old.len()).filter(|&i| old[i].depth == depth && !taken.old[i]) {
                    for leaf in &mut old_leaves[i] {
                        leaf.2 = rewrite(&leaf.2, &taken.names);
                    }
                }
                read = taken.learned;
            } else if round > 0 {
                break;
            }
            let scopes = scopes(&sides, &taken, depth);
            assign(&sides, &old_leaves, &mut taken, scopes);
        }
    }
    taken.out
}

/// The removals and additions of `depth` no pair has taken, by scope: across modules, or by module
/// where that would make more than `MAX_PAIRS` pairs.
fn scopes(s: &Sides, taken: &Taken, depth: usize) -> Vec<(Vec<usize>, Vec<usize>)> {
    type Groups<'n> = BTreeMap<Scope<'n>, BTreeMap<Scope<'n>, (Vec<usize>, Vec<usize>)>>;
    let mut wide: Groups = BTreeMap::new();
    for i in (0..s.old.len()).filter(|&i| s.old[i].depth == depth && !taken.old[i]) {
        let o = &s.old[i];
        let (narrow, broad) = match &o.parent {
            None => (Scope::Module(o.kind, o.module), Scope::Kind(o.kind)),
            Some((kind, qualname)) => match taken.renamed.get(&(*kind, qualname.as_str())) {
                Some(&(to, family, _)) => (Scope::Parent(o.kind, kind, to), Scope::Family(o.kind, kind, family)),
                None => (
                    Scope::Parent(o.kind, kind, qualname),
                    Scope::Family(o.kind, kind, o.family.as_deref().unwrap_or_default()),
                ),
            },
        };
        wide.entry(broad).or_default().entry(narrow).or_default().0.push(i);
    }
    for j in (0..s.new.len()).filter(|&j| s.new[j].depth == depth && !taken.new[j]) {
        let n = &s.new[j];
        let (narrow, broad) = match &n.parent {
            None => (Scope::Module(n.kind, n.module), Scope::Kind(n.kind)),
            Some((kind, qualname)) => (
                Scope::Parent(n.kind, kind, qualname),
                Scope::Family(n.kind, kind, n.family.as_deref().unwrap_or_default()),
            ),
        };
        wide.entry(broad).or_default().entry(narrow).or_default().1.push(j);
    }
    let mut out = Vec::new();
    for narrow in wide.into_values() {
        let (olds, news): (usize, usize) = narrow.values().fold((0, 0), |(o, n), g| (o + g.0.len(), n + g.1.len()));
        if olds * news <= MAX_PAIRS {
            let (mut olds, mut news) = (Vec::new(), Vec::new());
            for (o, n) in narrow.into_values() {
                olds.extend(o);
                news.extend(n);
            }
            out.push((olds, news));
        } else {
            out.extend(narrow.into_values());
        }
    }
    out
}

/// Scores the pairs of each scope and takes them one to one, best score first and the lowest
/// symbol ids on a tie, each only when it beats by `MARGIN` every other pair its removal or its
/// addition could still take.
fn assign<'n>(
    s: &Sides<'n, '_>,
    old_leaves: &[Vec<Leaf>],
    taken: &mut Taken<'n, '_>,
    scopes: Vec<(Vec<usize>, Vec<usize>)>,
) {
    let (old, new) = (s.old, s.new);
    for (olds, news) in scopes {
        if olds.is_empty() || news.is_empty() || olds.len() * news.len() > MAX_PAIRS {
            continue;
        }
        let floor = THRESHOLD - MARGIN;
        let mut candidates = Vec::new();
        for &i in &olds {
            // One name on both sides is a move the move pass found ambiguous, never a rename.
            for &j in news.iter().filter(|&&j| new[j].unscoped != old[i].unscoped) {
                let (milli, evidence) = score(s, (i, &old_leaves[i]), j);
                if milli >= floor {
                    candidates.push((milli, i, j, evidence));
                }
            }
        }
        candidates.sort_by(|a, b| {
            (b.0.cmp(&a.0))
                .then(old[a.1].id.cmp(&old[b.1].id))
                .then(new[a.2].id.cmp(&new[b.2].id))
        });
        // The scores of each removal's and each addition's candidates, for the runner-up check.
        let (mut by_old, mut by_new): (Candidates, Candidates) = (BTreeMap::new(), BTreeMap::new());
        for &(milli, i, j, _) in &candidates {
            by_old.entry(i).or_default().push((milli, j));
            by_new.entry(j).or_default().push((milli, i));
        }
        let runner_up = |list: &[(u32, usize)], other: usize, free: &dyn Fn(usize) -> bool| {
            (list.iter())
                .find(|&&(_, k)| k != other && free(k))
                .map_or(0, |&(milli, _)| milli)
        };
        for (milli, i, j, evidence) in candidates {
            if taken.old[i] || taken.new[j] || milli < THRESHOLD {
                continue;
            }
            let second =
                runner_up(&by_old[&i], j, &|k| !taken.new[k]).max(runner_up(&by_new[&j], i, &|k| !taken.old[k]));
            if milli >= second + MARGIN {
                taken.take(&old[i], &new[j], (i, j), milli, evidence);
            }
        }
    }
}

struct Taken<'n, 'r> {
    old: Vec<bool>,
    new: Vec<bool>,
    /// Old `(kind, qualname)` to the new qualname, the new one without `@<module>`, and the
    /// confidence in thousandths.
    renamed: BTreeMap<(&'n str, &'n str), (&'n str, &'n str, u32)>,
    /// The old name of each renamed symbol to its new one, which the symbols left may still
    /// reference, as a field's `related` path names a field renamed on another model.
    names: BTreeMap<&'n str, &'n str>,
    /// Old names renamed to more than one new name, as `name` on two models, which are not read.
    ambiguous: BTreeSet<&'n str>,
    /// How many times `names` changed, so that a round reads it again only when it did.
    learned: usize,
    refs: &'r [Change<'r>],
    out: Vec<(usize, usize, Rename)>,
}

impl<'n> Taken<'n, '_> {
    fn take(&mut self, o: &Node<'n>, n: &Node<'n>, (i, j): (usize, usize), milli: u32, evidence: Evidence) {
        self.old[i] = true;
        self.new[j] = true;
        self.renamed
            .insert((o.kind, o.qualname), (n.qualname, n.unscoped, milli));
        if o.local != n.local && !self.ambiguous.contains(o.local) {
            match self.names.insert(o.local, n.local) {
                Some(to) if to != n.local => {
                    self.names.remove(o.local);
                    self.ambiguous.insert(o.local);
                }
                Some(_) => {}
                None => self.learned += 1,
            }
        }
        let confidence = f64::from(milli) / 1000.0;
        let referrers: BTreeSet<&str> = (self.refs.iter())
            .filter(|c| replaces(c.before, c.after, o.local, n.local))
            .map(|c| c.qualname)
            .collect();
        let referrers = referrers.into_iter().map(String::from).collect();
        let Evidence { signals, similarity } = evidence;
        self.out.push((
            i,
            j,
            Rename {
                confidence,
                signals,
                referrers,
                similarity,
            },
        ));
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
    let local = parent
        .as_ref()
        .map_or_else(|| unscoped(ex, item.path, item.qualname), |p| p.local);
    Node {
        kind: item.kind,
        qualname: item.qualname,
        id: SymbolId::new(item.kind, item.qualname),
        module: ex.module(item.path),
        unscoped: unscoped(ex, item.path, item.qualname),
        family: (parent.as_ref()).map(|p| unscoped(ex, item.path, &p.qualname).to_string()),
        parent: parent.map(|p| (p.kind, p.qualname)),
        local,
        words: words(local),
        depth,
        sketch: item.sketch,
    }
}

fn leaves(attrs: &BTreeMap<String, Value>) -> Vec<Leaf<'_>> {
    let mut out = Vec::new();
    for (key, value) in attrs {
        flatten(key, String::new(), value, &mut out);
    }
    // `attr_score` merge-joins on this order, which depth-first output breaks: `a` before `a.b`.
    out.sort_unstable();
    out
}

fn flatten<'a>(top: &'a str, path: String, value: &Value, out: &mut Vec<Leaf<'a>>) {
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

/// The confidence in thousandths of pairing removal `i`, with its leaves as they now read, and
/// addition `j`, and its signals.
fn score(s: &Sides, (i, old_leaves): (usize, &[Leaf]), j: usize) -> (u32, Evidence) {
    let (o, n) = (&s.old[i], &s.new[j]);
    let (mut score, mut evidence) = attr_score((o, old_leaves), (n, &s.new_leaves[j]));
    let signals = &mut evidence.signals;
    let kids = (
        s.old_children.get(&(o.kind, o.qualname)),
        s.new_children.get(&(n.kind, n.qualname)),
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
        let replaced = (s.refs.iter())
            .filter(|c| replaces(c.before, c.after, o.local, n.local))
            .count();
        if replaced > 0 {
            score += (replaced as f64 * REF_BOOST).min(MAX_REF_BOOST);
            signals.push("refs".into());
        }
    }
    (milli(score.min(1.0)), evidence)
}

/// Matched weight over total weight, scaled down while the matched weight is below `EVIDENCE`,
/// and the top-level keys whose every leaf matched. Differing hashes whose sketches are at least
/// `SIMILAR` alike match in proportion and give the signal `similar`. Any other differing attr but
/// a trivial one, such as a label or a reference, counts once and matches for the words its values
/// share. The names' shared words weigh `NAME_WEIGHT` and give the signal `words`.
fn attr_score((o, a): (&Node, &[Leaf]), (n, b): (&Node, &[Leaf])) -> (f64, Evidence) {
    let (mut matched, mut total) = (0.0, 0.0);
    let mut similarity = None;
    let (mut keys, mut unequal) = (BTreeSet::new(), BTreeSet::new());
    let words_shared = shared(&o.words, &n.words);
    let (mut a, mut b) = (a.iter().peekable(), b.iter().peekable());
    loop {
        // The leaf's top-level key, its weight and whether it matched.
        let (top, w, equal) = match (a.peek().copied(), b.peek().copied()) {
            (None, None) => break,
            (Some(x), Some(y)) if (x.0, &x.1) == (y.0, &y.1) => {
                a.next();
                b.next();
                let alike = (o.sketch.zip(n.sketch))
                    .filter(|_| HASHES.contains(&x.0))
                    .and_then(|(a, b)| sketch::similarity(a, b))
                    .filter(|&s| s >= SIMILAR);
                let own = (x.2 != y.2).then(|| replace(&x.2, o.local, n.local));
                let share = if own.as_ref().is_none_or(|own| *own == y.2) {
                    None
                } else if let Some(s) = alike {
                    similarity = Some(f64::from(milli(s)) / 1000.0);
                    Some(HASH_WEIGHT * s)
                } else if !HASHES.contains(&x.0) && weight(x.0, &x.2) == 1.0 {
                    // A rename usually rewrites its label, and a reference may follow another rename.
                    Some(shared(&words(own.as_deref().unwrap_or_default()), &words(&y.2)))
                } else {
                    keys.insert(x.0);
                    unequal.insert(x.0);
                    total += CONFLICT * weight(x.0, &x.2);
                    continue;
                };
                match share {
                    None => (x.0, weight(x.0, &x.2), true),
                    Some(part) => {
                        matched += part;
                        total += weight(x.0, &x.2);
                        keys.insert(x.0);
                        unequal.insert(x.0);
                        continue;
                    }
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
    // Counted only for the words shared, since renames as often pick an unrelated name as a close one.
    let name = NAME_WEIGHT * words_shared;
    let score = if matched == 0.0 {
        0.0
    } else {
        (matched + name) / (total + name) * ((matched + name) / EVIDENCE).min(1.0)
    };
    let mut signals: Vec<String> = keys.difference(&unequal).map(|k| k.to_string()).collect();
    if similarity.is_some() {
        signals.push("similar".into());
    }
    if words_shared >= NAME_SIGNAL {
        signals.push("words".into());
    }
    (score, Evidence { signals, similarity })
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

/// The lowercase words of a name or label: runs of letters and digits, split again where a
/// capital follows a small letter or a digit, as in `AccountMove`.
fn words(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut word = String::new();
    let mut after_small = false;
    for c in text.chars() {
        if !c.is_alphanumeric() || (c.is_uppercase() && after_small) {
            out.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
        }
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        }
        after_small = c.is_lowercase() || c.is_numeric();
    }
    out.extend((!word.is_empty()).then_some(word));
    out
}

/// The share of words two sets have in common, 0 when both are empty. A word of two letters or more
/// matches one it begins, as `mo` does `mon`.
fn shared(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let numbers = |set: &BTreeSet<String>| -> BTreeSet<String> {
        set.iter()
            .filter(|w| w.chars().all(|c| c.is_ascii_digit()))
            .cloned()
            .collect()
    };
    let (x, y) = (numbers(a), numbers(b));
    if !x.is_empty() && !y.is_empty() && x.is_disjoint(&y) {
        return 0.0;
    }
    let alike = |w: &String, v: &String| {
        w == v || (w.len().min(v.len()) >= 2 && (w.starts_with(v.as_str()) || v.starts_with(w.as_str())))
    };
    let common = a.iter().filter(|w| b.iter().any(|v| alike(w, v))).count();
    let union = (a.len() + b.len()).saturating_sub(common);
    if union == 0 {
        0.0
    } else {
        (common as f64 / union as f64).min(1.0)
    }
}

/// Whether a change replaced a reference to `old` with one to `new`: the old side names `old`
/// and not `new`, the new side the reverse.
fn replaces(before: &str, after: &str, old: &str, new: &str) -> bool {
    let names = |text: &str, name: &str| occurrences(text, name, false).next().is_some();
    names(before, old) && !names(before, new) && names(after, new) && !names(after, old)
}

/// `text` with each whole-name occurrence of `old` read as `new`, and each occurrence between
/// underscores too when `old` is at least `SEGMENT_MIN` long, so that a field's compute method
/// follows its rename.
fn replace(text: &str, old: &str, new: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for at in occurrences(text, old, old.len() >= SEGMENT_MIN) {
        out.push_str(&text[last..at]);
        out.push_str(new);
        last = at + old.len();
    }
    out.push_str(&text[last..]);
    out
}

/// `text` with each whole name `names` holds read as the name it maps to, found as `occurrences`
/// finds one: a dotted name only where no dot follows it.
fn rewrite(text: &str, names: &BTreeMap<&str, &str>) -> String {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(word) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c: char| !word(c) && c != '.').unwrap_or(rest.len());
        let mut run = &rest[..end];
        rest = &rest[end..];
        // A dotted name runs to the end of the dotted run; a plain one is any segment of it.
        loop {
            if run.contains('.')
                && let Some(to) = names.get(run)
            {
                out.push_str(to);
                break;
            }
            let (segment, tail) = run.split_once('.').map_or((run, None), |(s, t)| (s, Some(t)));
            out.push_str(names.get(segment).copied().unwrap_or(segment));
            match tail {
                Some(tail) => {
                    out.push('.');
                    run = tail;
                }
                None => break,
            }
        }
    }
    out.push_str(rest);
    out
}

/// Where `name` occurs in `text` as a whole name: between characters that cannot continue an
/// identifier, or `within` one between underscores, and, for a dotted name such as a model's, not
/// followed by a dot, so that `mail.channel` is not found in `mail.channel.member`.
fn occurrences<'t>(text: &'t str, name: &'t str, within: bool) -> impl Iterator<Item = usize> + 't {
    let word = move |c: u8| c.is_ascii_alphanumeric() || (c == b'_' && !within);
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
                parts: None,
                sketch: None,
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
                0.917,
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
    fn a_related_field_follows_the_field_renamed_on_another_model() {
        let company = |field: &str| {
            let attrs =
                json!({"type": "Boolean", "args": {"string": "Test environment", "help": "Send to the test server"}});
            table(&[("field", &format!("res.company#{field}"), attrs)])
        };
        let settings = |field: &str, target: &str| {
            let attrs =
                json!({"type": "Boolean", "args": {"related": format!("company_id.{target}"), "readonly": false}});
            table(&[("field", &format!("res.config.settings#{field}"), attrs)])
        };
        let (old_company, new_company) = (company("edi_test_env"), company("sii_test_env"));
        let (old_settings, new_settings) = (
            settings("use_test", "edi_test_env"),
            settings("sandbox", "sii_test_env"),
        );
        let files: [(Side, Side); 2] = [
            (
                Some(("addons/l10n_es/models/res_company.py", &old_company)),
                Some(("addons/l10n_es/models/res_company.py", &new_company)),
            ),
            (
                Some(("addons/l10n_es/models/res_config_settings.py", &old_settings)),
                Some(("addons/l10n_es/models/res_config_settings.py", &new_settings)),
            ),
        ];
        let d = delta(&Python, &files);
        let pairs: Vec<_> = renames(&d).iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            pairs,
            [
                ("res.company#edi_test_env@l10n_es", "res.company#sii_test_env@l10n_es"),
                (
                    "res.config.settings#use_test@l10n_es",
                    "res.config.settings#sandbox@l10n_es"
                ),
            ]
        );
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
    fn members_of_other_models_never_pair_but_those_of_other_modules_do() {
        let attrs = json!({"type": "Many2one", "comodel": "res.partner", "args": {"compute": "_c"}});
        let old = table(&[("field", "sale.order#partner_id", attrs.clone())]);
        let new = table(&[("field", "sale.order.line#customer_id", attrs.clone())]);
        assert!(renames(&one(&Python, SALE, &old, &new)).is_empty());

        let other = table(&[("field", "sale.order#customer_id", attrs)]);
        let files: [(Side, Side); 2] = [
            (Some((SALE, &old)), None),
            (None, Some(("addons/sale_stock/models/sale_order.py", &other))),
        ];
        let d = delta(&Python, &files);
        let pairs: Vec<_> = renames(&d).iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            pairs,
            [("sale.order#partner_id@sale", "sale.order#customer_id@sale_stock")]
        );
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
    fn a_pair_must_clear_the_runner_up_of_both_its_symbols() {
        let side = |symbols: &[(&str, &str)]| {
            let symbols: Vec<(&str, String, Value)> = (symbols.iter())
                .map(|(name, body)| ("method", format!("sale.order#{name}"), json!({"body": body})))
                .collect();
            let refs: Vec<(&str, &str, Value)> = symbols.iter().map(|(k, n, a)| (*k, n.as_str(), a.clone())).collect();
            table(&refs)
        };
        let tie = one(
            &Python,
            SALE,
            &side(&[("_a", "h1"), ("_b", "h1")]),
            &side(&[("_c", "h1")]),
        );
        assert!(renames(&tie).is_empty(), "{:?}", ops(&tie));

        let clear = one(
            &Python,
            SALE,
            &side(&[("_a", "h1"), ("_b", "h2")]),
            &side(&[("_c", "h1")]),
        );
        assert_eq!(
            renames(&clear).iter().map(|r| r.0).collect::<Vec<_>>(),
            ["sale.order#_a@sale"]
        );
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
            field("customer", "Customer"),
            related(1, "customer"),
            related(2, "customer"),
        ]);
        let new = build(vec![field("buyer", "Buyer"), related(1, "buyer"), related(2, "buyer")]);
        let d = one(&Python, SALE, &old, &new);
        assert_eq!(
            renames(&d),
            [(
                "sale.order#customer@sale",
                "sale.order#buyer@sale",
                0.763,
                vec!["comodel", "type", "refs"]
            )]
        );

        let old = build(vec![field("customer", "Customer")]);
        let new = build(vec![field("buyer", "Buyer")]);
        assert!(renames(&one(&Python, SALE, &old, &new)).is_empty());
    }

    #[test]
    fn a_scope_past_the_pair_limit_is_not_scored() {
        let side = |prefix: &str| {
            let symbols: Vec<(String, Value)> = (0..501)
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
        let found = |text: &str, name: &str| occurrences(text, name, false).collect::<Vec<_>>();
        assert_eq!(found(r#"{"comodel":"mail.channel"}"#, "mail.channel"), [12]);
        assert!(found(r#"["mail.channel.member"]"#, "mail.channel").is_empty());
        assert_eq!(found("partner_id.name", "partner_id"), [0]);
        assert!(found("partner_id_x", "partner_id").is_empty());
        assert_eq!(replace("a.b a.bc a.b.c", "a.b", "x"), "x a.bc a.b.c");
        assert_eq!(
            replace("_compute_kanban_state kanban_states", "kanban_state", "state"),
            "_compute_state kanban_states"
        );
        assert_eq!(replace("_compute_uid", "uid", "user"), "_compute_uid");
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

    #[test]
    fn words_match_whole_or_by_prefix_and_numbers_must_agree() {
        let share = |a: &str, b: &str| shared(&words(a), &words(b));
        assert_eq!(words("GardenPlotNote"), words("garden.plot.note"));
        assert_eq!(share("mo", "mon"), 1.0);
        assert_eq!(share("coa_account_4011", "coa_account_4021"), 0.0);
        assert_eq!(share("coa_4011", "coa_account_4011"), 2.0 / 3.0);
        assert_eq!(share("", ""), 0.0);
    }

    #[test]
    fn rewrite_reads_renamed_names_as_occurrences_finds_them() {
        let names = BTreeMap::from([("sale.order", "sale.ord"), ("partner_id", "customer_id")]);
        assert_eq!(
            rewrite(r#"["sale.order","sale.order.line"]"#, &names),
            r#"["sale.ord","sale.order.line"]"#
        );
        assert_eq!(
            rewrite("partner_id.name x.partner_id", &names),
            "customer_id.name x.customer_id"
        );
        assert_eq!(rewrite("a..b.", &names), "a..b.");
    }

    #[test]
    fn a_wide_scope_past_the_pair_limit_falls_back_to_modules() {
        let side = |module: &str, prefix: &str, n: usize| {
            let symbols: Vec<(String, Value)> = (0..n)
                .map(|i| {
                    (
                        format!("{prefix}_{i}"),
                        json!({"body": format!("{module}{i}"), "args": ["self"]}),
                    )
                })
                .collect();
            let refs: Vec<(&str, &str, Value)> = symbols
                .iter()
                .map(|(name, a)| ("function", name.as_str(), a.clone()))
                .collect();
            table(&refs)
        };
        // 600 × 450 pairs across both modules, but 1 × 1 within `sale`.
        let (big_old, big_new) = (side("x", "old", 600), side("y", "new", 450));
        let (old, new) = (side("s", "before", 1), side("s", "after", 1));
        let files: [(Side, Side); 3] = [
            (
                Some(("addons/stock/models/a.py", &big_old)),
                Some(("addons/stock/models/a.py", &big_new)),
            ),
            (Some((SALE, &old)), None),
            (None, Some(("addons/sale/models/b.py", &new))),
        ];
        let d = delta(&Python, &files);
        assert_eq!(renames(&d).len(), 1, "{:?}", renames(&d));
    }
}
