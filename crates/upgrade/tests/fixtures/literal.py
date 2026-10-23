# -*- coding: utf-8 -*-
from odoo.upgrade import util


def migrate(cr, version):
    util.rename_field(cr, "shop.order", "customer_id", "partner_id")
    util.rename_model(cr, "shop.cart", "shop.basket", rename_table=False)
    util.rename_xmlid(cr, "shop.view_cart_form", "shop.view_basket_form", noupdate=False)
    util.rename_module(cr, "shop_extra", "shop_plus")
    util.merge_module(cr, "shop_legacy", "shop", update_dependers=False)
    util.move_field_to_module(cr, "shop.order", "coupon_code", "shop", "shop_coupon")
    util.move_model(cr, "shop.coupon", "shop", "shop_coupon", move_data=True)
    util.merge_model(cr, "shop.quote", "shop.order")
    util.remove_field(cr, "shop.order", "legacy_ref", drop_column=False)
    util.remove_field(cr, model="shop.order", fieldname="old_note")
    util.remove_model(cr, "shop.wizard")
    util.delete_model(cr, "shop.report")
    util.remove_view(cr, "shop.view_order_tree")
    util.remove_view(cr, xml_id="shop.view_order_kanban", silent=True)
    util.remove_view(cr, view_id=vid, key="shop.portal_layout")
    util.remove_record(
        cr,
        "shop.action_cart",
    )
    util.remove_record(cr, name='shop.menu_cart')
    util.remove_module(cr, "shop_demo")
    util.remove_column(cr, "shop_order", "legacy_ref")
    util.if_unchanged(cr, "shop.mail_template", util.remove_record)
