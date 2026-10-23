from odoo.upgrade.util import remove_record, remove_view as drop_view
from odoo.addons.base.maintenance.migrations import util as u
import odoo.upgrade.util as uu


def migrate(cr, version):
    remove_record(cr, "shop.rule_cart")
    drop_view(cr, "shop.view_cart_search")
    u.remove_field(cr, "shop.cart", "token")
    uu.remove_model(cr, "shop.token")
    # Not util's: a local function of the same name.
    helper.remove_field(cr, "shop.cart", "other")
