# OWL extractor

The `owl` extractor reads the `.xml` files under a module's `static/src`, test directories aside;
the few `<templates>` files 12.0 to 14.0 keep under `views/` are not read. Names are global, so
every qualname ends with `@<addon>`, the addon that declares or extends the symbol.

| Kind | Declared by | Example |
| --- | --- | --- |
| `owl_template` | a `t-name`, a primary `t-inherit`, a legacy `t-name` with `t-extend` | `web.FormView@web` |
| `owl_extension` | an extension-mode `t-inherit`, a bare `t-extend`; `[2]` from the second of one parent | `web.FormView@mail` |
| `owl_node` | a named node of a template, or one its specs add | `web.FormView#div[@t-ref='root']@web` |

A template records its parent as written in `inherit`, its `mode` (`primary` when unwritten, as
Odoo defaults), `declared: "t-extend"` for the legacy forms, its `owl` attribute, an `arch` hash
and its `specs`: a `t-inherit`'s like [a view's](data.md#attributes), a `t-extend`'s as `jquery`
and `operation`. A node is any element with a `name`, `id`, `t-name`, `t-ref` or `t-set-slot`, a
template's own element included, and an extension's nodes go under its parent, so `web.FormView#`
lists every addon's nodes of that template. Extensions of one parent from several files of an
addon share their symbols, as patches do: deleting one of the files reads as a removal. A template
file with a syntax error keeps the templates closed before it and leaves a warning.
