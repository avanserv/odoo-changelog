# JavaScript extractor

The `js` extractor reads the `.js` files under a module's `static/src`, test directories and
`.test.js` files aside, and every `.js` file under `static/tests` but tours, `*.test.js` and
`*_tests.js`, for the helpers tests import. Only modules and exports count in `static/tests`,
since its registries and patches never ship; `static/lib` is not read. It parses with
tree-sitter and reads only the code that runs when the file loads, an `odoo.define` body and a
mail model's factory included, so nothing inside another function counts.

| Kind | Declared by | Example |
| --- | --- | --- |
| `js_module` | an ES module, an `odoo.define`, an `alias=` | `@web/core/registry`, `web.Widget` |
| `js_export` | an ES export, an `odoo.define` return (`default`, or each key of an object) | `@web/core/registry#registry` |
| `js_component` | an OWL component class | `@web/views/fields/many2one/many2one_field#Many2OneField` |
| `js_widget` | a legacy `Parent.extend({...})` class | `web.Widget#Widget` |
| `js_registry` | a registry's `add(key, ...)` | `fields:many2one@web` |
| `js_patch` | `patch(target, {...})`, `target.include({...})`, a mail model patch | `web.FormController@barcodes` |
| `js_mail_model` | mail's `registerModel`, `registerNewModel` (14.0 to 16.0) | `Thread@mail`, `mail.thread@mail` |

A module is named the way Odoo defines it: `@<addon>/<path under static/src>` for an ES module
(a file with an `@odoo-module` header, an import or an export), or
`@<addon>/../tests/<path under static/tests>` under `static/tests`, the given name for an
`odoo.define`, and a second name for an `alias=`. References read `<module>#<member>`: imports,
requires and the file's own declarations resolve to the module that declares them, a relative
import against the file's path. A registry entry is named by its category, chained categories
joined with `>`, or by the legacy registry it is added to (`web.core#action_registry`), then
its key. A mail model is named as it is registered. The calls of a file that patch one, by
`registerPatch`, `registerInstancePatchModel`, `registerClassPatchModel`,
`registerFieldPatchModel` or `registerIdentifyingFieldsPatch`, make one patch of `mail:<model>`.
Entries, patches and mail models fill slots every addon shares, so only `js_registry`, `js_patch`
and `js_mail_model` qualnames end with `@<addon>`, the addon that declares them: read it after the
last `@`, since the module names in the rest start with `@` too. A second `patch()` of one target
in a file reads `<target>[2]`, and a second export of one declaration points at the first with
`of`.

## Attributes

Attributes hold a module's imports or a define's `requires`, an export's kind and value, a
class's `extends`, `members` and `static`, a component's `template`, `props` and `components`,
and an entry's `value`, `sequence`, `force` and, for a service, `dependencies`. A mail model
keeps its call as `declared`, a mail patch its calls, and both their sections by the 16.0 names:
`fields`, `recordMethods`, `modelMethods`, `lifecycleHooks`, `onChanges` and the like. A 14.0 or
15.0 class gives its instance methods as `recordMethods` and its statics as `modelMethods`; a
14.0 or 15.0 patch call fills the section it patches and adds its free-text patch name to
`names`. A field reads as its helper's `type`, such as `many` or `one2many`, and `args`.
Functions, methods and classes are a hash of their tokens without layout, comments, quote style,
trailing commas, semicolons, their own name or local names: a name bound anywhere inside reads by
the order it is first bound, so a consistent rename of a local hashes alike. A shorthand `{ a }`
keeps `a` as the key it also names. A file that does not fully parse still gives the symbols
around the error and leaves a warning.
