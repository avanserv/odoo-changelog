# Extract

`extract` diffs each walked commit against its parent and turns its changed files into symbol
events, as [Events](#events) describes. Merge commits are skipped, so each event belongs to the
commit that made the change. File renames are paired with git's default settings (50%
similarity), whatever the clone's config says, and file renames, moves and renames are paired only
among the files one extractor reads: an extractor's events never depend on which other extractors run. A symbol's id
is `blake3(kind, 0x00, qualname)`. Each blob is parsed
once per extractor version: the result is cached under the blob id, the extractor's name and its
version, and bumping the version re-extracts that extractor's commits only. `--from` and `--to`
bound the `version` of the commits extracted, and a later run picks up the rest.

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
scoring at least 0.6 becomes one `renamed` event. Pairs are only scored within a scope: members
of the same parent (the fields, methods and constraints of one model, a field's selection values,
a view's nodes, a JS module's exports, components and widgets), and any other symbol within its
kind and module. Parents pair first, and the members of a renamed parent follow it by name with
the parent's confidence: renaming `mail.channel` renames `mail.channel#name@mail` to
`discuss.channel#name@mail` too.

A pair's score is the share of attribute weight equal on both sides, an attribute counting as
equal when it differs only by the old name written as the new one, as `_name` does. The `body`,
`arch` and `values` hashes weigh 4, a boolean, number, empty value or one-character string 0.5,
any other attribute 1, and an attribute both sides hold with different values counts double
against the pair. A pair matching less than a weight of 4 scores in proportion, so two `Char`
fields sharing only their type, or two view nodes sharing only `count` and `path`, are no rename.
Two `body` hashes that differ count, instead of against the pair, for the share of their weight
that their bodies are alike: each Python def, JS function and SCSS mixin or function of at least
eight shingles of three tokens carries a 32-slot b-bit MinHash sketch of its normalized tokens,
cached with the symbol but never stored in an event, and two sketches estimating at least 0.5
similarity give that share. A renamed method with one line of its body edited so pairs below an
exact rename. The sketches take 107 MB of the 673 MB Python cache, 4 MB of 126 MB for JS, from
17.0 to 18.0 (2026-10-23). A parent averages that with the overlap of its members' names. Each change in the same
commit and extractor that replaces a reference to the old name with the new one, such as a
`comodel` moving to the renamed model, adds 0.1, up to 0.3, to a pair already scoring 0.4. Pairs are assigned one to one, best
score first and the lowest symbol id on a tie. A scope with more than 100,000 possible pairs, such
as a regenerated chart of accounts, is not scored. A symbol removed and added under one name is
never a rename: the move pass decides it.

Before scoring, a removal and an addition under one qualname whose kind changed in a way the
extractor declares pair as a rename with confidence 1 and the signal `kind`, when each is alone
under that qualname: a `rule` re-declared as an `access`, as master's conversion to `ir.access`
does. No other kinds pair, since a view and a record can share an id without being one thing.

`confidence` runs from 0.6 to 1 in steps of 0.001. `signals` is a JSON array of what matched: the
top-level attributes equal on both sides, then `similar`, `members`, `refs` and `parent` when
they counted. `similarity` is the estimate that made bodies count as `similar`, from 0.5 to 1.
`referrers` is a JSON array of the qualnames whose change in the same commit and extractor
replaced the old name with the new one, such as the templates whose `t-call` follows a renamed
template, whatever the score; a symbol still naming the old one is not listed.

```sql
SELECT o.qualname, s.qualname, e.confidence, e.signals FROM events e
  JOIN symbols s ON s.id = e.symbol_id
  JOIN symbols o ON o.id = e.old_symbol_id
 WHERE e.op = 'renamed' AND s.kind = 'model';
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
