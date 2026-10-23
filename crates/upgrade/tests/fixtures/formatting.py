from odoo.upgrade import util

MODULE = "shop"


def migrate(cr, version):
    prefix = "shop.view"
    util.remove_view(cr, prefix + "_order_form")
    util.remove_view(cr, f"{prefix}_order_tree")
    util.remove_view(cr, "%s_order_%s" % (prefix, "kanban"))
    util.remove_view(cr, "%(p)s_order_pivot" % {"p": prefix})
    util.remove_record(cr, "{}.action_{}".format(MODULE, "order"))
    util.remove_record(cr, "{m}.menu_{0}".format("order", m=MODULE))
    util.remove_record(cr, MODULE + "." "menu_" 'root')
    util.remove_record(cr, "shop.action_%d" % 3)
    util.remove_record(cr, prefix.replace("view", "action"))
    util.remove_record(cr, "shop.a.b.c".split(".", 1)[1])
    util.remove_record(cr, f"shop.{'A_B'.lower()}")
    util.remove_record(cr, f"{prefix!r}")
    util.remove_record(cr, "{:>10}".format(prefix))

    twice = "shop.a"
    twice = "shop.b"
    util.remove_record(cr, twice)


def _remove(cr, xmlid):
    util.remove_record(cr, xmlid)
