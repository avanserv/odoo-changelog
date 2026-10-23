# Upgrade

`upgrade` reads the scripts of a clone of Odoo's `odoo/upgrade` repository and records the
renames, moves, merges and removals their `util` calls declare, each with the version its folder
upgrades to. These events say what changed, but not in which commit: [matching](#matching) finds
the commit among the renames `extract` detected.

```toml
[upgrade]
path = "/path/to/upgrade"
```

The command reads `<remote>/master` through git, never the working tree, and runs no fetch. A
rerun on the same commit reads no script and only matches again; any other commit replaces every
stored event. It prints the events and partial events per function. Without an `[upgrade]` path
it fails.

## Scripts

A script is a `.py` file in `migrations/<module>/<folder>/`, where the folder is the series
followed by the module's own version:

| Folder | `version` |
| --- | --- |
| `17.0.1.2`, `16.0.13.0.1` | `17.0`, `16.0` |
| `saas~17.2.1.1` | `saas-17.2` |
| `saas~17.5.1.0` (from 16 on) | `18.0`: no saas-X.5 ships, master's (17, 5) becomes 18.0 |
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

## Matching

`upgrade` and `extract` both end by matching every stored upgrade event against every detected
rename (`op = 'renamed'` in `events`), from scratch, so the result depends on the stored rows and
not on which command ran last. An event that renames or merges a field, model or xmlid, is not
partial and has a version matches each rename whose commit ships in that version and whose
names, `@<module>` suffix and `[<Class>]` stripped, are the event's:

| `kind` | Detected kinds | Names compared |
| --- | --- | --- |
| model | `model` | `old`, `new` |
| field | `field` | `model`, `old`, `new`: `model` may be either side's model, since a script may name it before or after renaming it |
| xmlid | `view`, `menu`, `action`, `group`, `rule`, `access`, `record` | `old`, `new` |

A model's rename matches in every module that declares the model. A matched rename gets
confidence 1 and `upgrade` as its last signal; `upgrade_matches` links it to the upgrade event
and keeps the detector's `detected_confidence`, which the rename gets back when a later match no
longer holds. `repo_id` and `sha` on an upgrade event name the earliest committed of the commits
it matched, and stay NULL when it matched none.

[`stats --validate`](stats.md#validate) turns the matches into the detector's recall and
precision, and its page gives the query for the events that matched nothing.
