from odoo import _, fields, models


class Book(models.Model):
    _name = 'demo.book'
    _description = 'Book'

    name = fields.Char('Title', required=True)
    title = fields.Char(string="Title", required=True)
    state = fields.Selection([('a', 'A')], 'State')
    kind = fields.Reference(selection='_kinds', string='Kind')
    author_id = fields.Many2one('res.partner', 'Author', ondelete='cascade')
    editor_id = fields.Many2one(comodel_name='res.partner', string='Editor')
    page_ids = fields.One2many('demo.page', 'book_id', 'Pages')
    tag_ids = fields.Many2many('demo.tag', 'demo_book_tag_rel', 'book_id', 'tag_id', 'Tags')
    weight = fields.Float('Weight', (16, 2))
    price = fields.Monetary('Price', 'currency_id')
    company_id = fields.Many2one(
        'res.company',
        default=lambda self: self.env.company,
    )
    date = fields.Date(default=fields.Date.today, string=_('Date'))
    help_text = fields.Text(
        help="A help text that was "
        'wrapped over two lines.',
    )
    extra = fields.Char('Extra', 'unexpected', **common_kwargs)
    notes: str = fields.Html()
    pages = fields.Integer()
    pages = fields.Integer(compute='_compute_pages')

    def _kinds(self):
        return []
