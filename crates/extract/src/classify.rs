//! Sorts a commit's events into functional (what a user sees) and technical (what custom code
//! sees), and flags those that break custom code, naming the rule that decided each. Reads
//! stored events rather than symbol tables, so a rule change re-reads no blob.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// Bump on any change to what `commit` returns for some input: `extract` reclassifies every
/// commit classified under another version.
pub const VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Functional,
    Technical,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Functional => "functional",
            Category::Technical => "technical",
        }
    }
}

/// One event as `events` stores it, attrs as their JSON text.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    pub kind: &'a str,
    pub qualname: &'a str,
    pub op: &'a str,
    pub module: Option<&'a str>,
    pub old_module: Option<&'a str>,
    pub attrs_before: Option<&'a str>,
    pub attrs_after: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Class {
    pub category: Category,
    pub breaking: bool,
    /// The breaking rule when one fired, else the category rule.
    pub rule: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitClass {
    /// `None` for a commit without events.
    pub category: Option<Category>,
    pub breaking: bool,
    pub rule: &'static str,
}

/// Arch node tags a user sees; a `div` or `group` is named mostly as an xpath anchor.
const VISIBLE_NODES: &[&str] = &["field", "button", "page", "filter", "notebook", "separator", "label"];
const LABEL_ARGS: &[&str] = &["string", "help", "selection", "selection_add"];
const FUNCTIONAL_TAGS: &[&str] = &["ADD", "IMP", "FIX", "REM"];

/// The class of each event, in input order, and of the commit: functional when it adds, removes
/// or renames a module, else the category most of its events have, its tag on a tie; breaking
/// when any event is.
pub fn commit(tag: Option<&str>, events: &[Event]) -> (Vec<Class>, CommitClass) {
    let parsed: Vec<Parsed> = events.iter().map(Parsed::new).collect();
    let viewed: BTreeSet<&str> = parsed
        .iter()
        .filter(|p| p.e.kind == "view" && p.e.op == "added")
        .filter_map(|p| p.after.get("model")?.as_str())
        .collect();
    let classes: Vec<Class> = parsed.iter().map(|p| class(p, &viewed)).collect();

    let functional = classes.iter().filter(|c| c.category == Category::Functional).count();
    let technical = classes.len() - functional;
    // A new module's fields, records and styles outnumber the views and menus that make it one.
    let module = events
        .iter()
        .any(|e| e.kind == "module" && matches!(e.op, "added" | "removed" | "renamed"));
    let (category, rule) = if classes.is_empty() {
        (None, "commit-no-events")
    } else if module {
        (Some(Category::Functional), "commit-module")
    } else if functional != technical {
        let category = if functional > technical {
            Category::Functional
        } else {
            Category::Technical
        };
        (Some(category), "commit-majority")
    } else if let Some(tag) = tag {
        let category = if FUNCTIONAL_TAGS.contains(&tag) {
            Category::Functional
        } else {
            Category::Technical
        };
        (Some(category), "commit-tag")
    } else {
        (Some(Category::Technical), "commit-default")
    };
    let commit = CommitClass {
        category,
        breaking: classes.iter().any(|c| c.breaking),
        rule,
    };
    (classes, commit)
}

struct Parsed<'a> {
    e: &'a Event<'a>,
    before: Map<String, Value>,
    after: Map<String, Value>,
}

impl<'a> Parsed<'a> {
    fn new(e: &'a Event<'a>) -> Self {
        Parsed {
            e,
            before: object(e.attrs_before),
            after: object(e.attrs_after),
        }
    }

    /// Whether `key` differs between the sides of an event that has two.
    fn differs(&self, key: &str) -> bool {
        !matches!(self.e.op, "added" | "removed") && (self.before.contains_key(key) || self.after.contains_key(key))
    }

    /// Whether `key` differs inside the object both sides hold under `parent`.
    fn differs_in(&self, parent: &str, key: &str) -> bool {
        let has = |side: &Map<String, Value>| {
            side.get(parent)
                .and_then(Value::as_object)
                .is_some_and(|o| o.contains_key(key))
        };
        !matches!(self.e.op, "added" | "removed") && (has(&self.before) || has(&self.after))
    }
}

fn object(json: Option<&str>) -> Map<String, Value> {
    match json.and_then(|j| serde_json::from_str(j).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn class(p: &Parsed, viewed: &BTreeSet<&str>) -> Class {
    let (rule, category) = category(p, viewed);
    match breaking(p) {
        Some(rule) => Class {
            category,
            breaking: true,
            rule,
        },
        None => Class {
            category,
            breaking: false,
            rule,
        },
    }
}

fn category(p: &Parsed, viewed: &BTreeSet<&str>) -> (&'static str, Category) {
    use Category::{Functional, Technical};
    let op = p.e.op;
    match p.e.kind {
        "module" if matches!(op, "added" | "removed" | "renamed") => ("module", Functional),
        "module" => ("module-manifest", Technical),
        "menu" => ("menu", Functional),
        "action" => ("action", Functional),
        "view" if matches!(op, "added" | "removed") || p.differs("arch") => ("view", Functional),
        "view" => ("view-meta", Technical),
        "view_node" if VISIBLE_NODES.contains(&node_tag(p.e.qualname)) => ("arch-node-visible", Functional),
        "view_node" => ("arch-node", Technical),
        "group" | "rule" | "access" => ("access-rights", Technical),
        "record" | "data_function" | "delete" => ("data", Technical),
        "model" if op == "added" && viewed.contains(model_name(p.e.qualname)) => ("model-with-views", Functional),
        "model" => ("model", Technical),
        "field" if LABEL_ARGS.iter().any(|k| p.differs_in("args", k)) => ("field-label", Functional),
        "field" => ("field", Technical),
        "selection" => ("selection", Functional),
        "method" => ("method", Technical),
        "class" | "function" | "route" | "constraint" | "constant" => ("python", Technical),
        k if k.starts_with("js_") => ("js", Technical),
        k if k.starts_with("owl_") => ("owl", Technical),
        k if k.starts_with("scss_") || k == "css_property" => ("style", Technical),
        _ => ("other", Technical),
    }
}

/// Changes custom code written against the old side may fail on.
fn breaking(p: &Parsed) -> Option<&'static str> {
    let kind = p.e.kind;
    match p.e.op {
        "removed" if matches!(kind, "view_node" | "owl_node") => Some("arch-node-removed"),
        // Neither declares anything another file can name.
        "removed" if matches!(kind, "data_function" | "delete") => None,
        "removed" => Some("removed"),
        "renamed" => Some("renamed"),
        "moved" if p.e.old_module != p.e.module => Some("moved-module"),
        _ if matches!(kind, "method" | "function") && p.differs("signature") => Some("method-signature"),
        _ => None,
    }
}

/// `field` of `sale.view_order_form#field[@name='partner_id']@sale`.
fn node_tag(qualname: &str) -> &str {
    let node = qualname.split_once('#').map_or(qualname, |(_, n)| n);
    node.split(['[', '@']).next().unwrap_or(node)
}

/// `res.users` of `res.users@base` and of `res.users[Users]@base`.
fn model_name(qualname: &str) -> &str {
    qualname.split(['[', '@']).next().unwrap_or(qualname)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Category::{Functional, Technical};

    fn ev<'a>(kind: &'a str, qualname: &'a str, op: &'a str) -> Event<'a> {
        Event {
            kind,
            qualname,
            op,
            module: Some("sale"),
            old_module: None,
            attrs_before: None,
            attrs_after: None,
        }
    }

    fn changed<'a>(kind: &'a str, qualname: &'a str, before: &'a str, after: &'a str) -> Event<'a> {
        Event {
            attrs_before: Some(before),
            attrs_after: Some(after),
            ..ev(kind, qualname, "changed")
        }
    }

    fn one(e: Event) -> (Category, bool, &'static str) {
        let (classes, _) = commit(None, &[e]);
        let c = classes[0];
        (c.category, c.breaking, c.rule)
    }

    #[test]
    fn removed_named_view_node_is_breaking_under_its_own_rule() {
        let node = "sale.view_order_form#field[@name='partner_id']@sale";
        assert_eq!(
            one(ev("view_node", node, "removed")),
            (Functional, true, "arch-node-removed")
        );
        let anchor = "sale.view_order_form#div[@name='totals']@sale";
        assert_eq!(
            one(ev("view_node", anchor, "removed")),
            (Technical, true, "arch-node-removed")
        );
        assert_eq!(
            one(ev("view_node", node, "added")),
            (Functional, false, "arch-node-visible")
        );
        assert_eq!(one(ev("view_node", anchor, "added")), (Technical, false, "arch-node"));
        assert_eq!(
            one(ev("owl_node", "web.X#div[@t-ref='r']@web", "removed")),
            (Technical, true, "arch-node-removed")
        );
    }

    #[test]
    fn modules() {
        assert_eq!(one(ev("module", "sale", "added")), (Functional, false, "module"));
        assert_eq!(one(ev("module", "sale", "removed")), (Functional, true, "removed"));
        assert_eq!(one(ev("module", "sale", "renamed")), (Functional, true, "renamed"));
        let depends = changed(
            "module",
            "sale",
            r#"{"depends":["base"]}"#,
            r#"{"depends":["base","mail"]}"#,
        );
        assert_eq!(one(depends), (Technical, false, "module-manifest"));
    }

    #[test]
    fn menus_and_actions_are_functional() {
        assert_eq!(
            one(ev("menu", "sale.menu_root@sale", "added")),
            (Functional, false, "menu")
        );
        assert_eq!(
            one(ev("action", "sale.action_report@sale", "changed")),
            (Functional, false, "action")
        );
    }

    #[test]
    fn views_are_functional_unless_only_their_inheritance_changes() {
        assert_eq!(one(ev("view", "sale.form@sale", "added")), (Functional, false, "view"));
        let arch = changed("view", "sale.form@sale", r#"{"arch":"a"}"#, r#"{"arch":"b"}"#);
        assert_eq!(one(arch), (Functional, false, "view"));
        let priority = changed("view", "sale.form@sale", r#"{"priority":"16"}"#, r#"{"priority":"20"}"#);
        assert_eq!(one(priority), (Technical, false, "view-meta"));
    }

    #[test]
    fn access_rights_and_records_are_technical() {
        for kind in ["group", "rule", "access"] {
            assert_eq!(
                one(ev(kind, "sale.x@sale", "added")),
                (Technical, false, "access-rights")
            );
        }
        assert_eq!(
            one(ev("record", "base.state_au_1@base", "added")),
            (Technical, false, "data")
        );
        assert_eq!(
            one(ev("data_function", "res.partner#write@sale", "removed")),
            (Technical, false, "data")
        );
        assert_eq!(
            one(ev("delete", "account.tax#search@l10n_fr", "removed")),
            (Technical, false, "data")
        );
    }

    #[test]
    fn a_new_model_is_functional_only_with_a_view_of_it() {
        let model = ev("model", "x.thing@x", "added");
        assert_eq!(one(model), (Technical, false, "model"));
        let view = Event {
            attrs_after: Some(r#"{"model":"x.thing","type":"form"}"#),
            ..ev("view", "x.thing_form@x", "added")
        };
        let (classes, _) = commit(None, &[model, view]);
        assert_eq!((classes[0].category, classes[0].rule), (Functional, "model-with-views"));
        let extension = ev("model", "x.thing[Thing]@x", "added");
        assert_eq!(commit(None, &[extension, view]).0[0].rule, "model-with-views");
    }

    #[test]
    fn field_labels_are_functional_and_the_rest_technical() {
        let label = changed(
            "field",
            "sale.order#x@sale",
            r#"{"args":{"string":"A"}}"#,
            r#"{"args":{"string":"B"}}"#,
        );
        assert_eq!(one(label), (Functional, false, "field-label"));
        let help = changed(
            "field",
            "sale.order#x@sale",
            r#"{"args":{}}"#,
            r#"{"args":{"help":"h"}}"#,
        );
        assert_eq!(one(help), (Functional, false, "field-label"));
        let store = changed(
            "field",
            "sale.order#x@sale",
            r#"{"args":{}}"#,
            r#"{"args":{"store":true}}"#,
        );
        assert_eq!(one(store), (Technical, false, "field"));
        let added = Event {
            attrs_after: Some(r#"{"args":{"string":"A"}}"#),
            ..ev("field", "sale.order#x@sale", "added")
        };
        assert_eq!(one(added), (Technical, false, "field"));
        assert_eq!(
            one(ev("field", "sale.order#x@sale", "removed")),
            (Technical, true, "removed")
        );
        assert_eq!(
            one(ev("selection", "sale.order#state:draft@sale", "changed")),
            (Functional, false, "selection")
        );
    }

    #[test]
    fn signatures_break_and_bodies_do_not() {
        let signature = changed(
            "method",
            "sale.order#f@sale",
            r#"{"signature":["self"]}"#,
            r#"{"signature":["self","x"]}"#,
        );
        assert_eq!(one(signature), (Technical, true, "method-signature"));
        let body = changed("method", "sale.order#f@sale", r#"{"body":"a"}"#, r#"{"body":"b"}"#);
        assert_eq!(one(body), (Technical, false, "method"));
        let decorators = changed(
            "method",
            "sale.order#f@sale",
            "{}",
            r#"{"decorators":[{"name":"api.model"}]}"#,
        );
        assert_eq!(one(decorators), (Technical, false, "method"));
        let function = changed(
            "function",
            "odoo.addons.sale.utils.f",
            r#"{"signature":[]}"#,
            r#"{"signature":["x"]}"#,
        );
        assert_eq!(one(function), (Technical, true, "method-signature"));
    }

    #[test]
    fn moves_break_only_across_modules() {
        let across = Event {
            old_module: Some("sale_ext"),
            module: Some("sale_more"),
            ..ev("field", "sale.order#foo@sale_more", "moved")
        };
        assert_eq!(one(across), (Technical, true, "moved-module"));
        let within = Event {
            old_module: Some("sale"),
            ..ev("field", "sale.order#foo@sale", "moved")
        };
        assert_eq!(one(within), (Technical, false, "field"));
    }

    #[test]
    fn client_and_python_code_are_technical() {
        for (kind, rule) in [
            ("js_export", "js"),
            ("js_registry", "js"),
            ("owl_template", "owl"),
            ("scss_variable", "style"),
            ("css_property", "style"),
            ("function", "python"),
            ("route", "python"),
            ("def", "other"),
        ] {
            assert_eq!(one(ev(kind, "x", "added")), (Technical, false, rule), "{kind}");
        }
        assert_eq!(
            one(ev("js_export", "@web/x#y", "removed")),
            (Technical, true, "removed")
        );
        assert_eq!(
            one(ev("js_registry", "fields:x@web", "removed")),
            (Technical, true, "removed")
        );
    }

    #[test]
    fn a_commit_takes_its_modules_then_its_events_majority_then_its_tag() {
        let menu = ev("menu", "sale.m@sale", "added");
        let field = ev("field", "sale.order#x@sale", "added");
        let gone = ev("field", "sale.order#y@sale", "removed");
        let class = |tag, events: &[Event]| {
            let c = commit(tag, events).1;
            (c.category, c.breaking, c.rule)
        };
        assert_eq!(
            class(Some("FIX"), &[field, gone, menu]),
            (Some(Technical), true, "commit-majority")
        );
        assert_eq!(
            class(None, &[menu, menu, field]),
            (Some(Functional), false, "commit-majority")
        );
        assert_eq!(
            class(Some("IMP"), &[menu, field]),
            (Some(Functional), false, "commit-tag")
        );
        assert_eq!(
            class(Some("REF"), &[menu, field]),
            (Some(Technical), false, "commit-tag")
        );
        assert_eq!(class(None, &[menu, field]), (Some(Technical), false, "commit-default"));
        assert_eq!(class(Some("ADD"), &[]), (None, false, "commit-no-events"));
        let module = ev("module", "theme_x", "added");
        assert_eq!(
            class(Some("ADD"), &[module, field, field, field]),
            (Some(Functional), false, "commit-module")
        );
        let manifest = ev("module", "theme_x", "changed");
        assert_eq!(
            class(None, &[manifest, menu, field]),
            (Some(Technical), false, "commit-majority")
        );
    }
}
