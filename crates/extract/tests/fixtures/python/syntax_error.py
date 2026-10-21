from odoo import fields, models


class Broken(models.Model):
    _name = 'demo.broken'

    before = fields.Char()

    def first(self):
        return 1

    def middle(self):
        return self.write({'a': (1,
}

    def last(self):
        return 3


class After(models.Model):
    _name = 'demo.after'

    after = fields.Char()
