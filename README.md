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
that declares them, so a base field and an override in another module have separate histories.
When several classes of one file declare the same model, such as the `res.users` classes in
`base`, the `model` symbol of each later class reads `<model>[<Class>]`, and so does a member of
it whose plain name an earlier class took. `class` covers classes that declare no model;
`function` covers module-level functions and the methods of such classes, as
`<Class>.<method>`, with `test*` methods left out unless routed. A method with an `http.route`
decorator also gives a `route` symbol of the same name. A `constraint` is an `_sql_constraints`
entry or, from 19.0, a `models.Constraint`, `UniqueIndex` or `Index`.

Attributes are normalized so that formatting is no change: literal values are decoded, other
expressions are kept as canonical text, and a def's `body` is a hash of its tokens without
comments, docstring or local names. Each symbol also records the line and byte span of its name,
which change detection ignores. A file that does not fully parse still gives the symbols around
the error. Such a file, Python 2 syntax, a class whose model name cannot be read and a dropped
duplicate each leave a warning on the file's table, and `extract` reports how many file sides
(the old or new version of a changed file) carry one.

### Manifest symbols

The `manifest` extractor gives each `__manifest__.py` and `__openerp__.py` one `module` symbol
named after its directory, the name `depends` lists, such as `sale`. The file is read as a Python
literal and never run, as Odoo reads it. Every key is kept as written, without Odoo's defaults,
and values follow the literal rules above: lists keep their order, so asset entries keep their
order within a bundle and a directive such as `('include', 'web._assets_helpers')` is an array;
dict keys are sorted; anything JSON cannot hold as a literal, a set among them, is kept as
`{"expr": "<source>"}`. A repeated key keeps its last value, as in Python. A manifest with a
syntax error keeps the keys that parse and leaves a warning.

### Data files

The `data` extractor reads every `.xml` file of a module outside `static/`, wherever it sits,
since Odoo loads any file a manifest lists, and the `ir.model.access.csv` and `ir.access.csv`
files. A file whose root is not `<odoo>`, `<openerp>` or `<data>`, such as an e-invoice sample,
gives no symbol. Symbols are named by xmlid, and an id written without a module gets the file's
module, as Odoo's `make_xml_id` does: in qualnames, and in the references `inherit_id`, `parent`,
`action`, `groups`, `model_id` and `group_id`.

| Kind | Declared by | Example |
| --- | --- | --- |
| `view` | an `ir.ui.view` record, a `<template>` | `sale.view_order_form@sale` |
| `view_node` | a named node of a view's arch | `sale.view_order_form#field[@name='partner_id']@sale` |
| `menu` | a `<menuitem>`, an `ir.ui.menu` record | `sale.menu_sale_quotations@sale` |
| `action` | an `ir.actions.*` record, `<report>`, `<act_window>` | `sale.action_orders@sale` |
| `group`, `rule`, `access` | `res.groups`, `ir.rule`, an access record or CSV row | `sale.access_sale_order@sale` |
| `record` | any other record, `<asset>` | `sale.mail_template_sale_confirmation@sale` |
| `data_function` | `<function>`, `[2]` from its second call in the file | `res.partner#write[2]@sale` |
| `delete` | `<delete>`, by id or by search | `account.tax#search@l10n_fr` |

The `@<module>` suffix is the module whose file declares the symbol, so an override of another
module's record has a history of its own. Declarations of one id in a file merge, the later
fields winning, as Odoo updates the record. `declared` says which tag declared the symbol, so a
`<report>` that became an `ir.actions.report` record is one `changed` event.

A view records its `model`, `type`, `inherit_id`, `priority`, `mode`, an `arch` hash and a `values`
hash of its other fields. Every node of its arch with a `name`, `id` or `t-name` attribute is a
`view_node`, keyed like `button[@id='action_confirm']`, with a `count` (above 1, a shorthand spec
matches the first) and the `path` of its named ancestors. An inheriting view lists its `specs` in
the order Odoo applies them: an `xpath` gives its `expr`, any other spec a `target` keyed like a
node, so that `field[@name='partner_id']` joins the node it changes; the nodes its specs add are its
own. Other records keep `name`, `model`, `res_model` and a `model_id` reference in clear, and hash
everything else into `values`. Arch and markup hashes ignore whitespace runs, comments, attribute
order and how a character is escaped; plain field text is hashed as written, since Odoo stores it
verbatim. Menuitems and access rows keep every attribute or cell in clear.

A file with a syntax error keeps the declarations closed before it, and leaves a warning.

On 8 cores, extracting community from 17.0 to 18.0 (about 50,000 commits) with the three
extractors takes 130 s with an empty cache and 42 s with a warm one (2026-10-20).

## Contributing

Setup, the gates and the release process are in [CONTRIBUTING.md](CONTRIBUTING.md).
