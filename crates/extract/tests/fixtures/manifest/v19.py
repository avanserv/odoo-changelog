{
    'name': "Orchard Tours",
    'version': '1.2',
    'category': 'Website/Tours',
    'summary': "Book guided walks through the orchard",
    'author': 'Example Growers',
    'depends': [
        'website',  # -> web, portal
        'orchard_base',

        'calendar',
    ],
    'data': [
        # Security first, so that later records can refer to the groups.
        'security/orchard_security.xml',
        'security/ir.model.access.csv',

        'views/tour_views.xml',  # backend
        'views/tour_templates.xml',
    ],
    'assets': {
        'web.assets_frontend': [
            'orchard_tours/static/src/js/**/*',
            ('include', 'web._assets_helpers'),
            ("prepend", "orchard_tours/static/src/scss/primary_variables.scss"),
        ],
        'web.assets_backend': [
            'orchard_tours/static/src/views/*.js',
            'orchard_tours/static/src/views/*.xml',
            ('remove', 'orchard_tours/static/src/views/legacy/**/*'),
            ('after', 'web/static/src/core/main.js', 'orchard_tours/static/src/boot.js'),
            ("before", "web/static/src/start.js", "orchard_tours/static/src/early.js"),
            ('replace', 'web/static/src/old_widget.js', 'orchard_tours/static/src/widget.js'),
        ],
        'web.assets_tests': [
            'orchard_tours/static/tests/tours/*.js',
        ],
    },
    'configurator_snippets': {
        'homepage': ['s_banner', 's_text_image'],
        'about_us': ['s_company_team'],
    },
    'countries': ['be', 'fr'],
    'auto_install': ['orchard_base'],
    'post_init_hook': '_post_init_hook',
    'license': 'OEEL-1',
}
