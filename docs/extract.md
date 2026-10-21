# Extract

`extract` diffs each walked commit against its parent and turns its changed files into symbol
events, as [Events](#events) describes. Merge commits are skipped, so each event belongs to the
commit that made the change. Renames are paired with git's default settings (50% similarity),
whatever the clone's config says, and renames and moves are paired only among the files one
extractor reads: an extractor's events never depend on which other extractors run. A symbol's id
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

A symbol is `moved` when its file was renamed, or when the commit removes it from one file and
adds it to another under the same qualname once each side's `@<module>` suffix is stripped:
moving `sale.order#foo@sale` to `sale_management` gives one `moved` event for
`sale.order#foo@sale_management`, not a removal and an addition. A move is paired only when no
other symbol of that kind and unscoped name leaves or enters a file in the commit; otherwise the
removals and additions stay. A renamed symbol is no move: its qualname differs.

"The entries that differ" holds, on each side, the attributes whose value differs from the other
side, down into nested objects; a key only one side has shows on that side only. Making a field
declared with a `string` required gives `{"args":{}}` before and `{"args":{"required":true}}`
after.

`path` and `module` are the new side's, or the old side's for a removal; `module` is NULL for a
file outside any module. `old_symbol_id`, `old_path` and `old_module` say where a moved symbol
was, and are NULL for every other op.

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
