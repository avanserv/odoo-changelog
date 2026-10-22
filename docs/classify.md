# Classify

After writing events, `extract` classifies each commit that every extractor has processed. Each
event, and the commit as a whole, gets a `category`, a `breaking` flag and the `rule_id` that
decided them. `functional` is what a user of Odoo sees. `technical` is what a developer of
custom modules sees. `breaking` is set independently of the category: it marks a change that
custom code written against the old side may fail on.

Classification reads the stored events and the commit's `tag`, never the blobs, so its result
depends on those rows alone. A wrong class gets fixed by changing a rule: bumping
`classify::VERSION` in `crates/extract/src/classify.rs` makes the next `extract` reclassify every
commit without parsing anything again. A commit whose events change, through a new extractor or an
extractor version bump, is reclassified the same way. `commits.classified` holds the version that
classified the commit, and is NULL while it waits.

## Breaking

The first of these that matches decides, and its rule id is the event's `rule_id`:

| `rule_id` | When |
| --- | --- |
| `arch-node-removed` | A `view_node` or `owl_node` is removed: custom xpaths target it |
| `removed` | Any other symbol is removed, except a `data_function` or `delete`, which declare nothing another file can name |
| `renamed` | The symbol is renamed |
| `moved-module` | The symbol moved to another module: a custom module may not depend on it |
| `method-signature` | The `signature` of a `method` or `function` differs |

## Category

When no breaking rule matches, the category rule gives the `rule_id`. A breaking event still
takes its category from this table. Kinds are listed on each [extractor's page](extract.md#extractors).

| `rule_id` | Kind and condition | Category |
| --- | --- | --- |
| `module` | `module` added, removed or renamed | functional |
| `module-manifest` | `module` changed or moved | technical |
| `menu`, `action` | `menu`, `action` (report actions included) | functional |
| `view` | `view` added or removed, or its `arch` differs | functional |
| `view-meta` | any other `view` event, such as a `priority` or `inherit_id` change | technical |
| `arch-node-visible` | `view_node` of a `field`, `button`, `page`, `filter`, `notebook`, `separator` or `label` | functional |
| `arch-node` | any other `view_node`, such as a `div` named as an xpath anchor | technical |
| `access-rights` | `group`, `rule`, `access` | technical |
| `data` | `record`, `data_function`, `delete` | technical |
| `model-with-views` | `model` added in a commit that adds a `view` whose `model` is it | functional |
| `model` | any other `model` event | technical |
| `field-label` | `field` whose `string`, `help`, `selection` or `selection_add` argument differs | functional |
| `field` | any other `field` event, compute, store and index changes included | technical |
| `selection` | `selection`: its values and labels are shown | functional |
| `method` | `method` | technical |
| `python` | `class`, `function`, `route`, `constraint`, `constant` | technical |
| `js`, `owl`, `style` | `js_*`, `owl_*`, `scss_*` and `css_property` kinds | technical |
| `other` | a kind no rule names | technical |

## Commits

A commit takes the category most of its events have (`commit-majority`). On a tie, its tag
decides (`commit-tag`): `[ADD]`, `[IMP]`, `[FIX]` and `[REM]` are functional, any other tag is
technical. A tie without a tag is technical (`commit-default`). A commit without events keeps a
NULL `category` with `commit-no-events`. A commit is `breaking` when any of its events is.

```sql
SELECT c.sha, c.subject, s.qualname, e.op, e.rule_id FROM events e
  JOIN commits c ON c.repo_id = e.repo_id AND c.sha = e.sha
  JOIN symbols s ON s.id = e.symbol_id
 WHERE c.version = '18.0' AND e.breaking = 1;
```
