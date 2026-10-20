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
```

`walk` is incremental: it resumes each branch from the last recorded head and fails if a
branch was rewritten, or if a fork moved since the last walk (a new `[fork_overrides]` entry,
a changed `first_version`, a deepened clone), since either renumbers rows a resume does not
revisit. Rebuild the database in both cases. Two runs on the same refs produce identical
databases.

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

## Contributing

Setup, the gates and the release process are in [CONTRIBUTING.md](CONTRIBUTING.md).
