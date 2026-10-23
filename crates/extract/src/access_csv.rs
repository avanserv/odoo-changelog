//! `ir.model.access.csv` and master's `ir.access.csv`: one `access` symbol per row, named by its
//! `id` column, with every other cell in clear.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::data::ACCESS;
use crate::data_csv::{Sheet, Warnings, field};
use crate::xml::refs::Refs;
use crate::{Symbol, SymbolTable};

/// The header names a column as Odoo's import does: `model_id:id` and `model_id/id` both give
/// `model_id`, while a bare `model_id`, which in `ir.access.csv` holds a model name rather than an
/// xmlid, gives `model`. A later row of an id replaces the earlier one.
pub(crate) fn symbols(sheet: &Sheet, table: &mut SymbolTable, warnings: &mut Warnings) {
    let columns: Vec<&str> = sheet.header.iter().map(|h| column(h)).collect();
    let mut rows: BTreeMap<&str, Symbol> = BTreeMap::new();
    for row in &sheet.rows {
        let Some(id) = sheet.id(row) else {
            warnings.push((row.line.into(), format!("line {}: row without id ignored", row.line)));
            continue;
        };
        let mut attrs: BTreeMap<String, Value> = columns
            .iter()
            .zip(&row.cells)
            .filter(|(c, v)| **c != "id" && !v.is_empty())
            .map(|(c, v)| ((*c).to_string(), Value::from(v.as_str())))
            .collect();
        attrs.insert("declared".into(), "csv".into());
        let mut refs = Refs::default();
        for (_, v) in columns
            .iter()
            .zip(&row.cells)
            .filter(|(c, _)| matches!(**c, "model_id" | "group_id"))
        {
            refs.ids(v);
        }
        refs.put(&mut attrs);
        rows.entry(id)
            .and_modify(|first| first.attrs = attrs.clone())
            .or_insert_with(|| Symbol {
                kind: ACCESS.into(),
                name: id.into(),
                line: row.line,
                span: sheet.span(row, id),
                attrs,
                parts: None,
            });
    }
    for symbol in rows.into_values() {
        table.insert(symbol);
    }
}

fn column(header: &str) -> &str {
    match field(header) {
        (name, true) => name,
        ("model_id", false) => "model",
        (name, false) => name,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::data_csv::extract;
    use crate::{Symbol, SymbolTable};
    use serde_json::Value;

    const HEADER: &str = "id,name,model_id:id,group_id:id,perm_read,perm_write,perm_create,perm_unlink";

    fn rows(table: &SymbolTable) -> Vec<(&str, u32, Value)> {
        table
            .iter()
            .map(|s: &Symbol| (s.name.as_str(), s.line, Value::from_iter(s.attrs.clone())))
            .collect()
    }

    #[test]
    fn crlf_quotes_and_blank_lines() {
        let src = format!(
            "{HEADER}\r\naccess_a,\"a, the first\",model_a,base.group_user,1,0,0,0\r\n\r\naccess_b,b,model_b,,1,1,1,1\r\n"
        );
        let table = extract(src.as_bytes());
        assert!(table.warnings().is_empty(), "{:?}", table.warnings());
        assert_eq!(
            rows(&table),
            [
                (
                    "access_a",
                    2,
                    json!({"declared": "csv", "name": "a, the first", "model_id": "model_a", "group_id": "base.group_user", "perm_read": "1", "perm_write": "0", "perm_create": "0", "perm_unlink": "0", "references": ["base.group_user", "model_a"]})
                ),
                (
                    "access_b",
                    4,
                    json!({"declared": "csv", "name": "b", "model_id": "model_b", "perm_read": "1", "perm_write": "1", "perm_create": "1", "perm_unlink": "1", "references": ["model_b"]})
                ),
            ]
        );
        let a = table.iter().next().unwrap();
        assert_eq!(&src.as_bytes()[a.span.range()], b"access_a");
    }

    #[test]
    fn both_id_suffixes_and_the_ir_access_header() {
        let colon = extract(format!("{HEADER}\nx,X,m,g,1,1,1,1\n").as_bytes());
        let slash = extract(format!("{}\nx,X,m,g,1,1,1,1\n", HEADER.replace(":id", "/id")).as_bytes());
        assert_eq!(rows(&colon), rows(&slash));
        assert_eq!(rows(&colon)[0].2["model_id"], "m");

        let src =
            "id,name,model_id,group_id/id,operation,domain\nr,R,sale.order,base.group_portal,r,\"[('a','=',1)]\"\n";
        assert_eq!(
            rows(&extract(src.as_bytes()))[0].2,
            json!({"declared": "csv", "name": "R", "model": "sale.order", "group_id": "base.group_portal", "operation": "r", "domain": "[('a','=',1)]", "references": ["base.group_portal"]})
        );
    }

    #[test]
    fn duplicates_extra_and_short_rows() {
        let src = format!(
            "\u{FEFF}{HEADER}\nx,first,m,,1,0,0,0\nx,second,m,,1,1,0,0\n,no id,m\ny,Y,m,,1,0,0,0,extra\nz,Z,m\nw,W,m,,1,0,0,0,\n"
        );
        let table = extract(src.as_bytes());
        assert_eq!(
            table.warnings(),
            ["line 4: row without id ignored", "line 5: 9 fields for 8 columns"]
        );
        let rows = rows(&table);
        assert_eq!(rows.len(), 4);
        assert_eq!((rows[1].0, rows[1].1, &rows[1].2["name"]), ("x", 2, &json!("second")));
        assert_eq!(
            rows[3].2,
            json!({"declared": "csv", "name": "Z", "model_id": "m", "references": ["m"]})
        );
        let x = table.iter().nth(1).unwrap();
        assert_eq!(&src.as_bytes()[x.span.range()], b"x");
    }

    #[test]
    fn a_headerless_file_gives_nothing() {
        let table = extract(b"access_x,x,model_x,base.group_user,1,0,0,0\n");
        assert_eq!(table.iter().count(), 0);
        assert_eq!(table.warnings(), ["line 1: no id column; rows ignored"]);
    }
}
