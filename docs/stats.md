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
| `reachable` | Those matched, or whose old name one commit and extractor of that version removed or renamed away while adding or renaming to the new name, a field on the event's model on at least one side |
| `reach` | `matched` / `reachable`: recall over the renames the detector could find |
| `detected` | Detected renames of that kind, an xmlid being any kind the [data extractor](extractors/data.md) names by xmlid but `view_node` |
| `confirmed` | Those an upgrade event matches, or that keep a field's name and follow a matched model rename |
| `precision` | `confirmed` / `detected` |

Merges count toward `confirmed` but not toward `upgrade`. Precision is a lower bound: an upgrade
script declares only what a database needs migrated, so a field renamed in the version that
added it, or a record renamed without stored data, has no event to match.

The rest of `upgrade` is out of the detector's reach: a symbol removed in one commit and added in
a later one, and a name no extractor declares, such as a generated `ir.model.fields` xmlid or a
record of an unwalked repository. Odoo's upgrade scripts also leave undeclared the xmlids a module
merge renames, so a record or view moved to another module unchanged is a correct rename that
`confirmed` does not count.

The upgrade events that matched nothing are the recall misses:

```sql
SELECT u.version, u.call, u.model, u.old, u.new FROM upgrade_events u
 WHERE u.op = 'renamed' AND u.kind IN ('field', 'model', 'xmlid')
   AND NOT u.partial AND u.version IS NOT NULL AND u.sha IS NULL
 ORDER BY u.version, u.path, u.line;
```
