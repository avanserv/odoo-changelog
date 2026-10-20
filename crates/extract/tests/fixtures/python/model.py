from odoo import fields, models


class Library(models.Model):
    _name = 'demo.library'
    _inherit = ['mail.thread', 'mail.activity.mixin']
    _inherits = {'res.partner': 'partner_id'}
    _table = 'demo_library_table'
    _order = 'sequence, name desc'
    _rec_name = 'title'
    _description = "Library"
    _x = 42
    __slots__ = ()

    title = fields.Char()


class LibraryExtension(models.Model):
    _inherit = 'demo.library'

    shelf = fields.Integer()


class SameName(models.Model):
    _name = 'demo.shelf'
    _inherit = 'demo.shelf'


class Wizard(models.TransientModel):
    _name = 'demo.wizard'
    _description = 'Wizard'


class Mixin(models.AbstractModel):
    _name = 'demo.mixin'
    _description = 'Mixin'


class Several(models.Model):
    _inherit = ['demo.mixin', 'demo.wizard']

    ignored = fields.Char()


class Helper:
    """Neither _name nor _inherit: a plain class."""


class Base:
    _name = None
    _inherit = None


# Bibliothèque, 5 € par mois: multi-byte text before a symbol.
class Chained(models.AbstractModel):
    _name = _description = 'demo.chained'
    _order = _rec_name = 'name'

    name = label = fields.Char()
