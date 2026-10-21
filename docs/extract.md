# Extract

`extract` diffs each walked commit against its parent and turns every changed file into symbol
events: added, removed, changed (its attributes differ) or moved (a renamed file, same
attributes). Merge commits are skipped, so each event belongs to the commit that made the
change. Renames are paired with git's default settings (50% similarity), whatever the clone's
config says, but only among the files one extractor reads: an extractor's events never depend on
which other extractors run. A symbol's id is `blake3(kind, 0x00, qualname)`. Each blob is parsed
once per extractor version: the result is cached under the blob id, the extractor's name and its
version, and bumping the version re-extracts that extractor's commits only. `--from` and `--to`
bound the `version` of the commits extracted, and a later run picks up the rest.

Each extractor leaves a warning on the file's table for input it could read only in part, as its
page says, and `extract` reports how many file sides (the old or new version of a changed file)
carry one.

On 8 idle cores, extracting community from 17.0 to 18.0 (about 53,000 commits) with the
`python`, `manifest`, `data`, `js`, `owl` and `scss` extractors takes 167 s with an empty cache
and 51 s with a warm one (2026-10-21).

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
