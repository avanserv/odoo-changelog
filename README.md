# odoo-changelog

[![CI](https://github.com/avanserv/odoo-changelog/actions/workflows/ci.yml/badge.svg)](https://github.com/avanserv/odoo-changelog/actions/workflows/ci.yml)

Deterministic, symbol-level history of Odoo community, enterprise and design-themes from 12.0
to master, stored in SQLite.

## Usage

Each [release](https://github.com/avanserv/odoo-changelog/releases) carries a Linux x86_64
binary; elsewhere, build it with `cargo build --release`. The tool reads full clones of the Odoo
repositories and never writes to them. Point a config at them first:

```sh
cp config/odoo-changelog.example.toml config/odoo-changelog.toml   # then edit the paths
./target/release/odoo-changelog walk            # all repos in config/odoo-changelog.toml
./target/release/odoo-changelog walk --repo odoo
./target/release/odoo-changelog stats
./target/release/odoo-changelog extract --repo odoo --from 17.0 --to 18.0 --timings
```

`walk` is incremental: it resumes each branch from the last recorded head and fails if a
branch was rewritten, or if a fork moved since the last walk (a new `[fork_overrides]` entry,
a changed `first_version`, a deepened clone), since either renumbers rows a resume does not
revisit. It also fails when a branch reaches a commit an earlier walk gave master, such as a
branch created from a commit master brought in through a merge: a full rebuild walks master
last and gives that commit to the branch. Rebuild the database in every case. Two runs on the
same refs produce identical databases, and an incremental walk equals a full rebuild.

## How versions are assigned

Each version branch's fork is where its first-parent chain meets master's. `git merge-base`
is not used: before 14.0 stable branches were merged forward into master, which moves the
merge-base to the last merged stable commit. When a shallow clone cuts the chain before the
fork, the community repo falls back to the parent of the master commit that bumped
`odoo/release.py` to the next series; `[fork_overrides]` in the config pins a fork by hand.

A commit on a version branch belongs to that version. A master commit belongs to the first
version forked after it (`version`) and to the X.0 release that includes it (`major`,
saas-17.2 → 18.0). Commits a merge brought in are recorded with the merge's `fp_idx` and a
`sub_idx` from 1, oldest first.

## How symbols are recorded

`extract` diffs each walked commit against its parent and turns every changed file into symbol
events: added, removed, changed (its attributes differ) or moved (a renamed file, same
attributes). Merge commits are skipped, so each event belongs to the commit that made the
change. Renames are paired with git's default settings (50% similarity), whatever the clone's
config says, but only among the files one extractor reads: an extractor's events never depend on
which other extractors run. A symbol's id is `blake3(kind, 0x00, qualname)`. Each blob is parsed
once per extractor version: the result is cached under the blob id, the extractor's name and its
version, and bumping the version re-extracts that extractor's commits only. `--from` and `--to`
bound the `version` of the commits extracted, and a later run picks up the rest.

### Python symbols

The `python` extractor reads every `.py` file whose path segments are all Python identifiers,
manifests aside: migration scripts and other files Python cannot import hold no model. It parses
with tree-sitter and names each symbol after what custom code refers to it by.

| Kind | Qualname | Example |
| --- | --- | --- |
| `model` | `<model>@<module>` | `sale.order@sale` |
| `field`, `method` | `<model>#<name>@<module>` | `sale.order#partner_id@sale` |
| `selection` | `<model>#<field>:<value>@<module>` | `sale.order#state:draft@sale` |
| `constraint` | `<model>#<name>@<module>` | `sale.order#date_order_conditional_required@sale` |
| `class`, `function`, `route` | `<import path>.<name>` | `odoo.addons.sale.controllers.portal.CustomerPortal.portal_my_orders` |

A model's name is its `_name`, else its single `_inherit`. Model-scope symbols carry the module
that declares them, so a base field and an override in another module have separate histories;
`[<Class>]` after the model tells apart two classes of one file that declare the same member,
such as the `res.users` classes in `base`. `class` and `function` cover classes that declare no
model and module-level functions, with `test*` methods left out; `route` is a `function` with an
`http.route` decorator. A `constraint` is an `_sql_constraints` entry or, from 19.0, a
`models.Constraint`, `UniqueIndex` or `Index`.

Attributes are normalized so that formatting is no change: literal values are decoded, other
expressions are kept as canonical text, and a method's `body` is a hash of its tokens without
comments, docstring or local names. Each symbol also records the line and byte span of its name,
which change detection ignores. A file that does not fully parse, such as Python 2 code, still
gives the symbols around the error; `extract` counts such files in a warning.

On 8 cores, extracting community from 17.0 to 18.0 (about 50,000 commits) takes 282 s with an
empty cache and 76 s with a warm one (2026-10-20).

## Contributing

Setup, the gates and the release process are in [CONTRIBUTING.md](CONTRIBUTING.md).
