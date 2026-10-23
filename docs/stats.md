# Stats

`stats` prints the number of commits and merge commits per repo and version, `?` standing for
commits no version claims yet.

## Validate

`stats --validate` measures the rename detector against Odoo's upgrade scripts, as
[Matching](upgrade.md#matching) pairs them. It fails until `upgrade` has stored events. One row
per walked version, oldest first, and kind (`field`, `model`, `xmlid`), then an `all` row per kind:

| Column | Counts |
| --- | --- |
| `upgrade` | Upgrade events renaming one of that kind, not partial |
| `matched` | Those that match a detected rename |
| `recall` | `matched` / `upgrade` |
| `detected` | Detected renames of that kind, an xmlid being any kind the [data extractor](extractors/data.md) names by xmlid but `view_node` |
| `confirmed` | Those an upgrade event matches, or that keep a field's name and follow a matched model rename |
| `precision` | `confirmed` / `detected` |

Merges count toward `confirmed` but not toward `upgrade`. Precision is a lower bound: an upgrade
script declares only what a database needs migrated, so a field renamed in the version that
added it, or a record renamed without stored data, has no event to match.

The upgrade events that matched nothing are the recall misses:

```sql
SELECT u.version, u.call, u.model, u.old, u.new FROM upgrade_events u
 WHERE u.op = 'renamed' AND u.kind IN ('field', 'model', 'xmlid')
   AND NOT u.partial AND u.version IS NOT NULL AND u.sha IS NULL
 ORDER BY u.version, u.path, u.line;
```
