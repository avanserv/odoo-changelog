from odoo import fields, models

STATES = [
    ('draft', "Draft"),
    ('done', "Done"),
]

STATES = [
    ('draft', "Quotation"),
    ('sale', "Sales Order"),
]


class Order(models.Model):
    _name = 'demo.order'
    _description = 'Order'

    state = fields.Selection(selection=STATES, default='draft')
    inline = fields.Selection([('a', 'Alpha'), ("b", "Beta")])
    priority = fields.Selection([(0, 'Low'), (1, 'High')])
    method = fields.Selection('_selection_method')


class OrderExtension(models.Model):
    _inherit = 'demo.order'

    state = fields.Selection(
        selection_add=[('sent', 'Sent'), ('cancel',)],
        ondelete={'sent': 'set default'},
    )

    def _selection_method(self):
        return [('x', 'X')]
