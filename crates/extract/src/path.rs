/// The Odoo module the file at `path` belongs to, in community, enterprise or design-themes.
/// Framework files (`odoo/` outside `odoo/addons/`) and top-level files belong to none. Any other
/// top-level directory reads as a module, which also names community's `setup/`, `doc/` and
/// `debian/`: they hold no models, so only odd Python-scope qualnames come of it.
pub fn module_of(path: &str) -> Option<&str> {
    let rest = match path
        .strip_prefix("odoo/addons/")
        .or_else(|| path.strip_prefix("addons/"))
    {
        Some(rest) => rest,
        None if path.starts_with("odoo/") => return None,
        None => path,
    };
    rest.split_once('/').map(|(module, _)| module)
}

/// The addon that serves the file: the segment before the first `static` one, since Odoo maps
/// `<addon>/static/...` to the URL `/<addon>/static/...`, nested test modules included. `None`
/// when there is no such segment or it is no addon name.
pub(crate) fn static_addon(path: &str) -> Option<&str> {
    static_file(path).map(|(addon, _)| addon)
}

/// `static_addon`, and the segments after that `static` one.
pub(crate) fn static_file(path: &str) -> Option<(&str, Vec<&str>)> {
    let segments: Vec<&str> = path.split('/').collect();
    let at = segments.iter().position(|s| *s == "static")?;
    let addon = *segments.get(at.checked_sub(1)?)?;
    let valid = !addon.is_empty() && addon.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    valid.then(|| (addon, segments[at + 1..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_addon_is_the_segment_before_static() {
        for (path, addon) in [
            ("addons/web/static/src/x.xml", Some("web")),
            ("addons/web/static/lib/bootstrap/scss/_variables.scss", Some("web")),
            ("odoo/addons/base/static/src/x.scss", Some("base")),
            ("theme_bistro/static/src/scss/a.scss", Some("theme_bistro")),
            (
                "addons/base_import_module/tests/test_module/static/src/a.xml",
                Some("test_module"),
            ),
            ("static/x.xml", None),
            ("addons/web/views/x.xml", None),
            ("addons/web.x/static/src/x.xml", None),
        ] {
            assert_eq!(static_addon(path), addon, "{path}");
        }
    }

    #[test]
    fn module_of_every_layout() {
        for (path, module) in [
            ("addons/sale/models/x.py", Some("sale")),
            ("odoo/addons/base/models/res_users.py", Some("base")),
            ("odoo/models.py", None),
            ("odoo/addons/__init__.py", None),
            ("sale_subscription/models/x.py", Some("sale_subscription")),
            ("theme_anelusia/models/theme_anelusia.py", Some("theme_anelusia")),
            ("setup.py", None),
            ("addons/__init__.py", None),
        ] {
            assert_eq!(module_of(path), module, "{path}");
        }
    }
}
