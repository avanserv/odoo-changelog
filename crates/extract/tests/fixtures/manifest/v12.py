# -*- coding: utf-8 -*-
# Part of a made-up suite. See LICENSE file for full copyright and licensing details.

{
    'name': u'Garden Planner',
    'version': '1.3',
    'category': 'Gardening',
    'sequence': '12',
    'summary': 'Plan beds, '
               'sow seeds and track harvests',
    'description': """
Garden Planner
==============
Lay out beds, schedule sowing and record what each plot gives.
    """,
    'website': 'https://example.com/garden',
    'depends': ['plant_base', 'calendar', 'mail'],
    'data': [
        'security/ir.model.access.csv',
        'views/garden_bed_views.xml',
        'views/sowing_views.xml',
        'data/plant_data.xml',
    ],
    'demo': [
        'demo/garden_demo.xml',
    ],
    'qweb': ['static/src/xml/planner.xml'],
    'external_dependencies': {'python': ['soilkit'], 'bin': ['moonphase']},
    'installable': True,
    'auto_install': False,
    'application': True,
    'post_init_hook': '_seed_default_beds',
    'uninstall_hook': "uninstall_hook",
    'license': 'LGPL-3',
}
