import logging
import re

from odoo import fields, models

_logger = logging.getLogger(__name__)
__all__ = ['DemoOrder']

INVOICE_STATUS = [
    ('upselling', 'Upselling Opportunity'),
    ('invoiced', 'Fully Invoiced'),
    ('to invoice', 'To Invoice'),
    ('no', 'Nothing to Invoice'),
]
TYPE_MAP = {'out_invoice': 'out_refund', 'in_invoice': 'in_refund'}
MAX_HASH_VERSION = 3
MAX_HASH_VERSION = 4
DELIMITER = '||'
ACCOUNT_REGEX = re.compile(r'^[0-9]+$')
TEMPLATE_KEYS = frozenset({'name', 'code'})
LIMIT: int = 80
BARE: int
LIMIT: int
WIDTH = HEIGHT = 1024
LOWER, UPPER = 1, 2
DELIMITER += '|'
default_limit = 10
EMPTY = object()

if True:
    HIDDEN = 1


def STRATEGY():
    return 'fifo'


STRATEGY = 'lifo'


class DemoOrder(models.Model):
    _name = 'demo.order'
    _description = 'Order'

    STEP = 2
    invoice_status = fields.Selection(INVOICE_STATUS, string='Invoice Status')
