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

`walk` is incremental and fails when a branch was rewritten or a fork moved since the last walk:
[rebuild the database](docs/walk.md#incremental-walks) then. Two runs on the same refs produce
identical databases.

## Documentation

- [Walk](docs/walk.md): incremental walks and how commits get their version.
- [Extract](docs/extract.md): how changed files become symbol events, and the
  [`python`](docs/extractors/python.md), [`manifest`](docs/extractors/manifest.md),
  [`data`](docs/extractors/data.md), [`js`](docs/extractors/js.md),
  [`owl`](docs/extractors/owl.md) and [`scss`](docs/extractors/scss.md) extractors.
- [Classify](docs/classify.md): which changes are functional, technical or breaking, and the
  rule that decided.

## Contributing

Setup, the gates and the release process are in [CONTRIBUTING.md](CONTRIBUTING.md).
