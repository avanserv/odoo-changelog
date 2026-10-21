# Manifest extractor

The `manifest` extractor gives each `__manifest__.py` and `__openerp__.py` one `module` symbol
named after its directory, the name `depends` lists, such as `sale`. The file is read as a Python
literal and never run, as Odoo reads it. Every key is kept as written, without Odoo's defaults,
and values follow the [literal rules of the `python` extractor](python.md#attributes): lists keep
their order, so asset entries keep their order within a bundle and a directive such as
`('include', 'web._assets_helpers')` is an array; dict keys are sorted; anything JSON cannot hold
as a literal, a set among them, is kept as `{"expr": "<source>"}`. A repeated key keeps its last
value, as in Python. A manifest with a syntax error keeps the keys that parse and leaves a
warning.
