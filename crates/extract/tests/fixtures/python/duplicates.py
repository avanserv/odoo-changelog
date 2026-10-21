from odoo import api, fields, models


class ResUsers(models.Model):
    _inherit = 'res.users'

    nickname = fields.Char()

    @api.model_create_multi
    def create(self, vals_list):
        return super().create(vals_list)


class Users(models.Model):
    _inherit = 'res.users'

    nickname = fields.Char(required=True)

    @api.model_create_multi
    def create(self, vals_list):
        return super().create(vals_list)


class Users(models.Model):
    _inherit = 'res.users'

    def create(self, vals_list):
        return super().create(vals_list)
