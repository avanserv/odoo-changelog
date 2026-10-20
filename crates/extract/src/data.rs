//! Odoo data files: the XML a module loads, by xmlid, with each view's named nodes and
//! inheritance specs, and the access CSVs. One extractor reads both, so that an access right
//! keeps its symbol whether a CSV row or an XML record declares it.

use std::borrow::Cow;
use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

use crate::path::module_of;
use crate::{Extractor, Symbol, SymbolTable, access_csv, xml};

pub(crate) const VIEW: &str = "view";
pub(crate) const VIEW_NODE: &str = "view_node";
pub(crate) const MENU: &str = "menu";
pub(crate) const ACTION: &str = "action";
pub(crate) const GROUP: &str = "group";
pub(crate) const RULE: &str = "rule";
pub(crate) const ACCESS: &str = "access";
pub(crate) const RECORD: &str = "record";
/// Not `function`, which the Python extractor owns.
pub(crate) const DATA_FUNCTION: &str = "data_function";
pub(crate) const DELETE: &str = "delete";

/// Attr keys holding xmlids as written: a string, or an array of strings for `groups`.
const REF_KEYS: [&str; 6] = ["action", "group_id", "groups", "inherit_id", "model_id", "parent"];

/// The kind of a `<record>` of `model`.
pub(crate) fn kind_of(model: &str) -> &'static str {
    match model {
        "ir.ui.view" | "theme.ir.ui.view" => VIEW,
        "ir.ui.menu" => MENU,
        "ir.actions.act_window"
        | "ir.actions.server"
        | "ir.actions.client"
        | "ir.actions.report"
        | "ir.actions.act_url"
        | "ir.actions.actions" => ACTION,
        "res.groups" => GROUP,
        "ir.rule" => RULE,
        "ir.model.access" | "ir.access" => ACCESS,
        _ => RECORD,
    }
}

/// Spans are exact for valid UTF-8 and approximate for a file in another encoding, which is
/// transcoded first, or with invalid bytes, which are replaced.
pub struct Data;

impl Extractor for Data {
    fn name(&self) -> &'static str {
        "data"
    }

    /// Bump with any change to the output, including a quick-xml upgrade.
    fn version(&self) -> u32 {
        1
    }

    /// Data files of a module, wherever they sit (views/, data/, template/, tests/, ...): Odoo
    /// loads any listed file. A foreign root such as an e-invoice sample gives an empty table.
    /// Files under static/ are client assets, which Odoo never loads as data.
    fn claims(&self, path: &str) -> bool {
        let file = path.rsplit('/').next().unwrap_or(path);
        module_of(path).is_some()
            && !path.split('/').any(|segment| segment == "static")
            && (file.ends_with(".xml") || matches!(file, "ir.model.access.csv" | "ir.access.csv"))
    }

    /// XML when the first byte past a BOM and whitespace is `<`, or there is none; CSV otherwise.
    fn extract(&self, blob: &[u8]) -> Result<SymbolTable> {
        let body = blob.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(blob).trim_ascii_start();
        Ok(match body.first() {
            None | Some(b'<') => xml::extract(blob),
            Some(_) => access_csv::extract(blob),
        })
    }

    /// `<xmlid>@<module>` with Odoo's rule for an id written without its module; a view node
    /// keeps its `#<node key>` after the xmlid. Calls and deletes by search are named after
    /// their model, which no module prefixes.
    fn qualify(&self, path: &str, symbol: &Symbol) -> String {
        let module = module_of(path).unwrap_or_default();
        if symbol.kind == DATA_FUNCTION || (symbol.kind == DELETE && symbol.name.contains('#')) {
            return format!("{}@{module}", symbol.name);
        }
        let (id, rest) = match symbol.name.split_once('#') {
            Some((id, rest)) => (id, format!("#{rest}")),
            None => (symbol.name.as_str(), String::new()),
        };
        let id = xml_id(module, id).unwrap_or_else(|| id.to_string());
        format!("{id}{rest}@{module}")
    }

    /// The xmlids under `REF_KEYS`, with the file's module where they are written without one.
    fn qualify_attrs<'a>(&self, path: &str, symbol: &'a Symbol) -> Cow<'a, BTreeMap<String, Value>> {
        let Some(module) = module_of(path) else {
            return Cow::Borrowed(&symbol.attrs);
        };
        let mut out: Option<BTreeMap<String, Value>> = None;
        for key in REF_KEYS {
            let qualified = match symbol.attrs.get(key) {
                Some(Value::String(r)) => xml_id(module, r).map(Value::String),
                Some(Value::Array(items))
                    if items
                        .iter()
                        .any(|i| i.as_str().and_then(|r| xml_id(module, r)).is_some()) =>
                {
                    let items = items.iter().map(|i| match i.as_str().and_then(|r| xml_id(module, r)) {
                        Some(r) => Value::String(r),
                        None => i.clone(),
                    });
                    Some(Value::Array(items.collect()))
                }
                _ => None,
            };
            if let Some(value) = qualified {
                out.get_or_insert_with(|| symbol.attrs.clone())
                    .insert(key.into(), value);
            }
        }
        out.map_or(Cow::Borrowed(&symbol.attrs), Cow::Owned)
    }
}

/// `make_xml_id`: `<module>.` before an id without a dot, after a `!` or `-` prefix. None when the
/// id is already whole.
fn xml_id(module: &str, id: &str) -> Option<String> {
    let rest = id.strip_prefix(['!', '-']).unwrap_or(id);
    let prefix = &id[..id.len() - rest.len()];
    (!module.is_empty() && !rest.is_empty() && !rest.contains('.')).then(|| format!("{prefix}{module}.{rest}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Span;

    fn symbol(kind: &str, name: &str, attrs: Value) -> Symbol {
        Symbol {
            kind: kind.into(),
            name: name.into(),
            line: 1,
            span: Span::default(),
            attrs: serde_json::from_value(attrs).unwrap(),
        }
    }

    #[test]
    fn claims_module_data_files_outside_static() {
        for path in [
            "addons/sale/views/x.xml",
            "l10n_cl_edi/template/a.xml",
            "odoo/addons/base/security/ir.model.access.csv",
            "addons/sale/security/ir.access.csv",
            "addons/sale/tests/data/x.xml",
        ] {
            assert!(Data.claims(path), "{path}");
        }
        for path in [
            "addons/sale/static/src/x.xml",
            "odoo/tests/dummy.xml",
            "addons/sale/data/res.country.csv",
            "addons/sale/x.xmlx",
        ] {
            assert!(!Data.claims(path), "{path}");
        }
    }

    #[test]
    fn qualify_every_kind() {
        let path = "addons/sale/views/x.xml";
        for (kind, name, qualname) in [
            (VIEW, "view_order_form", "sale.view_order_form@sale"),
            (
                VIEW_NODE,
                "view_order_form#field[@name='partner_id']",
                "sale.view_order_form#field[@name='partner_id']@sale",
            ),
            (MENU, "base.menu_x", "base.menu_x@sale"),
            (ACTION, "action_orders", "sale.action_orders@sale"),
            (GROUP, "group_x", "sale.group_x@sale"),
            (RULE, "rule_x", "sale.rule_x@sale"),
            (ACCESS, "access_x", "sale.access_x@sale"),
            (RECORD, "mail_template_x", "sale.mail_template_x@sale"),
            (
                DATA_FUNCTION,
                "product.product#write[2]",
                "product.product#write[2]@sale",
            ),
            (DELETE, "menu_x", "sale.menu_x@sale"),
            (DELETE, "account.tax#search", "account.tax#search@sale"),
        ] {
            assert_eq!(
                Data.qualify(path, &symbol(kind, name, json!({}))),
                qualname,
                "{kind} {name}"
            );
        }
        assert_eq!(Data.qualify("odoo/x.xml", &symbol(VIEW, "v", json!({}))), "v@");
    }

    #[test]
    fn qualify_attrs_prefixes_the_files_module() {
        let path = "addons/sale/views/x.xml";
        let attrs = json!({
            "parent": "menu_root",
            "action": "base.action_x",
            "groups": ["group_a", "-group_b", "base.group_c", "!x"],
            "inherit_id": {"eval": "False"},
            "model_id": "",
            "name": "not_a_ref",
        });
        let s = symbol(MENU, "m", attrs);
        let qualified = Data.qualify_attrs(path, &s);
        assert_eq!(
            Value::from_iter(qualified.into_owned()),
            json!({
                "parent": "sale.menu_root",
                "action": "base.action_x",
                "groups": ["sale.group_a", "-sale.group_b", "base.group_c", "!sale.x"],
                "inherit_id": {"eval": "False"},
                "model_id": "",
                "name": "not_a_ref",
            })
        );

        let whole = symbol(
            MENU,
            "m",
            json!({"parent": "base.menu_root", "groups": ["base.g"], "name": "x"}),
        );
        assert!(matches!(Data.qualify_attrs(path, &whole), Cow::Borrowed(_)));
        let s = symbol(MENU, "m", json!({"parent": "menu_root"}));
        assert!(matches!(Data.qualify_attrs("odoo/x.xml", &s), Cow::Borrowed(_)));
    }
}
