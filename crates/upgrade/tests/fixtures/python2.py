# -*- coding: utf-8 -*-
from openerp.addons.base.maintenance.migrations import util


def migrate(cr, version):
    print "upgrading"
    try:
        util.rename_field(cr, 'shop.order', 'cust', 'partner_id')
    except Exception, e:
        pass
    for m in ['shop.a', 'shop.b']:
        util.delete_model(cr, m)
    if cr.rowcount <> 0:
        util.remove_record(cr, 'shop.menu_old')
