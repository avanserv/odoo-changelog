# Upgrade

`upgrade` reads the scripts of a clone of Odoo's `odoo/upgrade` repository and records the
renames, moves, merges and removals their `util` calls declare, each with the version its folder
upgrades to. These events say what changed, but not in which commit.

```toml
[upgrade]
path = "/path/to/upgrade"
```

The command reads `<remote>/master` through git, never the working tree, and runs no fetch. A
rerun on the same commit is a no-op; any other commit replaces every stored event. It prints the
events and partial events per function. Without an `[upgrade]` path it fails.

## Scripts

A script is a `.py` file in `migrations/<module>/<folder>/`, where the folder is the series
followed by the module's own version:

| Folder | `version` |
| --- | --- |
| `17.0.1.2`, `16.0.13.0.1` | `17.0`, `16.0` |
| `saas~17.2.1.1` | `saas-17.2` |
| `9.saas~11.1.3` (before 11.0) | `saas-11` |
| `0.0.0` (runs on every upgrade) | NULL |

`phase` is `pre`, `post`, `end` or `init` from a `<phase>-*.py` file name, or NULL for a helper
module. Every other file is skipped: `tests/` and `test_*.py`, `__pycache__/`, module folders
starting with `_`, non-Python files, and symlinks, whether to a script or a whole folder. A
symlink runs a script whose own folder already records its events, and recording them again at a
later version would show a change that version did not make. Calls through
`util.import_script` are skipped for the same reason.

## Events

`upgrade_events` holds one row per event, keyed by `(path, line, col, seq)`, with `module` and
`folder` from the path.

| Call | `kind` | `op` | Fields |
| --- | --- | --- | --- |
| `rename_field` | field | renamed | `model`, `old`, `new` |
| `rename_model`, `rename_xmlid`, `rename_module` | model, xmlid, module | renamed | `old`, `new` |
| `merge_module`, `merge_model` | module, model | merged | `old`, `new` |
| `move_field_to_module` | field | moved | `model`, `old` = `new`, `old_module`, `new_module` |
| `move_model` | model | moved | `old` = `new`, `old_module`, `new_module` |
| `remove_field` | field | removed | `model`, `old` |
| `remove_model`, `delete_model` | model | removed | `old` |
| `remove_view` | view | removed | `old`, from `xml_id` or `key` |
| `remove_record` | xmlid | removed | `old` |
| `remove_module` | module | removed | `old` |

`call` is the upgrade-util function, `delete_model` kept apart from `remove_model`. A call is
recognised as `util.<function>(...)`, whatever `util` was imported from or as, or as a name
imported from a `util` module, aliased or not.

## Resolving arguments

Arguments are evaluated statically, without running the script:

- string literals, `+`, `%`, `.format()`, f-strings, `.replace()`, `.split()` and indexing;
- `*util.expand_braces("shop.{a,b}")` and any name assigned to it, such as `eb`;
- names assigned once in the function or the module;
- the `for` loops around the call, over literal lists, tuples, sets, dicts and their
  `.items()`, `util.splitlines("""...""")`, `zip`, `enumerate` and `sorted`. Each run of the
  loops gives one event, numbered by `seq`. A loop whose variables the call does not use adds
  no runs.

Conditions around a call (`if util.module_installed(...)`, `util.version_gte(...)`) are not
evaluated: the event is recorded either way.

A field whose argument does not resolve is NULL, and its event is marked `partial` and keeps the
call's text in `source`. That covers a database query result, a helper's parameter, a list
changed in place, a `view_id`, and a name that the loop body, a nested loop, a `with`, `except`
or comprehension binds again.

`upgrade_runs` holds the commit read and the parser version. A parser change bumps
`odoo_changelog_upgrade::VERSION`, and the next run reads every script again.
