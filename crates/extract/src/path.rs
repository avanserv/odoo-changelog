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

#[cfg(test)]
mod tests {
    use super::*;

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
