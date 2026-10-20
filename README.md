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

`extract` diffs each walked commit against its parent and turns every changed file into
symbol events: added, removed, changed (its attributes differ) or moved (a renamed file, same
attributes). Merge commits are skipped, so each event belongs to the commit that made the
change. Renames are paired as git pairs them by default (50% similarity), whatever the clone's
config says. A symbol's id is `blake3(kind, 0x00, qualname)`. Each blob is parsed once per
extractor version: the result is cached under the blob id, the extractor's name and its
version, and bumping the version re-extracts that extractor's commits only. `--from` and `--to`
bound the `version` of the commits extracted, and a later run picks up the rest.

`extract` records nothing until an extractor is registered in `Registry::builtin()`.

## Contributing

Setup, the gates and the release process are in [CONTRIBUTING.md](CONTRIBUTING.md).
