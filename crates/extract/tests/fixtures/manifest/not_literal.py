from glob import glob

{
    'name': 'Compost Log',
    'version': VERSION,
    'depends': ['garden_base'],
    'data': ['views/heap_views.xml'],
    **extra,
    1: 'x',
    'data': glob('views/*.xml'),
    'license': 'LGPL-3',
}
