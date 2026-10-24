# Extract

`extract` diffs each walked commit against its parent and turns its changed files into symbol
events, as [Events](#events) describes. A merge records only what resolving it changed, as
[Merges](#merges) describes, so each event belongs to the commit that made the change. File
renames are paired with git's default settings (50% similarity), whatever the clone's config
says, and file renames, moves and renames are paired only among the files one extractor reads: an
extractor's events never depend on which other extractors run. A symbol's id is
`blake3(kind, 0x00, qualname)`. Each blob is parsed once per extractor version: the result is
cached under the blob id, the extractor's name and its version, and bumping the version
re-extracts that extractor's commits only. `--from` and `--to` bound the `version` of the
commits extracted, and a later run picks up the rest.

Each extractor leaves a warning on the file's table for input it could read only in part, as its
page says, and `extract` reports how many file sides (the old or new version of a changed file)
carry one.

On 8 idle cores, extracting community from 17.0 to 18.0 (about 53,000 commits) with the
`python`, `manifest`, `data`, `js`, `owl` and `scss` extractors takes 167 s with an empty cache
and 51 s with a warm one (2026-10-21).

## Events

Each row of `events` is one symbol in one commit, with its `op`:

| `op` | When | `attrs_before`, `attrs_after` |
| --- | --- | --- |
| `added`, `removed` | The symbol appears or disappears | The whole side that exists |
| `changed` | Same file, attributes differ | The entries that differ |
| `moved` | Another file than before | The entries that differ, both NULL when equal |
| `renamed` | Another qualname or kind than before, as [Renames](#renames) pairs it | The entries that differ, both NULL when equal |

A symbol is `moved` when its file was renamed, or when the commit removes it from one file and
adds it to another under the same qualname once each side's `@<module>` suffix is stripped:
moving `sale.order#foo@sale` to `sale_management` gives one `moved` event for
`sale.order#foo@sale_management`, not a removal and an addition. A move is paired only when no
other symbol of that kind and unscoped name leaves or enters a file in the commit; otherwise the
removals and additions stay. A symbol whose qualname changed is no move, but may be `renamed`.

"The entries that differ" holds, on each side, the attributes whose value differs from the other
side, down into nested objects; a key only one side has shows on that side only. Making a field
declared with a `string` required gives `{"args":{}}` before and `{"args":{"required":true}}`
after.

`path` and `module` are the new side's, or the old side's for a removal; `module` is NULL for a
file outside any module. `old_symbol_id`, `old_path` and `old_module` say where a moved or renamed
symbol was, and are NULL for every other op; the `symbols` row of `old_symbol_id` holds the old
kind. `confidence`, `signals`, `referrers` and `similarity` are a rename's, and NULL for every
other op. `category`, `breaking` and `rule_id` say what the change means to a user and
to custom code, as [Classify](classify.md) describes.

A forward-ported commit records its original in `commits.original_sha`, so its events reach the
original's through a join. The trailer may abbreviate the sha, and may name a commit the
database does not hold, so the join finds nothing for some forward ports.

```sql
SELECT o.* FROM events e
  JOIN commits c ON c.repo_id = e.repo_id AND c.sha = e.sha
  JOIN events o ON o.repo_id = c.repo_id AND o.sha = c.original_sha
               AND o.extractor = e.extractor AND o.symbol_id = e.symbol_id
 WHERE e.sha = :sha;
```

## Renames

The removals and additions a commit leaves once moves are paired are scored in pairs, and a pair
scoring at least 0.6 becomes one `renamed` event. Parents pair first, and the members of a renamed
parent follow it by name with the parent's confidence: renaming `mail.channel` renames
`mail.channel#name@mail` to `discuss.channel#name@mail` too. The rest is scored across modules: the
members of parents of one name (the fields, methods and constraints of models of one name, a
field's selection values, a view's nodes, a JS module's exports, components and widgets), and any
other symbol by kind. A field moved from `garden_rent` to `garden` with its rename so pairs, as
does an xmlid moved from `l10n_x` to `account_x`. Where that would make more than 100,000 possible
pairs, as a regenerated chart of accounts does, members are scored by parent and other symbols by
module, and a scope still past that is not scored. When a symbol without a parent was renamed, its
depth is scored once more with the old name read as the new one in the attributes of the symbols
left, so that a menu whose `action` follows a renamed action pairs too. A reference across
extractors, such as an action's `res_model` naming a model the same commit renamed, is not
followed.

A pair's score is the share of attribute weight equal on both sides, an attribute counting as
equal when it differs only by the old name written as the new one, as `_name` does. The `body`,
`arch` and `values` hashes weigh 4, a boolean, number, empty value or one-character string 0.5,
any other attribute 1. Such an attribute, a label or a reference, that both sides hold with
different values counts once, matched for the share of words the two values have in common: a
rename usually rewrites its label, as a field's `string` or a model's `_description` and `class`.
A hash or a trivial value that differs counts double against the pair. Words split at
non-alphanumerics and where a capital follows a small letter, so `GardenPlotNote` and
`garden.plot.note` share theirs, and a word of two letters or more matches one it begins,
as `mo` does `mon`. The old and new names' share of words adds to both sides of the ratio with a
weight of 2, so a close name raises a pair and an unrelated one costs nothing; names whose numbers
all differ, as `coa_4011` and `coa_4021`, share none. A pair matching less than a weight of 4,
the names' share included, scores in proportion, so two `Char` fields sharing only their type, or
two view nodes sharing only `count` and `path`, are no rename.

Two hashes that differ count, instead of against the pair, for the share of their weight that
their sketches say is alike, from 0.5 up. Each Python def, JS function and SCSS mixin or function
of at least eight shingles of three tokens carries a 32-slot b-bit MinHash sketch of its
normalized tokens; each data declaration a sketch of the set of its `values` parts and of the
elements and texts of a view's arch. Sketches are cached with the symbol but never stored in an
event. A renamed method with one line of its body edited, or a renamed view with one element of
its arch edited, so pairs below an exact rename. The Python and JS sketches take 107 MB of the 673
MB Python cache and 4 MB of 126 MB for JS, from 17.0 to 18.0 (2026-10-23). A parent averages its
score with the overlap of its members' names. Each change in the same commit and extractor that
replaces a reference to the old name with the new one, such as a `comodel` moving to the renamed
model, adds 0.1, up to 0.3, to a pair already scoring 0.4.

Pairs are assigned one to one, best score first and the lowest symbol id on a tie, and a pair is
taken only when it scores at least 0.06 above any other pair its removal or its addition could
still take: of two equal candidates, as in a renumbered chart of accounts, neither pairs. A symbol
removed and added under one name is never a rename: the move pass decides it.

Before scoring, a removal and an addition under one qualname whose kind changed in a way the
extractor declares pair as a rename with confidence 1 and the signal `kind`, when each is alone
under that qualname: a `rule` re-declared as an `access`, as master's conversion to `ir.access`
does. No other kinds pair, since a view and a record can share an id without being one thing.

`confidence` runs from 0.6 to 1 in steps of 0.001, and is 1 for a rename an upgrade script
declares, as [Matching](upgrade.md#matching) describes. `signals` is a JSON array of what matched: the
top-level attributes equal on both sides, then `similar`, `words` (the names share at least half
their words), `members`, `refs` and `parent` when they counted, and `upgrade` last for a rename
an upgrade script declares. `similarity` is the estimate that made hashes count as `similar`,
from 0.5 to 1.
`referrers` is a JSON array of the qualnames whose change in the same commit and extractor
replaced the old name with the new one, such as the templates whose `t-call` follows a renamed
template, whatever the score; a symbol still naming the old one is not listed.

```sql
SELECT o.qualname, s.qualname, e.confidence, e.signals FROM events e
  JOIN symbols s ON s.id = e.symbol_id
  JOIN symbols o ON o.id = e.old_symbol_id
 WHERE e.op = 'renamed' AND s.kind = 'model';
```

## Merges

A merge is diffed against each of its parents, on the files `git diff --cc` lists: those the merge
left unlike every parent at one same line, or between the same two lines, and those added or
deleted against a parent. A file each side changed in other places and merged cleanly gives no
event, even when both edited one method. On each file listed, a symbol gives an event only when it
differs from every parent in one same respect: its name, its path or one attribute value, down into
nested objects so that each keyword argument of a field counts on its own. A fix merged into a file
the other side renamed gives none: the symbol differs from one parent in its body and from the
other in its path. A hashed value, such as a method's body, is one value, so a method each side
edited cleanly still gives an event when its file holds a resolved conflict elsewhere. The event is
taken against a parent whose name and path for the symbol both differ, and keeps only the
attributes that differ from every parent.

Up to 13.0, stable branches were merged forward into master, and a conflict resolved there is
often where a fix was adapted to a rename or a refactor on the newer branch. On 2026-10-24, about
580 merges of `odoo` and `enterprise` resolved a conflict in a file an extractor reads, all in
12.0 to 13.0, and about 480 of them give 4,100 events. `commits.is_merge` tells a merge's events
from the rest:

```sql
SELECT e.* FROM events e
  JOIN commits c ON c.repo_id = e.repo_id AND c.sha = e.sha
 WHERE c.is_merge;
```

## Extractors

- [`python`](extractors/python.md): models, fields, methods, routes and plain Python code.
- [`manifest`](extractors/manifest.md): one module per manifest, with its dependencies, data and
  assets.
- [`data`](extractors/data.md): XML and CSV data files by xmlid, and views down to their named
  nodes.
- [`js`](extractors/js.md): JavaScript modules, exports, components, registry entries, patches
  and mail models.
- [`owl`](extractors/owl.md): client templates and their extensions.
- [`scss`](extractors/scss.md): SCSS variables, map entries, mixins, functions, placeholders and
  `:root` custom properties, and what each stylesheet includes, extends and imports.
