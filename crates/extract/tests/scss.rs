//! The SCSS extractor on hand-written fixtures. Each `fixtures/scss/<name>.scss` has its
//! expected output beside it in `<name>.json`; `ODOO_CHANGELOG_BLESS=1` rewrites those.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{batch, check, clones, find, fixtures, git, render};
use odoo_changelog_extract::{Extractor, Op, Scss, Symbol, SymbolTable, delta};
use serde_json::{Value, json};

const PATH: &str = "addons/demo/static/src/scss/v.scss";

/// `theme.scss` sits in a theme addon, the others in `demo`.
fn fixture_path(file: &Path) -> String {
    let name = file.file_name().unwrap().to_str().unwrap();
    match name {
        "theme.scss" => format!("theme_demo/static/src/scss/{name}"),
        _ => format!("addons/demo/static/src/scss/{name}"),
    }
}

fn fixture(stem: &str) -> Vec<u8> {
    let file = fixtures("scss", "scss")
        .into_iter()
        .find(|f| f.file_stem().unwrap() == stem)
        .unwrap();
    std::fs::read(file).unwrap()
}

fn extract(src: &str) -> SymbolTable {
    Scss.extract(src.as_bytes()).unwrap()
}

fn attrs(symbol: &Symbol) -> Value {
    Value::from_iter(symbol.attrs.clone())
}

#[test]
fn fixtures_match_expected_output() {
    let files = fixtures("scss", "scss");
    assert!(files.len() >= 4, "fixtures missing: {files:?}");
    for file in files {
        let blob = std::fs::read(&file).unwrap();
        check(
            &file.with_extension("json"),
            &render(&Scss, &fixture_path(&file), &blob),
        );

        let first = Scss.extract(&blob).unwrap().to_bytes();
        assert_eq!(first, Scss.extract(&blob).unwrap().to_bytes(), "{}", file.display());
        assert_eq!(SymbolTable::from_bytes(&first).unwrap().to_bytes(), first);
    }
}

#[test]
fn scss_variables_keep_default() {
    let src = "$a: 1 !default;\n$b: (x: 1,\n  y: 2,) !default; // note\n$c: 2;\n";
    let table = extract(src);
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
    let a = find(&table, "scss_variable", "$a");
    assert_eq!(attrs(a), json!({"default": true, "value": "1"}));
    assert_eq!(Scss.qualify(PATH, a), "$a@demo");
    assert_eq!(&src.as_bytes()[a.span.range()], b"$a");
    assert_eq!(
        attrs(find(&table, "scss_variable", "$b")),
        json!({"default": true, "value": "(x: 1, y: 2)"})
    );
    assert_eq!(attrs(find(&table, "scss_variable", "$c")), json!({"value": "2"}));
}

#[test]
fn conditional_and_repeated_assignments() {
    let table = Scss.extract(&fixture("theme")).unwrap();
    assert!(table.warnings().is_empty(), "{:?}", table.warnings());
    assert_eq!(
        attrs(find(&table, "scss_variable", "$o-demo-size-multiplier")),
        json!({
            "value": "(1.25 / 1.125)",
            "within": ["@if o-website-value('font') == 'Demo Serif'"],
            "reassigned": [{"value": "1", "within": ["@else if o-website-value('font') == 'Demo Mono'"]}],
        })
    );
    assert_eq!(
        attrs(find(&table, "scss_variable", "$o-demo-palettes")),
        json!({
            "value": "()",
            "reassigned": [{
                "value": "map-merge($o-demo-palettes, ($name: $palette))",
                "within": ["@each $name, $palette in $o-demo-base-palettes"],
            }],
        })
    );
    let flags = |src: &str| attrs(find(&extract(src), "scss_variable", "$x"));
    assert_eq!(flags("$x: 1 !global !default;"), flags("$x: 1!default ! global;"));
    assert_eq!(flags(".a { $x: 1 !global; }"), json!({"global": true, "value": "1"}));
    assert!(!extract(".a { $x: 1; }\n@mixin m { $x: 1; }").contains("scss_variable", "$x"));
}

#[test]
fn reformatting_scss_gives_no_event() {
    let src = String::from_utf8(fixture("mixins")).unwrap();
    let old = extract(&src);
    for reformatted in [
        src.replace("    ", "\t").replace(": ", ":  "),
        src.replace("($px / $base)", "( $px  /  $base )").replace(
            "$bottom: auto, $left: auto)",
            "$bottom: auto,\n    $left: auto, // last\n)",
        ),
        src.replace("margin: 0;", "margin: 0; /* none */"),
    ] {
        let new = extract(&reformatted);
        assert!(
            new.warnings().iter().all(|w| w.contains("duplicate")),
            "{:?}",
            new.warnings()
        );
        assert_eq!(
            delta(&Scss, Some((PATH, &old)), Some((PATH, &new))),
            [],
            "{reformatted}"
        );
    }

    let edited = extract(&src.replace("@return ($px / $base) * 1rem;", "@return $px / $base * 1rem;"));
    let changes = delta(&Scss, Some((PATH, &old)), Some((PATH, &edited)));
    let ops: Vec<(&str, &str, Op)> = changes
        .iter()
        .map(|d| (d.kind.as_str(), d.qualname.as_str(), d.op))
        .collect();
    assert_eq!(ops, [("scss_function", "o-demo-to-rem@demo", Op::Changed)]);
    let (before, after): (Value, Value) = (
        serde_json::from_str(changes[0].attrs_before.as_deref().unwrap()).unwrap(),
        serde_json::from_str(changes[0].attrs_after.as_deref().unwrap()).unwrap(),
    );
    assert_ne!(before["body"], after["body"]);
    assert_eq!(before["params"], after["params"]);

    let map = |src: &str| extract(src);
    let (a, b) = (map("$m: (a: 1, b: 2);"), map("$m: (\n  a: 1,\n  b: 2,\n);"));
    assert_eq!(delta(&Scss, Some((PATH, &a)), Some((PATH, &b))), []);
}

#[test]
fn broken_scss_gives_a_partial_table_and_a_warning() {
    let table = Scss.extract(&fixture("broken")).unwrap();
    let names: Vec<&str> = table.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["$o-demo-after", "$o-demo-kept"]);
    assert_eq!(
        table.warnings(),
        ["line 2: unmatched }", "line 4: unclosed {", "line 6: unclosed comment"]
    );
    for src in [
        "", "\u{FEFF}", "}}}", "{{{", "((((", "\"", "#{", "/*", "$", "$a:", "@mixin", "%",
    ] {
        assert!(Scss.extract(src.as_bytes()).is_ok(), "{src:?}");
    }
    assert_eq!(
        extract("$a: ;\n$b:;").warnings(),
        [
            "line 1: $a without a value ignored",
            "line 2: $b without a value ignored"
        ]
    );
}

#[test]
fn every_symbol_spans_its_name() {
    for file in fixtures("scss", "scss") {
        let blob = std::fs::read(&file).unwrap();
        for symbol in Scss.extract(&blob).unwrap().iter() {
            assert_eq!(
                &blob[symbol.span.range()],
                symbol.name.as_bytes(),
                "{}: {symbol:?}",
                file.display()
            );
            let line = blob[..symbol.span.start as usize]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            assert_eq!(symbol.line as usize, line, "{}: {symbol:?}", file.display());
        }
    }
}

/// Every flat loop stays within a rayon worker's 2 MiB stack.
#[test]
fn deep_nesting_reads_within_a_worker_stack() {
    let n = 20_000;
    let deep = |open: &str, close: &str| format!("{}{}", open.repeat(n), close.repeat(n));
    let sources = [
        deep("@if a { $x: 1; ", "}"),
        deep(".a { ", "}"),
        deep("@mixin m { ", "}"),
        format!("$a: {};", deep("(", ")")),
        format!("$a: #{{{}}};", deep("{", "}")),
        "{".repeat(n),
        "(".repeat(n),
        "}".repeat(n),
    ];
    let tables = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || sources.map(|src| Scss.extract(src.as_bytes()).map(|t| t.to_bytes().len())))
        .unwrap()
        .join()
        .unwrap();
    for table in tables {
        assert!(table.is_ok());
    }
}

/// Inputs that repeat a long text or a deep context in every entry they give, each over 1 MiB so
/// that the output budget's constant does not hide a faster growth.
#[test]
fn reassignments_and_deep_control_flow_stay_linear() {
    let long = format!("@if {} {{\n", "c".repeat(5000));
    let k = 200_000;
    let shapes = [
        format!("{long}{}}}", "$a: 1;\n".repeat(k)),
        format!("{}{}", "@if a { $x: 1; ".repeat(k / 2), "}".repeat(k / 2)),
        format!("{}{}{}", long.repeat(10), "$a: 1;\n".repeat(k), "}".repeat(10)),
        format!(
            "{}{}{}",
            "@each $i in (1, 2) {\n".repeat(200),
            "$v: $i;\n".repeat(k),
            "}".repeat(200)
        ),
        format!(
            "{long}{}}}",
            (0..k / 4)
                .map(|i| format!("@mixin m{i} {{ a: b; }}\n"))
                .collect::<String>()
        ),
    ];
    for src in shapes {
        let table = extract(&src);
        let size = table.to_bytes().len();
        let head = &src[..60];
        assert!(src.len() > 1 << 20, "{} bytes for {head}", src.len());
        assert!(size < 30 * src.len(), "{size} bytes from {} for {head}", src.len());
        assert!(
            table.warnings().iter().all(|w| w.contains("output budget exceeded")),
            "{:?} for {head}",
            table.warnings()
        );
    }
}

/// Blobs pinned so that the facts hold as the branches move: the 17.0 heads of community and
/// design-themes on 2026-10-20, and community's 12.0.
const PRIMARY_VARIABLES_17: &str = "4e1f2c632c06b2ca92eaa5db2bfd0fc52636b5e2";
const PRIMARY_VARIABLES_12: &str = "02835171794a4027c65b3e08ae89999288de4a69";
const BOOTSTRAP_VARIABLES_17: &str = "f33b804a173b6296c4b690b0cea2bf36ad5c9bd4";
const UTILS_17: &str = "c68d07314a602ffd0fe18c266ec3c9fca1197a10";
const FUNCTIONS_17: &str = "ca6fedddc21eab6043d6bfe3949a1acedf440a0a";
const BISTRO_OVERRIDDEN_17: &str = "06305361d260958a3d1f102da24f2c274fa548c3";
const ANELUSIA_OVERRIDDEN_17: &str = "c056330fcddfb406b56de86d66ce9213f9b4bb36";

#[test]
#[ignore = "reads the Odoo clones in config/odoo-changelog.toml"]
fn real_stylesheets() {
    let read = |repo: &str, oid: &str| {
        let blob = common::odoo_blob(repo, oid);
        let table = Scss.extract(&blob).unwrap();
        assert!(table.warnings().is_empty(), "{oid}: {:?}", table.warnings());
        table
    };
    let var = |table: &SymbolTable, name: &str| attrs(find(table, "scss_variable", name));

    let web = "addons/web/static/src/scss/primary_variables.scss";
    let primary = read("odoo", PRIMARY_VARIABLES_17);
    let brand = find(&primary, "scss_variable", "$o-brand-primary");
    assert_eq!(Scss.qualify(web, brand), "$o-brand-primary@web");
    assert_eq!(attrs(brand), json!({"default": true, "value": "$o-community-color"}));
    assert_eq!(
        var(&primary, "$o-line-height-base"),
        json!({"default": true, "value": "1.5"})
    );
    let grays = (1..10)
        .map(|i| format!("{i}00: $o-gray-{i}00"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        var(&primary, "$o-grays"),
        json!({"default": true, "value": format!("({grays})")})
    );
    assert_eq!(var(&primary, "$o-theme-text-colors")["default"], true);
    assert!(var(&primary, "$o-btns-bs-override").get("reassigned").is_some());
    assert_eq!(primary.iter().filter(|s| s.kind == "scss_variable").count(), 100);

    let primary_12 = read("odoo", PRIMARY_VARIABLES_12);
    assert_eq!(
        var(&primary_12, "$o-brand-primary"),
        json!({"value": "$o-community-color"})
    );

    let bootstrap = read("odoo", BOOTSTRAP_VARIABLES_17);
    let spacer = find(&bootstrap, "scss_variable", "$spacer");
    assert_eq!(
        Scss.qualify("addons/web/static/lib/bootstrap/scss/_variables.scss", spacer),
        "$spacer@web"
    );
    assert_eq!(attrs(spacer), json!({"default": true, "value": "1rem"}));
    assert_eq!(
        var(&bootstrap, "$border-radius"),
        json!({"default": true, "value": ".25rem"})
    );

    let utils = read("odoo", UTILS_17);
    let position = attrs(find(&utils, "scss_mixin", "o-position-absolute"));
    assert_eq!(
        position["params"],
        json!(["$top: auto", "$right: auto", "$bottom: auto", "$left: auto"])
    );
    assert_eq!(position["body"].as_str().unwrap().len(), 32);
    assert_eq!(
        attrs(find(&utils, "scss_mixin", "o-hover-text-color"))["params"],
        json!(["$default-color: $body-color", "$hover-color: $link-color"])
    );

    let functions = read("odoo", FUNCTIONS_17);
    let to_rem = find(&functions, "scss_function", "o-to-rem");
    assert_eq!(
        Scss.qualify("addons/web/static/src/scss/functions.scss", to_rem),
        "o-to-rem@web"
    );
    assert_eq!(attrs(to_rem)["params"], json!(["$-px-value", "$-base: 16"]));

    let bistro = read("design-themes", BISTRO_OVERRIDDEN_17);
    let spacer = find(&bistro, "scss_variable", "$spacer");
    assert_eq!(
        Scss.qualify("theme_bistro/static/src/scss/bootstrap_overridden.scss", spacer),
        "$spacer@theme_bistro"
    );
    assert_eq!(attrs(spacer), json!({"default": true, "value": "1.25rem"}));
    assert_eq!(var(&bistro, "$enable-shadows"), json!({"value": "true"}));
    assert_eq!(
        var(&bistro, "$o-theme-font-size-lg-multiplier"),
        json!({
            "value": "(1.25 / 1.125)",
            "within": ["@if o-website-value('font') == 'Abel'"],
            "reassigned": [{"value": "(1.25 / 1.125)", "within": ["@else if o-website-value('font') == 'Dosis'"]}],
        })
    );

    let anelusia = read("design-themes", ANELUSIA_OVERRIDDEN_17);
    assert_eq!(var(&anelusia, "$border-radius"), json!({"default": true, "value": "0"}));
    assert_eq!(
        var(&anelusia, "$box-shadow"),
        json!({"default": true, "value": "0 0 .3125rem rgba(0, 0, 0, .25)"})
    );
}

/// Every claimed stylesheet at the tips reads, deterministically, with spans inside the blob
/// and without warnings other than duplicates.
#[test]
#[ignore = "reads every Odoo clone in config/odoo-changelog.toml"]
fn every_claimed_stylesheet_at_the_tips_reads() {
    let mut read = 0;
    for (repo, clone) in clones() {
        let refs = git(
            &clone,
            &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin/"],
        );
        let refs = String::from_utf8(refs).unwrap();
        let mut blobs = BTreeMap::new();
        for branch in ["12.0", "14.0", "16.0", "17.0", "master"] {
            let rev = format!("origin/{branch}");
            if !refs.lines().any(|r| r == rev) {
                continue;
            }
            let tree = git(&clone, &["ls-tree", "-r", "-z", &rev]);
            for line in String::from_utf8(tree).unwrap().split_terminator('\0') {
                let (meta, path) = line.split_once('\t').unwrap();
                if Scss.claims(path) {
                    let oid = meta.rsplit(' ').next().unwrap().to_string();
                    blobs.entry(oid).or_insert_with(|| format!("{rev}:{path}"));
                }
            }
        }
        let oids: Vec<&str> = blobs.keys().map(String::as_str).collect();
        let (mut kinds, mut warned) = (BTreeMap::<String, usize>::new(), Vec::new());
        for (blob, (oid, at)) in batch(&clone, &oids).into_iter().zip(&blobs) {
            let table = Scss.extract(&blob).unwrap();
            assert_eq!(table.to_bytes(), Scss.extract(&blob).unwrap().to_bytes(), "{repo} {at}");
            for symbol in table.iter() {
                *kinds.entry(symbol.kind.clone()).or_default() += 1;
                assert!(
                    symbol.span.start <= symbol.span.end && symbol.span.end as usize <= blob.len(),
                    "{repo} {at} {oid}"
                );
            }
            if !table.warnings().is_empty() {
                warned.push(format!("{repo} {at}: {:?}", table.warnings()));
            }
        }
        for warning in warned.iter().take(20) {
            println!("{warning}");
        }
        println!(
            "{repo}: {} files, {} with warnings, {kinds:?}",
            blobs.len(),
            warned.len()
        );
        // design-themes' compatibility file declares one mixin twice; anything else is a bug.
        let unexpected: Vec<&String> = warned.iter().filter(|w| !w.contains(": duplicate scss_")).collect();
        assert!(unexpected.is_empty(), "{repo}: {unexpected:?}");
        read += blobs.len();
    }
    assert!(read > 0);
}
