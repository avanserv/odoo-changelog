//! Where a file sits in Odoo's JS module system, after `odoo/tools/js_transpiler.py` (15.0 and
//! later): its addon, its module name, how its relative imports resolve and its `@odoo-module`
//! header.

use std::borrow::Cow;
use std::ops::Range;

/// The addon and the segments after `static/src` of `path`, when its first `static` segment is
/// followed by `src` and has a segment before it. Odoo would name a file under a second
/// `static/src` after the last one, but such a file sits under another directory of `static`.
fn static_src(path: &str) -> Option<(&str, Vec<&str>)> {
    let segments: Vec<&str> = path.split('/').collect();
    let at = segments.iter().position(|s| *s == "static")?;
    if segments.get(at + 1) != Some(&"src") {
        return None;
    }
    let addon = *segments.get(at.checked_sub(1)?)?;
    Some((addon, segments[at + 2..].to_vec()))
}

/// Web client sources Odoo serves from `static/src`; tests and third-party libraries under
/// `static/tests` and `static/lib` are not claimed, nor test directories inside `static/src`.
pub(super) fn claims(path: &str) -> bool {
    let Some((addon, rest)) = static_src(path) else {
        return false;
    };
    let Some((file, dirs)) = rest.split_last() else {
        return false;
    };
    file.ends_with(".js")
        && !file.ends_with(".test.js")
        && !addon.is_empty()
        && addon.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !dirs.iter().any(|d| matches!(*d, "tests" | "test"))
}

/// The addon whose `static/src` holds the file.
pub(super) fn addon(path: &str) -> &str {
    static_src(path).map_or("", |(addon, _)| addon)
}

/// The URL Odoo serves the file at, `/<addon>/static/src/<rest>`.
fn url(path: &str) -> Option<String> {
    let (addon, rest) = static_src(path)?;
    Some(format!("/{addon}/static/src/{}", rest.join("/")))
}

/// The name Odoo defines the file's module under, such as `@web/core/registry`.
pub(super) fn module_path(path: &str) -> String {
    url(path).and_then(|u| url_to_module_path(&u)).unwrap_or_default()
}

/// `url_to_module_path`: `URL_RE`'s greedy module group takes the last `/static/<type>/`, and
/// the path after it stops at whitespace.
fn url_to_module_path(url: &str) -> Option<String> {
    let url = url.strip_prefix('/').unwrap_or(url);
    url.rmatch_indices("/static/").find_map(|(at, _)| {
        let module = &url[..at];
        let after = &url[at + "/static/".len()..];
        let (kind, rest) = ["src", "tests", "lib"]
            .into_iter()
            .find_map(|kind| Some((kind, after.strip_prefix(kind).filter(|r| r.starts_with('/'))?)))?;
        if module.is_empty() || module.contains(char::is_whitespace) {
            return None;
        }
        let rest = rest.split(char::is_whitespace).next().unwrap_or(rest);
        let rest = match rest.ends_with("/index.js") || rest.ends_with("/index") {
            true => rest.rsplit_once('/').map_or(rest, |(r, _)| r),
            false => rest,
        };
        let rest = rest.strip_suffix(".js").unwrap_or(rest);
        Some(match kind {
            "src" => format!("@{module}{rest}"),
            "lib" => format!("@{module}/../lib{rest}"),
            _ => format!("@{module}/../tests{rest}"),
        })
    })
}

/// `remove_index`, which the sources of an ES module's imports and re-exports go through:
/// `x/index` and `x/index/` read as `x`.
pub(super) fn remove_index(spec: &str) -> &str {
    spec.strip_suffix('/')
        .unwrap_or(spec)
        .strip_suffix("/index")
        .unwrap_or(spec)
}

/// `relative_path_to_module_path`, for a specifier that starts with `.` and holds a `/`; `None`
/// for any other, or where Odoo would fail to name the result.
fn relative(path: &str, spec: &str) -> Option<String> {
    if !(spec.starts_with('.') && spec.contains('/')) {
        return None;
    }
    let url = url(path)?;
    let url: Vec<&str> = url.split('/').collect();
    let parts: Vec<&str> = spec.split('/').collect();
    let back = parts.iter().filter(|p| **p == "..").count() + 1;
    let mut joined: Vec<&str> = url[..url.len().saturating_sub(back)].to_vec();
    joined.extend(parts.iter().filter(|p| !matches!(**p, "." | "..")));
    url_to_module_path(&joined.join("/"))
}

/// A reference or name as the file at `path` means it: an empty module (`""`, `#x`) is the
/// file's own, and a relative one is resolved against the file.
pub(super) fn qualify_ref<'a>(path: &str, reference: &'a str) -> Cow<'a, str> {
    let (module, member) = reference.split_at(reference.find('#').unwrap_or(reference.len()));
    if module.is_empty() {
        return Cow::Owned(format!("{}{member}", module_path(path)));
    }
    match relative(path, module) {
        Some(module) => Cow::Owned(format!("{module}{member}")),
        None => Cow::Borrowed(reference),
    }
}

/// The `@odoo-module` comment, read like 18.0's `ODOO_MODULE_RE`: the comment opens the file and
/// the marker sits on its first line, the last one there winning as the greedy `.*` does.
#[derive(Debug, PartialEq)]
pub(super) struct Header {
    /// The `@odoo-module` marker.
    pub at: Range<usize>,
    pub ignore: bool,
    pub alias: Option<Range<usize>>,
    pub default: Option<Range<usize>>,
}

const MARKER: &[u8] = b"@odoo-module";

pub(super) fn header(blob: &[u8]) -> Option<Header> {
    let start = if blob.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let start = start + blob[start..].iter().take_while(|b| b.is_ascii_whitespace()).count();
    if !(blob[start..].starts_with(b"/*") || blob[start..].starts_with(b"//")) {
        return None;
    }
    let line_end = blob[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(blob.len(), |i| start + i);
    let line = &blob[start + 2..line_end];
    let at = start + 2 + line.windows(MARKER.len()).rposition(|w| w == MARKER)?;
    let mut end = at + MARKER.len();
    let ignore = after_space(blob, end, b"ignore").inspect(|&e| end = e).is_some();
    let mut value = |prefix: &[u8], allowed: fn(u8) -> bool| {
        let from = after_space(blob, end, prefix)?;
        let len = blob[from..].iter().take_while(|&&b| allowed(b)).count();
        (len > 0).then(|| {
            end = from + len;
            from..from + len
        })
    };
    let alias = value(b"alias=", |b| !b.is_ascii_whitespace() && b != b'*');
    let default = value(b"default=", |b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
    Some(Header {
        at: at..at + MARKER.len(),
        ignore,
        alias,
        default,
    })
}

/// The offset after `\s+<literal>` at `at`.
fn after_space(blob: &[u8], at: usize, literal: &[u8]) -> Option<usize> {
    let space = blob[at..].iter().take_while(|b| b.is_ascii_whitespace()).count();
    (space > 0 && blob[at + space..].starts_with(literal)).then_some(at + space + literal.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_static_src_js_only() {
        for path in [
            "addons/web/static/src/core/registry.js",
            "odoo/addons/base/static/src/x.js",
            "web_studio/static/src/a/index.js",
            "addons/web/static/src/libs/fullcalendar.js",
            "addons/base_import_module/tests/test_module/static/src/js/test.js",
        ] {
            assert!(claims(path), "{path}");
        }
        for path in [
            "addons/web/static/tests/core/registry_tests.js",
            "addons/web/static/lib/jquery/jquery.js",
            "addons/web/static/src/core/registry.test.js",
            "addons/mail/static/src/components/thread/tests/thread_tests.js",
            "addons/web_editor/static/src/js/editor/odoo-editor/test/spec/a.js",
            "addons/web/static/src/core/types.d.ts",
            "addons/web/static/src/core/x.mjs",
            "addons/web/static/src/core/x.xml",
            "addons/web/static/image_shapes/x.js",
            "addons/web/tooling/eslint.js",
            "static/src/x.js",
            "addons/web-x/static/src/x.js",
            "addons/web/static/src",
            "addons/x/static/lib/foo/static/src/y.js",
        ] {
            assert!(!claims(path), "{path}");
        }
    }

    #[test]
    fn module_path_follows_odoo() {
        for (path, module) in [
            ("addons/x/static/src/a/b.js", "@x/a/b"),
            ("odoo/addons/base/static/src/x.js", "@base/x"),
            ("web_studio/static/src/a/index.js", "@web_studio/a"),
            ("addons/web/static/src/index.js", "@web"),
            (
                "addons/base_import_module/tests/test_module/static/src/js/test.js",
                "@test_module/js/test",
            ),
            ("addons/sign/static/src/fields/ many2many/x.js", "@sign/fields/"),
        ] {
            assert_eq!(module_path(path), module, "{path}");
            assert_eq!(addon(path), module.trim_start_matches('@').split('/').next().unwrap());
        }
    }

    #[test]
    fn relative_specifiers_resolve_like_odoo() {
        let path = "addons/web/static/src/views/fields/many2one/many2one_field.js";
        for (spec, resolved) in [
            ("./a", "@web/views/fields/many2one/a"),
            ("../standard_field_props", "@web/views/fields/standard_field_props"),
            ("../../a/index", "@web/views/a"),
            ("./utils/utils.js", "@web/views/fields/many2one/utils/utils"),
            ("../../../../tests/helpers", "@web/../tests/helpers"),
            ("../../../../lib/x.js", "@web/../lib/x"),
            ("@web/core/registry", "@web/core/registry"),
            ("./", "@web/views/fields/many2one/"),
            ("x", "x"),
            (".", "."),
        ] {
            assert_eq!(qualify_ref(path, spec), resolved, "{spec}");
        }
        assert_eq!(qualify_ref(path, "../../a/b#c.d"), "@web/views/a/b#c.d");
        assert_eq!(
            qualify_ref(path, "#Foo"),
            "@web/views/fields/many2one/many2one_field#Foo"
        );
        assert_eq!(qualify_ref(path, ""), "@web/views/fields/many2one/many2one_field");
        assert_eq!(qualify_ref(path, "web.Widget#x"), "web.Widget#x");
        assert_eq!(
            qualify_ref(path, "../../../../../../../x/y"),
            "../../../../../../../x/y"
        );
    }

    #[test]
    fn index_is_dropped() {
        for (spec, stripped) in [
            ("@web/core/index", "@web/core"),
            ("./a/index/", "./a"),
            ("./index", "."),
            ("@web/core/indexes", "@web/core/indexes"),
            ("index", "index"),
        ] {
            assert_eq!(remove_index(spec), stripped, "{spec}");
        }
    }

    fn read(src: &str) -> Option<(String, bool, Option<String>, Option<String>)> {
        let h = header(src.as_bytes())?;
        let text = |r: Option<Range<usize>>| r.map(|r| src[r].to_string());
        assert_eq!(&src[h.at.clone()], "@odoo-module");
        Some((
            src[h.at.start..].lines().next().unwrap().into(),
            h.ignore,
            text(h.alias),
            text(h.default),
        ))
    }

    #[test]
    fn header_variants() {
        for src in [
            "/** @odoo-module **/\n",
            "/* @odoo-module */",
            "// @odoo-module\nimport x from 'y';",
            "\n\n  /** @odoo-modules */",
            "\u{feff}/** @odoo-module */",
        ] {
            let (_, ignore, alias, default) = read(src).unwrap_or_else(|| panic!("{src:?}"));
            assert_eq!((ignore, alias, default), (false, None, None), "{src:?}");
        }
        let (_, ignore, alias, default) = read("/** @odoo-module alias=web.Widget default=false */").unwrap();
        assert_eq!(
            (ignore, alias.as_deref(), default.as_deref()),
            (false, Some("web.Widget"), Some("false"))
        );
        assert!(read("/* @odoo-module ignore */").unwrap().1);
        assert_eq!(
            read("// @odoo-module alias=@web/../tests/x").unwrap().2.as_deref(),
            Some("@web/../tests/x")
        );
        for src in [
            "/**\n * @odoo-module\n */",
            "'use strict';\n/** @odoo-module */",
            "/** odoo-module */",
            "",
        ] {
            assert_eq!(header(src.as_bytes()), None, "{src:?}");
        }
    }
}
