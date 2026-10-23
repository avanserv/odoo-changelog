from odoo.upgrade import util

PREFIX = "shop." + model
SUFFIXES = ["_a"]
SUFFIXES.append("_b")


def migrate(cr, version):
    # Rebound in the loop body: the loop value alone would be wrong.
    for name in ["form", "tree"]:
        name = "shop.view_" + name
        util.remove_view(cr, name)

    # PREFIX reads the module's `model`, not this loop's.
    for model in ["shop.a"]:
        util.remove_record(cr, PREFIX)

    renames = {}
    renames["shop.a"] = "shop.b"
    for old, new in renames.items():
        util.rename_model(cr, old, new)

    for suffix in SUFFIXES:
        util.remove_record(cr, "shop.x" + suffix)

    xmlid = "shop.outer"
    [util.remove_record(cr, xmlid) for xmlid in ["shop.inner"]]

    util.remove_record(cr, "shop.\x61ctioné")
    util.remove_record(cr, "shop.\N{BULLET}")
    util.remove_record(cr, f"{xmlid=}")
    util.remove_record(cr, "%s" % ["shop.list"])

    seen = {"a": "shop.first", "a": "shop.last"}
    util.remove_record(cr, "%(a)s" % seen)
    for m in {"shop.m", "shop.m"}:
        util.remove_model(cr, m)

    m = "shop.with"
    with open("x") as m:
        pass
    util.remove_model(cr, m)

    for outer in ["shop.a", "shop.b"]:
        for outer in ["shop.c"]:
            pass
        util.remove_model(cr, outer)

    for once in set(["shop.s", "shop.s"]):
        util.remove_model(cr, once)

    util.remove_view(cr, xml_id=None, key="shop.by_key")

    for helped in HELPED:
        util.remove_model(cr, helped)
    util.remove_model(cr, GLOBAL)


HELPED = ["shop.h"]
GLOBAL = "shop.g"


def _more():
    global GLOBAL
    GLOBAL = "shop.g2"
    HELPED.append("shop.h2")


def _except(cr):
    e = "shop.e"
    try:
        pass
    except Exception as e:
        pass
    util.remove_record(cr, e)
