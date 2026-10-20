//! `ir.model.access.csv` and master's `ir.access.csv`: one `access` symbol per row, named by its
//! `id` column, with every other cell in clear.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::data::ACCESS;
use crate::{Span, Symbol, SymbolTable};

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// The header names a column as Odoo's import does: `model_id:id` and `model_id/id` both give
/// `model_id`, while a bare `model_id`, which in `ir.access.csv` holds a model name rather than an
/// xmlid, gives `model`. A later row of an id replaces the earlier one.
pub(crate) fn extract(blob: &[u8]) -> SymbolTable {
    let offset = if blob.starts_with(BOM) { BOM.len() } else { 0 };
    let body = &blob[offset..];
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(body);
    let mut table = SymbolTable::default();
    let mut header: Option<(Vec<String>, usize)> = None;
    let mut rows: BTreeMap<String, Symbol> = BTreeMap::new();
    let mut record = csv::ByteRecord::new();
    loop {
        match reader.read_byte_record(&mut record) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                let line = e.position().map_or(1, |p| p.line());
                table.warn(format!("line {line}: {e}"));
                break;
            }
        }
        if record.iter().all(<[u8]>::is_empty) {
            continue;
        }
        let position = record.position().expect("a record read from a reader has a position");
        // The reader positions a record at the end of the one before, ahead of line breaks.
        let at = position.byte() as usize;
        let at = at + body[at..].iter().take_while(|b| matches!(b, b'\r' | b'\n')).count();
        let line = body[..at].iter().filter(|&&b| b == b'\n').count() as u32 + 1;
        let cells: Vec<String> = record.iter().map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        let Some((columns, id_column)) = &header else {
            let columns: Vec<String> = cells.iter().map(|c| column(c)).collect();
            match columns.iter().position(|c| c == "id") {
                Some(id_column) => header = Some((columns, id_column)),
                None => {
                    table.warn(format!("line {line}: no id column; rows ignored"));
                    return table;
                }
            }
            continue;
        };
        // Odoo drops empty trailing cells, which a stray comma at the end of a row gives.
        if cells.iter().skip(columns.len()).any(|c| !c.is_empty()) {
            table.warn(format!(
                "line {line}: {} fields for {} columns",
                cells.len(),
                columns.len()
            ));
        }
        let Some(id) = cells.get(*id_column).filter(|id| !id.is_empty()) else {
            table.warn(format!("line {line}: row without id ignored"));
            continue;
        };
        let mut attrs: BTreeMap<String, Value> = columns
            .iter()
            .zip(&cells)
            .filter(|(c, v)| *c != "id" && !v.is_empty())
            .map(|(c, v)| (c.clone(), Value::from(v.as_str())))
            .collect();
        attrs.insert("declared".into(), "csv".into());
        let span = id_span(body, at, id.as_bytes());
        let span = Span {
            start: (span.start + offset) as u32,
            end: (span.end + offset) as u32,
        };
        rows.entry(id.clone())
            .and_modify(|first| first.attrs = attrs.clone())
            .or_insert_with(|| Symbol {
                kind: ACCESS.into(),
                name: id.clone(),
                line,
                span,
                attrs,
            });
    }
    for symbol in rows.into_values() {
        table.insert(symbol);
    }
    table
}

fn column(header: &str) -> String {
    match header.strip_suffix(":id").or_else(|| header.strip_suffix("/id")) {
        Some(name) => name.to_string(),
        None if header == "model_id" => "model".to_string(),
        None => header.to_string(),
    }
}

/// The first occurrence of `id` on the record's first line, or an empty span at its start.
fn id_span(body: &[u8], at: usize, id: &[u8]) -> std::ops::Range<usize> {
    let line = &body[at..];
    let line = &line[..line.iter().position(|&b| b == b'\n').unwrap_or(line.len())];
    match line.windows(id.len()).position(|w| w == id) {
        Some(i) => at + i..at + i + id.len(),
        None => at..at,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const HEADER: &str = "id,name,model_id:id,group_id:id,perm_read,perm_write,perm_create,perm_unlink";

    fn rows(table: &SymbolTable) -> Vec<(&str, u32, Value)> {
        table
            .iter()
            .map(|s| (s.name.as_str(), s.line, Value::from_iter(s.attrs.clone())))
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
                    json!({"declared": "csv", "name": "a, the first", "model_id": "model_a", "group_id": "base.group_user", "perm_read": "1", "perm_write": "0", "perm_create": "0", "perm_unlink": "0"})
                ),
                (
                    "access_b",
                    4,
                    json!({"declared": "csv", "name": "b", "model_id": "model_b", "perm_read": "1", "perm_write": "1", "perm_create": "1", "perm_unlink": "1"})
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
            json!({"declared": "csv", "name": "R", "model": "sale.order", "group_id": "base.group_portal", "operation": "r", "domain": "[('a','=',1)]"})
        );
    }

    #[test]
    fn duplicates_extra_and_short_rows() {
        let src = format!(
            "\u{FEFF}{HEADER}\nx,first,m,,1,0,0,0\nx,second,m,,1,1,0,0\ny,Y,m,,1,0,0,0,extra\nz,Z,m\nw,W,m,,1,0,0,0,\n,no id,m\n"
        );
        let table = extract(src.as_bytes());
        assert_eq!(
            table.warnings(),
            ["line 4: 9 fields for 8 columns", "line 7: row without id ignored"]
        );
        let rows = rows(&table);
        assert_eq!(rows.len(), 4);
        assert_eq!((rows[1].0, rows[1].1, &rows[1].2["name"]), ("x", 2, &json!("second")));
        assert_eq!(rows[3].2, json!({"declared": "csv", "name": "Z", "model_id": "m"}));
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
