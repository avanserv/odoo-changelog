from odoo import fields, models


class Pond(models.Model):
    _name = 'demo.pond'
    depth = fields.Float()

    def drain(self):
        return 0

    else:
        pass
