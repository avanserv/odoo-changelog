//! A module's CSV data files: one symbol per row, named by its `id` column. The reader sees the
//! bytes alone, so the header tells the rows' kind: the access files by their permission or
//! `operation` columns, record rules by `domain_force`, any other model's rows are records.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::data::{RECORD, RULE};
use crate::hash::digest;
use crate::xml::refs::Refs;
use crate::{Parts, Span, Symbol, SymbolTable, access_csv, sketch, xml};

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Marks each row continuing the record above in its `#continued` part, apart from the bytes
/// `xml::feed_field` writes.
const CONTINUED: u8 = 0x08;

/// Warnings come in line order.
pub(crate) fn extract(blob: &[u8]) -> SymbolTable {
    let mut table = SymbolTable::default();
    let mut warnings = Warnings::new();
    if let Some(sheet) = Sheet::read(blob, &mut warnings) {
        let has = |name: &str| sheet.header.iter().any(|h| field(h).0 == name);
        let bare_model_id = sheet.header.iter().any(|h| h == "model_id");
        if has("domain_force") {
            records(&sheet, RULE, &mut table, &mut warnings);
        } else if has("perm_read") || (has("operation") && bare_model_id) {
            access_csv::symbols(&sheet, &mut table, &mut warnings);
        } else {
            records(&sheet, RECORD, &mut table, &mut warnings);
        }
    }
    warnings.sort_by_key(|(line, _)| *line);
    for (_, warning) in warnings {
        table.warn(warning);
    }
    table
}

/// `(line, "line N: ...")`.
pub(crate) type Warnings = Vec<(u64, String)>;

/// A column's field and whether its cells are xmlids: `x:id` and `x/id` both name `x`, as
/// Odoo's import reads them.
pub(crate) fn field(header: &str) -> (&str, bool) {
    match header.strip_suffix(":id").or_else(|| header.strip_suffix("/id")) {
        Some(name) => (name, true),
        None => (header, false),
    }
}

/// The header and the rows of a file with an `id` column, blank rows dropped.
pub(crate) struct Sheet<'b> {
    body: &'b [u8],
    offset: usize,
    /// As written.
    pub(crate) header: Vec<String>,
    pub(crate) id_column: usize,
    pub(crate) rows: Vec<Row>,
}

pub(crate) struct Row {
    pub(crate) line: u32,
    /// Where the row starts in the body.
    at: usize,
    pub(crate) cells: Vec<String>,
}

impl<'b> Sheet<'b> {
    /// None when the first row names no `id` column, with a warning unless it is a single cell. A row with more cells than
    /// the header warns, unless the extra ones are empty: Odoo drops empty trailing cells, which
    /// a stray comma at the end of a row gives.
    fn read(blob: &'b [u8], warnings: &mut Warnings) -> Option<Self> {
        let offset = if blob.starts_with(BOM) { BOM.len() } else { 0 };
        let body = &blob[offset..];
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(body);
        let mut header: Option<(Vec<String>, usize)> = None;
        let mut rows = Vec::new();
        let mut record = csv::ByteRecord::new();
        loop {
            match reader.read_byte_record(&mut record) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    let line = e.position().map_or(1, |p| p.line());
                    warnings.push((line, format!("line {line}: {e}")));
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
            let Some((columns, _)) = &header else {
                match cells.iter().position(|c| field(c).0 == "id") {
                    Some(id_column) => header = Some((cells, id_column)),
                    // A first row of one cell is no comma-separated header, such as the `|`
                    // separated codes an install hook reads: not a file Odoo loads as data.
                    None if cells.len() == 1 => return None,
                    None => {
                        warnings.push((line.into(), format!("line {line}: no id column; rows ignored")));
                        return None;
                    }
                }
                continue;
            };
            if cells.iter().skip(columns.len()).any(|c| !c.is_empty()) {
                let warning = format!("line {line}: {} fields for {} columns", cells.len(), columns.len());
                warnings.push((line.into(), warning));
            }
            rows.push(Row { line, at, cells });
        }
        let (header, id_column) = header?;
        Some(Sheet {
            body,
            offset,
            header,
            id_column,
            rows,
        })
    }

    pub(crate) fn id<'r>(&self, row: &'r Row) -> Option<&'r str> {
        row.cells
            .get(self.id_column)
            .map(String::as_str)
            .filter(|id| !id.is_empty())
    }

    /// The first occurrence of `id` on the row's first line, or an empty span at its start, as
    /// offsets into the blob.
    pub(crate) fn span(&self, row: &Row, id: &str) -> Span {
        let line = &self.body[row.at..];
        let line = &line[..line.iter().position(|&b| b == b'\n').unwrap_or(line.len())];
        let id = id.as_bytes();
        let range = match line.windows(id.len()).position(|w| w == id) {
            Some(i) => row.at + i..row.at + i + id.len(),
            None => row.at..row.at,
        };
        Span {
            start: (range.start + self.offset) as u32,
            end: (range.end + self.offset) as u32,
        }
    }

    /// The row's non-empty cells but the id, by field, with whether each is an xmlid.
    fn fields<'r>(&self, row: &'r Row) -> impl Iterator<Item = (&str, bool, &'r str)> + use<'_, 'r> {
        self.header
            .iter()
            .zip(&row.cells)
            .enumerate()
            .filter(|(i, (_, v))| *i != self.id_column && !v.is_empty())
            .map(|(_, (h, v))| {
                let (name, is_ref) = field(h);
                (name, is_ref, v.as_str())
            })
    }
}

/// A non-empty cell but the id: its field, whether it is an xmlid, its text.
type Cell<'s> = (&'s str, bool, &'s str);

/// The rows of one id: its first row's position, its fields with later rows of the id winning and
/// the line that set each, and the rows without an id that follow it, with their lines.
struct Rows<'s> {
    line: u32,
    span: Span,
    fields: BTreeMap<&'s str, (bool, &'s str, u32)>,
    continued: Vec<(u32, Vec<Cell<'s>>)>,
}

/// Attrs as an XML record of the same fields has them: `name`, `model`, `res_model` and an
/// xmlid `model_id` in clear, and `values` hashing every field as `<field name="x" ref="..."/>`
/// or `<field name="x">...</field>` would, part by part. A row without an id continues the record
/// above, as a chart template's repartition lines do, and is hashed in the `#continued` part.
fn records(sheet: &Sheet, kind: &'static str, table: &mut SymbolTable, warnings: &mut Warnings) {
    let mut by_id: BTreeMap<&str, Rows> = BTreeMap::new();
    let mut last: Option<&str> = None;
    for row in &sheet.rows {
        let Some(id) = sheet.id(row) else {
            match last.and_then(|id| by_id.get_mut(id)) {
                Some(rows) => rows.continued.push((row.line, sheet.fields(row).collect())),
                None => warnings.push((row.line.into(), format!("line {}: row without id ignored", row.line))),
            }
            continue;
        };
        let rows = by_id.entry(id).or_insert_with(|| Rows {
            line: row.line,
            span: sheet.span(row, id),
            fields: BTreeMap::new(),
            continued: Vec::new(),
        });
        rows.fields
            .extend(sheet.fields(row).map(|(name, is_ref, v)| (name, (is_ref, v, row.line))));
        last = Some(id);
    }
    let mut spellings = xml::Spellings::default();
    by_id.keys().for_each(|id| spellings.add(id));
    for (id, rows) in &by_id {
        let mut attrs = BTreeMap::from([("declared".to_string(), Value::from("csv"))]);
        for key in ["name", "model", "res_model"] {
            if let Some((_, v, _)) = rows.fields.get(key) {
                attrs.insert(key.into(), v.trim().into());
            }
        }
        if let Some((true, v, _)) = rows.fields.get("model_id") {
            attrs.insert("model_id".into(), (*v).into());
        }
        let feed = |hasher: &mut blake3::Hasher, name: &str, is_ref: bool, v: &str| match is_ref {
            true => xml::feed_field(hasher, name, &[("ref", v)], ""),
            false => xml::feed_field(hasher, name, &[], v),
        };
        let mut parts = Parts::new();
        for (name, (is_ref, v, line)) in &rows.fields {
            let mut hasher = blake3::Hasher::new();
            feed(&mut hasher, name, *is_ref, v);
            parts.insert((*name).to_string(), (*line, digest(&hasher)));
        }
        if let Some((line, _)) = rows.continued.last() {
            let mut hasher = blake3::Hasher::new();
            for (_, row) in &rows.continued {
                hasher.update(&[CONTINUED]);
                for (name, is_ref, v) in row {
                    feed(&mut hasher, name, *is_ref, v);
                }
            }
            parts.insert("#continued".into(), (*line, digest(&hasher)));
        }
        if let Some(values) = xml::values_of(&parts) {
            attrs.insert("values".into(), values);
        }
        let mut refs = Refs::default();
        let cells = rows.fields.values().map(|&(is_ref, v, _)| (is_ref, v));
        let continued = rows
            .continued
            .iter()
            .flat_map(|(_, row)| row)
            .map(|&(_, is_ref, v)| (is_ref, v));
        for (_, v) in cells.chain(continued).filter(|(is_ref, _)| *is_ref) {
            refs.ids(v);
        }
        refs.put(&mut attrs);
        let sketch = sketch::set(xml::part_items(&parts));
        table.insert(Symbol {
            kind: kind.into(),
            name: (*id).into(),
            line: rows.line,
            span: rows.span,
            attrs,
            parts: spellings.both(id).then_some(parts),
            sketch,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::data::ACCESS;

    fn rows(table: &SymbolTable) -> Vec<(&str, &str, u32, Value)> {
        table
            .iter()
            .map(|s| {
                (
                    s.kind.as_str(),
                    s.name.as_str(),
                    s.line,
                    Value::from_iter(s.attrs.clone()),
                )
            })
            .collect()
    }

    #[test]
    fn the_header_tells_the_kind() {
        let kind = |header: &str| {
            let table = extract(format!("{header}\nx,X,m,g,1,1,1,1\n").as_bytes());
            table.iter().next().map(|s| s.kind.clone()).unwrap()
        };
        assert_eq!(
            kind("id,name,model_id:id,group_id:id,perm_read,perm_write,perm_create,perm_unlink"),
            ACCESS
        );
        assert_eq!(kind("id,name,model_id,group_id/id,operation,domain"), ACCESS);
        assert_eq!(
            kind("id,model_id/id,name,active,perm_read,perm_write,perm_create,domain_force"),
            RULE
        );
        assert_eq!(kind("id,country_id:id,name,code"), RECORD);
        assert_eq!(kind("id,name,model_id,operation"), ACCESS);
        assert_eq!(kind("id,name,model_id:id,operation"), RECORD);
    }

    #[test]
    fn a_record_hashes_its_fields_as_xml_does() {
        let csv = extract(b"\"id\",\"country_id:id\",\"name\",\"code\"\nstate_nsw,au,\"New South Wales\",NSW\n");
        assert!(csv.warnings().is_empty(), "{:?}", csv.warnings());
        let xml = crate::xml::extract(
            br#"<odoo><record id="state_nsw" model="res.country.state"><field name="code">NSW</field><field name="country_id" ref="au"/><field name="name">New South Wales</field></record></odoo>"#,
        );
        let (csv, xml) = (csv.iter().next().unwrap(), xml.iter().next().unwrap());
        assert_eq!(
            (csv.kind.as_str(), csv.attrs["name"].as_str()),
            (RECORD, Some("New South Wales"))
        );
        assert_eq!(csv.attrs["values"], xml.attrs["values"]);
    }

    #[test]
    fn a_file_of_one_column_is_no_data_file() {
        let table = extract(b"code|name|active\n01|{\"en_US\":\"x\"}|True\n");
        assert_eq!((table.iter().count(), table.warnings().len()), (0, 0));
    }

    #[test]
    fn later_rows_merge_and_rows_without_id_continue() {
        let src = "id,name,amount,repartition_line_ids/factor_percent\n,orphan,,\nt,T,1,\n,,,100\n,,,-100\nu,U,2,\n";
        let table = extract(src.as_bytes());
        assert_eq!(table.warnings(), ["line 2: row without id ignored"]);
        let without_continuation = extract(b"id,name,amount\nt,T,1\n");
        let t = &rows(&table)[0];
        assert_eq!((t.1, t.2), ("t", 3));
        assert_ne!(t.3["values"], rows(&without_continuation)[0].3["values"]);

        let merged = extract(b"id,name,amount\nt,T,1\nt,,3\n");
        let single = extract(b"id,name,amount\nt,T,3\n");
        assert_eq!(rows(&merged)[0].3, rows(&single)[0].3);
        assert_eq!(
            rows(&merged)[0].3,
            json!({"declared": "csv", "name": "T", "values": rows(&single)[0].3["values"]})
        );
    }
}
