use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

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
    pub attrs: BTreeMap<String, Value>,
}

/// The symbols of one blob, keyed by `(kind, name)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolTable {
    symbols: BTreeMap<(String, String), Symbol>,
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

    /// In `(kind, name)` order.
    pub fn iter(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.values()
    }

    /// The symbols in `(kind, name)` order as compact JSON, so equal tables give equal bytes.
    /// The blob cache stores this: a change that old bytes no longer decode under needs a store
    /// migration running `DELETE FROM blob_cache; DELETE FROM events; DELETE FROM extracted;
    /// DELETE FROM symbols; DELETE FROM extractors;`.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.symbols.values().collect::<Vec<_>>()).expect("a symbol always serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut table = SymbolTable::default();
        for symbol in serde_json::from_slice::<Vec<Symbol>>(bytes)? {
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

        let bytes = table.to_bytes();
        let decoded = SymbolTable::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, table);
        assert_eq!(decoded.to_bytes(), bytes);
    }
}
