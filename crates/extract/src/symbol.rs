use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::ops::Range;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One named thing a blob defines. `kind` belongs to exactly one extractor, and renaming one
/// changes symbol ids, so it takes a version bump.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub kind: String,
    pub name: String,
    /// Informational only: change detection ignores it, so code moving above a symbol is no event.
    pub line: u32,
    /// Byte range of the name in the blob, or of the whole definition when the name comes from the
    /// path (a manifest), for codemods; ignored by change detection like `line`.
    pub span: Span,
    pub attrs: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// The symbols of one blob, keyed by `(kind, name)`, and what the extractor could not read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolTable {
    symbols: BTreeMap<(String, String), Symbol>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct Envelope<'a> {
    symbols: Vec<&'a Symbol>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    warnings: &'a [String],
}

#[derive(Deserialize)]
struct OwnedEnvelope {
    symbols: Vec<Symbol>,
    #[serde(default)]
    warnings: Vec<String>,
}

impl SymbolTable {
    /// Keeps the first symbol of a `(kind, name)` and returns false for a later one, which is
    /// deterministic since an extractor reads the bytes in order.
    pub fn insert(&mut self, symbol: Symbol) -> bool {
        let key = (symbol.kind.clone(), symbol.name.clone());
        match self.symbols.entry(key) {
            Entry::Occupied(_) => false,
            Entry::Vacant(slot) => {
                slot.insert(symbol);
                true
            }
        }
    }

    pub fn contains(&self, kind: &str, name: &str) -> bool {
        self.symbols.contains_key(&(kind.to_owned(), name.to_owned()))
    }

    /// In `(kind, name)` order.
    pub fn iter(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.values()
    }

    /// Records what the extractor could not read as written, such as a syntax error, so that
    /// symbols may be missing.
    pub fn warn(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    /// In the order the extractor recorded them.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// `{"symbols":[...],"warnings":[...]}` as compact JSON, without `warnings` when there are
    /// none and with symbols in `(kind, name)` order, so that equal tables give equal bytes. The
    /// blob cache stores this, warnings included, so that a hit warns like a fresh parse. A change
    /// that old bytes no longer decode under needs a store migration running
    /// `DELETE FROM blob_cache; DELETE FROM events; DELETE FROM extracted; DELETE FROM symbols;
    /// DELETE FROM extractors;`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let envelope = Envelope {
            symbols: self.symbols.values().collect(),
            warnings: &self.warnings,
        };
        serde_json::to_vec(&envelope).expect("a symbol always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let envelope: OwnedEnvelope = serde_json::from_slice(bytes)?;
        let mut table = SymbolTable {
            warnings: envelope.warnings,
            ..SymbolTable::default()
        };
        for symbol in envelope.symbols {
            table.insert(symbol);
        }
        Ok(table)
    }
}

/// `blake3(kind + qualname)`, with a 0x00 byte between the two: without it, `view` + `_node.x`
/// and `view_node` + `.x` would hash alike. A kind must not contain a NUL byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId([u8; 32]);

impl SymbolId {
    pub fn new(kind: &str, qualname: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(kind.as_bytes());
        hasher.update(&[0]);
        hasher.update(qualname.as_bytes());
        SymbolId(*hasher.finalize().as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(kind: &str, name: &str, line: u32, body: &str) -> Symbol {
        Symbol {
            kind: kind.into(),
            name: name.into(),
            line,
            span: Span {
                start: line,
                end: line + 1,
            },
            attrs: BTreeMap::from([("body".into(), Value::from(body))]),
        }
    }

    #[test]
    fn symbol_id_is_stable() {
        assert_eq!(
            SymbolId::new("field", "sale.order#partner_id").to_string(),
            "45a3ad9fc2d5ff1976e5e7489523cc04bac7f4ceb0a5dac77a1bf7c88cb8ffb7"
        );
    }

    #[test]
    fn kind_and_qualname_do_not_run_together() {
        assert_ne!(SymbolId::new("view", "_node.x"), SymbolId::new("view_node", ".x"));
    }

    #[test]
    fn duplicates_keep_the_first_and_bytes_round_trip() {
        let mut table = SymbolTable::default();
        assert!(table.insert(symbol("def", "b", 1, "one")));
        assert!(table.insert(symbol("def", "a", 2, "two")));
        assert!(!table.insert(symbol("def", "b", 3, "three")));
        let names: Vec<_> = table.iter().map(|s| (s.name.as_str(), s.line)).collect();
        assert_eq!(names, [("a", 2), ("b", 1)]);
        assert!(table.contains("def", "a") && !table.contains("def", "c"));
        assert_eq!(
            String::from_utf8(table.to_bytes()).unwrap(),
            r#"{"symbols":[{"kind":"def","name":"a","line":2,"span":{"start":2,"end":3},"attrs":{"body":"two"}},{"kind":"def","name":"b","line":1,"span":{"start":1,"end":2},"attrs":{"body":"one"}}]}"#
        );
        table.warn("line 4: syntax error");

        let bytes = table.to_bytes();
        let decoded = SymbolTable::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, table);
        assert_eq!(decoded.to_bytes(), bytes);
    }
}
