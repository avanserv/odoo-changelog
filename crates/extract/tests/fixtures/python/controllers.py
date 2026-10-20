from odoo import http
from odoo.http import request, route


class Portal(http.Controller):

    @http.route('/my/orders', type='http', auth='user', website=True)
    def orders(self, **kw):
        return request.render('demo.orders', kw)

    @http.route(['/my/order/<int:order_id>', '/my/order/<int:order_id>/print'], auth='public')
    def order(self, order_id, **kw):
        return self._render(order_id)

    @route()
    def inherited(self, **kw):
        return super().inherited(**kw)

    @http.route(route='/my/home', methods=['GET'], csrf=False)
    def home(self):
        return self._render(None)

    def _render(self, order_id):
        return request.render('demo.order', {'id': order_id})

    def test_render(self):
        return self._render(1)


def post_init_hook(env):
    env['demo.order'].search([]).unlink()


@decorator(1)
class Helper(Base, metaclass=Meta):
    pass
