# JavaScript extractor

The `js` extractor reads the `.js` files under a module's `static/src`, test directories and
`.test.js` files aside; `static/tests` and `static/lib` are not read. It parses with tree-sitter
and reads only the code that runs when the file loads, an `odoo.define` body included, so
nothing inside another function counts.

| Kind | Declared by | Example |
| --- | --- | --- |
| `js_module` | an ES module, an `odoo.define`, an `alias=` | `@web/core/registry`, `web.Widget` |
| `js_export` | an ES export, an `odoo.define` return (`default`, or each key of an object) | `@web/core/registry#registry` |
| `js_component` | an OWL component class | `@web/views/fields/many2one/many2one_field#Many2OneField` |
| `js_widget` | a legacy `Parent.extend({...})` class | `web.Widget#Widget` |
| `js_registry` | a registry's `add(key, ...)` | `fields:many2one@web` |
| `js_patch` | `patch(target, {...})`, `target.include({...})` | `web.FormController@barcodes` |

A module is named the way Odoo defines it: `@<addon>/<path under static/src>` for an ES module
(a file with an `@odoo-module` header, an import or an export), the given name for an
`odoo.define`, and a second name for an `alias=`. References read `<module>#<member>`: imports,
requires and the file's own declarations resolve to the module that declares them, a relative
import against the file's path. A registry entry is named by its category, chained categories
joined with `>`, or by the legacy registry it is added to (`web.core#action_registry`), then
its key. Entries and patches fill slots every addon shares, so only `js_registry` and `js_patch`
qualnames end with `@<addon>`, the addon that declares them: read it after the last `@`, since
the module names in the rest start with `@` too. A second patch of one target in a file reads
`<target>[2]`, and a second export of one declaration points at the first with `of`.

## Attributes

Attributes hold a module's imports or a define's `requires`, an export's kind and value, a
class's `extends`, `members` and `static`, a component's `template`, `props` and `components`,
and an entry's `value`, `sequence`, `force` and, for a service, `dependencies`. Functions,
methods and classes are a hash of their tokens without layout, comments, quote style, trailing
commas, semicolons or their own name. A file that does not fully parse still gives the symbols
around the error and leaves a warning.
