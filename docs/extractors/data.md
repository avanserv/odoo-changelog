# Data extractor

The `data` extractor reads every `.xml` file of a module outside `static/`, wherever it sits,
since Odoo loads any file a manifest lists, and every `.csv` file there whose name up to the first
`-` is a model, as Odoo derives it: `res.country.state.csv`, `ir.model.access.csv`, a chart
template's `account.tax-be.csv`. A file whose root is not `<odoo>`, `<openerp>` or `<data>`, such
as an e-invoice sample, gives no symbol, and neither does a CSV whose first row is one cell, such
as the `|`-separated codes an install hook reads. Symbols are named by xmlid, and an id written without a module gets the file's
module, as Odoo's `make_xml_id` does: in qualnames, and in the references `inherit_id`, `parent`,
`action`, `groups`, `model_id` and `group_id`.

| Kind | Declared by | Example |
| --- | --- | --- |
| `view` | an `ir.ui.view` record, a `<template>` | `sale.view_order_form@sale` |
| `view_node` | a named node of a view's arch | `sale.view_order_form#field[@name='partner_id']@sale` |
| `menu` | a `<menuitem>`, an `ir.ui.menu` record | `sale.menu_sale_quotations@sale` |
| `action` | an `ir.actions.*` record, `<report>`, `<act_window>` | `sale.action_orders@sale` |
| `group`, `rule`, `access` | `res.groups`, `ir.rule`, an access record or CSV row | `sale.access_sale_order@sale` |
| `record` | any other record or CSV row, `<asset>` | `base.state_au_1@base` |
| `data_function` | `<function>`, `[2]` from its second call in the file | `res.partner#write[2]@sale` |
| `delete` | `<delete>`, by id or by search | `account.tax#search@l10n_fr` |

The `@<module>` suffix is the module whose file declares the symbol, so an override of another
module's record has a history of its own. Declarations of one id in a file merge, the later
fields winning, as Odoo updates the record. `declared` says which tag declared the symbol, so a
`<report>` that became an `ir.actions.report` record is one `changed` event.

## Attributes

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

A CSV row is a record named by its `id` column, with `declared: "csv"` and the file's model as
`record_model`. It keeps the same attributes in clear as an XML record and hashes its non-empty
cells into `values` as the XML fields setting them would hash, a `country_id:id` cell as a `ref`,
so a record of text and single-reference fields moving between the two formats is no change; a
`tag_ids/id` cell is hashed as a `ref` of its text too, which XML would write as an `eval`. Later
rows of an id add to it,
and a row without an id continues the one above, as a chart template's repartition lines do. The
reader sees the file's bytes alone, so the header tells the kind: permission columns, or
`operation` beside a bare `model_id`, give `access` rows with every cell in clear, and
`domain_force` gives `rule` rows. Chart template ids are named as the file writes them, without
the company prefix Odoo adds when it loads them.

A file with a syntax error keeps the declarations closed before it, and leaves a warning.
