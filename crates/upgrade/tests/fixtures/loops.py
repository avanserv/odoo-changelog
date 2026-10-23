from odoo.upgrade import util

RENAMES = {
    "shop.order": [("cust_ref", "partner_ref"), ("amount", "amount_total")],
    "shop.cart": [("owner", "user_id")],
}


def migrate(cr, version):
    for old, new in [("note", "description"), ("ref", "reference")]:
        util.rename_field(cr, "shop.order", old, new)

    for model, renames in RENAMES.items():
        for old, new in renames:
            util.rename_field(cr, model, old, new)

    for name in util.splitlines(
        """
        shop.view_a   # a comment
        shop.view_b

        """
    ):
        util.remove_view(cr, name)

    for field in "promo_code promo_ok".split():
        if util.column_exists(cr, "shop_order", field):
            util.remove_field(cr, "shop.order", field)

    xmlids = ("shop.action_a", "shop.action_b")
    for xmlid in xmlids:
        for suffix in ("", "_menu"):
            util.remove_record(cr, xmlid + suffix)

    # The loop does not touch the call's arguments: one event, not three.
    for _ in range(3):
        util.remove_model(cr, "shop.temp")

    for i, model in enumerate(["shop.a", "shop.b"]):
        util.remove_model(cr, model)

    cr.execute("SELECT name FROM ir_model WHERE model LIKE 'shop.x%'")
    for (model,) in cr.fetchall():
        util.remove_model(cr, model)
