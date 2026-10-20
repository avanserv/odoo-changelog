# Contributing

## Setup

You need rustup, `make`, `git` and [prek](https://github.com/j178/prek) (or pre-commit). rustup
installs the toolchain `rust-toolchain.toml` pins on the first cargo call.

```bash
make tools    # cargo-deny and cargo-machete, at the versions CI uses
make hooks    # installs the git hooks and runs them over the whole tree
cp config/odoo-changelog.example.toml config/odoo-changelog.toml
git config commit.template .gitmessage
```

`config/odoo-changelog.toml` is ignored: it holds paths on your machine.

## The gates

`make check` runs everything CI runs but the git hooks: formatting (rustfmt), lints with warnings
as errors (clippy), unused dependencies (cargo-machete), advisories, licenses and sources
(cargo-deny, configured in `deny.toml`), and the tests. `make help` lists the individual targets.

The git hooks add what only makes sense there: secrets (gitleaks), spelling (typos), Markdown
(markdownlint) and the workflow audits (actionlint, zizmor, schema and timeout checks).

## How the workspace is laid out

- `core` holds the types and rules every other crate shares, and depends on no other crate here.
- `git` reads the Odoo clones and never touches SQLite; `store` writes SQLite from plain data and
  never sees a git type.
- `extract` turns one blob into symbols and never sees git or SQLite; an extractor's output
  depends on the blob's bytes alone, and changing it bumps `version()`.
- `cli` orchestrates and holds no logic another command would need.

A dependency is declared in the same change as the code that uses it, never ahead of it:
cargo-machete fails otherwise.

## Determinism

Two runs on the same refs must produce identical databases, and an incremental walk must equal a
full rebuild. `crates/cli/tests/determinism.rs` builds a small Odoo-shaped repository with the git
CLI and checks both. Extend its fixture when a change adds a history shape it does not cover.

An extractor's expected outputs live beside its hand-written fixtures, such as
`crates/extract/tests/fixtures/python/model.json` for `model.py`. After changing what it returns,
regenerate them with `ODOO_CHANGELOG_BLESS=1 cargo test -p odoo-changelog-extract`, review the
diff, and bump the extractor's `version()`.

Tests never read the real Odoo clones. A test that needs them is `#[ignore]` and says so. Nothing
from the private `odoo/upgrade` repository is copied into a fixture: write fixtures by hand.

## Commits and pull requests

Commits follow [Conventional Commits](https://www.conventionalcommits.org):
`<type>(<scope>): Summary`, with a crate or command as the scope. `.gitmessage` has the details.

Pull requests are squash-merged, so **the pull request title is the commit**. CI checks that it is
a conventional one.

- `feat` bumps the minor version and `fix` the patch; use them only for a change a user of the
  binary or its database can see.
- `feat!`, or a `BREAKING CHANGE:` footer, is a breaking change: a minor bump while the version is
  below 1.0, a major one after.
- `perf` and `revert` bump the patch too: whatever appears in the changelog is released.
- Everything else (`docs`, `chore`, `refactor`, `test`, `ci`, `build`, `style`) bumps nothing.

## Releases

Nobody edits a version. [Release Please](https://github.com/googleapis/release-please) reads the
commits on `main` and keeps a release pull request open with the next version and the changelog.

Before merging one, wait for the Release run that last updated it to finish and for CI to pass on
the branch head: Release Please does not know about `Cargo.lock`, so that run pushes a
`chore: sync Cargo.lock with the release version` commit whenever the version changed.

Merging the release pull request tags the release, and the Release workflow attaches a Linux
x86_64 archive of the binary with its SHA-256.
