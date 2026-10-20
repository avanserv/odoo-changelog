from odoo import models


class Tag(models.Model):
    _name = 'demo.tag'
    _description = 'Tag'

    _sql_constraints = [
        ('name_uniq', 'unique (name)', "Tag names must be unique."),
        ('color_check', 'CHECK(color >= 0)', 'Colors are positive.'),
    ]


class Label(models.Model):
    _name = 'demo.label'
    _description = 'Label'

    _name_uniq = models.Constraint('unique (name)', "Label names must be unique.")
    _code_idx = models.UniqueIndex('(code) WHERE active', message='Codes must be unique.')
    _date_idx = models.Index('(date DESC)')


class Badge(models.Model):
    _name = 'demo.badge'
    _description = 'Badge'

    _sql_constraints = BASE_CONSTRAINTS + [('extra', 'CHECK(true)', 'x')]
