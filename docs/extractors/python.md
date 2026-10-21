# Python extractor

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

## Attributes

Attributes are normalized so that formatting is no change: literal values are decoded, other
expressions are kept as canonical text, and a def's `body` is a hash of its tokens without
comments, docstring or local names. Each symbol also records the line and byte span of its name,
which change detection ignores. A file that does not fully parse still gives the symbols around
the error. Such a file, Python 2 syntax, a class whose model name cannot be read and a dropped
duplicate each leave a warning.
