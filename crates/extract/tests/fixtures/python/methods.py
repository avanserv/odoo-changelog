from odoo import api, fields, models


class Order(models.Model):
    _name = 'demo.order'
    _description = 'Order'

    amount = fields.Float()

    @api.depends('line_ids', 'line_ids.price')
    def _compute_amount(self):
        """Sum the lines."""
        for order in self:
            order.amount = sum(order.line_ids.mapped('price'))

    @api.model
    def _default_name(self):
        return 'New'

    @api.onchange('partner_id')
    def _onchange_partner(self):
        self.amount = 0

    @api.constrains('amount')
    def _check_amount(self):
        if any(order.amount < 0 for order in self):
            raise ValueError('negative')

    @property
    def label(self):
        return self._label

    @label.setter
    def label(self, value):
        self._label = value

    def signature(self, a, b=None, *args, c, d: int = 3, **kwargs):
        return a

    def separators(self, x, /, y, *, z):
        return x + y + z

    def annotated(self, count: int) -> list[str]:
        return [str(i) for i in range(count)]

    async def fetch(self):
        return await self.env['demo.client'].get()
