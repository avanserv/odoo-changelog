from odoo.upgrade import util


def migrate(cr, version):
    cr.execute("SELECT id FROM ir_ui_view WHERE key = 'shop.x'")
    [view_id] = cr.fetchone()
    util.remove_view(cr, view_id=view_id)
    util.rename_field(cr, "shop.order", get_old(), "partner_id")
    renames = dict(cr.fetchall())
    util.rename_xmlid(cr, *renames.popitem())
    util.rename_xmlid(cr, "shop.a", "shop.b", **{"on_collision": "merge"})
