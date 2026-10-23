from odoo.upgrade import util

eb = util.expand_braces


def migrate(cr, version):
    util.rename_xmlid(cr, *eb("shop.{cart,basket}_rule"))
    util.rename_field(cr, "shop.order", *eb("{cust,partner}_ref"))
    util.rename_model(cr, *util.expand_braces("shop.{cart,basket}.line"))
    util.rename_xmlid(cr, *eb(f"shop.view_{'order'}_{{form,form2}}"))
    # Two brace groups: upgrade-util raises, so this stays partial.
    util.rename_xmlid(cr, *eb("shop.{a,b}.{c,d}"))
