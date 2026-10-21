from openerp import models, fields


class Legacy(models.Model):
    _name = 'demo.legacy'

    name = fields.Char()

    def run(self):
        print "running"
        try:
            self.go()
        except ValueError, e:
            print `e`
        if self.a <> self.b:
            pass

    def after(self):
        return True
