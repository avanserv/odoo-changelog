//! Where a script sits: `migrations/<module>/<folder>/<file>.py`, and the version its folder names.

/// A script's place in the upgrade repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script<'a> {
    pub module: &'a str,
    pub folder: &'a str,
    /// The branch the folder upgrades to, such as `17.0` or `saas-17.2`; `None` for `0.0.0`,
    /// which runs on every upgrade.
    pub version: Option<String>,
    /// `pre`, `post`, `end` or `init`, from the file name; `None` for a helper module.
    pub phase: Option<&'a str>,
}

/// The script at `path`, or `None` for anything the upgrade does not run as one: files outside a
/// version folder, `tests/`, `__pycache__/`, `_`-prefixed modules, `test_*.py` and non-Python
/// files.
pub fn script(path: &str) -> Option<Script<'_>> {
    let mut parts = path.split('/');
    let (Some("migrations"), Some(module), Some(folder), Some(file), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if module.starts_with('_') || file.starts_with("test_") || !file.ends_with(".py") {
        return None;
    }
    let version = folder_version(folder)?;
    // Odoo runs `<phase>-*.py` only; anything else is a helper another script imports.
    let phase = ["pre", "post", "end", "init"]
        .into_iter()
        .find(|p| file.strip_prefix(p).is_some_and(|rest| rest.starts_with('-')));
    Some(Script {
        module,
        folder,
        version,
        phase,
    })
}

/// The branch a version folder upgrades to: `Some(None)` for `0.0.0`, `None` for a folder that
/// names no version. A folder is the series followed by the module's own version:
/// `17.0.1.2` is 17.0, `saas~17.2.1.1` saas-17.2, and the older `9.saas~11.1.3` saas-11.
/// From 16 on Odoo releases no saas-X.5: master carries (X, 5) until (X+1).0 forks, so a
/// `saas~17.5` folder holds 18.0's scripts.
pub fn folder_version(folder: &str) -> Option<Option<String>> {
    if folder == "0.0.0" {
        return Some(None);
    }
    let numbers = |s: &str| -> Option<Vec<u32>> { s.split('.').map(|n| n.parse().ok()).collect() };
    if let Some((series, rest)) = folder.split_once(".saas~") {
        series.parse::<u32>().ok()?;
        let saas = numbers(rest)?;
        return (saas.len() >= 2).then(|| Some(format!("saas-{}", saas[0])));
    }
    if let Some(rest) = folder.strip_prefix("saas~") {
        let n = numbers(rest)?;
        if n.len() < 3 || n[1] == 0 {
            return None;
        }
        return Some(Some(if n[0] >= 16 && n[1] == 5 {
            format!("{}.0", n[0] + 1)
        } else {
            format!("saas-{}.{}", n[0], n[1])
        }));
    }
    let n = numbers(folder)?;
    (n.len() >= 3 && n[1] == 0).then(|| Some(format!("{}.0", n[0])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_name_their_series() {
        for (folder, want) in [
            ("17.0.1.2", Some(Some("17.0"))),
            ("16.0.13.0.1", Some(Some("16.0"))),
            ("8.0.1.0", Some(Some("8.0"))),
            ("saas~17.2.1.1", Some(Some("saas-17.2"))),
            ("saas~16.5.0.0.1", Some(Some("17.0"))),
            ("saas~18.5.1.0", Some(Some("19.0"))),
            ("saas~14.5.1.2", Some(Some("saas-14.5"))),
            ("saas~11.1.1.3", Some(Some("saas-11.1"))),
            ("9.saas~11.1.3", Some(Some("saas-11"))),
            ("10.saas~17.1.0.1", Some(Some("saas-17"))),
            ("0.0.0", Some(None)),
            ("tests", None),
            ("__pycache__", None),
            ("1.0", None),
            ("17.1.1.0", None),
            ("saas~17.0.1.0", None),
            ("saas~17.2", None),
        ] {
            assert_eq!(folder_version(folder), want.map(|v| v.map(String::from)), "{folder}");
        }
    }

    #[test]
    fn scripts_sit_in_a_version_folder() {
        assert_eq!(
            script("migrations/sale/saas~17.2.1.1/pre-migrate.py"),
            Some(Script {
                module: "sale",
                folder: "saas~17.2.1.1",
                version: Some("saas-17.2".into()),
                phase: Some("pre"),
            })
        );
        for (file, phase) in [
            ("post-10-data.py", Some("post")),
            ("end-views.py", Some("end")),
            ("init-x.py", Some("init")),
            ("pre_migrate.py", None),
            ("sanitize.py", None),
            ("__init__.py", None),
        ] {
            let path = format!("migrations/sale/17.0.1.2/{file}");
            assert_eq!(script(&path).unwrap().phase, phase, "{file}");
        }
        assert_eq!(script("migrations/base/0.0.0/end-views.py").unwrap().version, None);
        for skipped in [
            "migrations/sale/tests/test_sale.py",
            "migrations/sale/17.0.1.2/__pycache__/pre-migrate.py",
            "migrations/sale/17.0.1.2/tests/test_x.py",
            "migrations/sale/17.0.1.2/test_sale.py",
            "migrations/_lib2to3_fixers/17.0.1.2/fix.py",
            "migrations/base/0.0.0/test_views.xml",
            "tools/17.0.1.2/x.py",
        ] {
            assert_eq!(script(skipped), None, "{skipped}");
        }
    }
}
