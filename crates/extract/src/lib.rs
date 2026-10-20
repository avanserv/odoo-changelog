//! Turns one blob into symbols, and two symbol tables into symbol-level changes. Never sees git
//! or SQLite: the CLI hands each extractor the bytes of the blobs it claims.

mod access_csv;
mod bound;
mod data;
mod delta;
mod hash;
mod js;
mod manifest;
mod path;
mod python;
mod symbol;
mod syntax;
mod xml;

use std::borrow::Cow;
use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

pub use data::Data;
pub use delta::{Delta, Op, delta};
pub use js::Js;
pub use manifest::Manifest;
pub use path::module_of;
pub use python::Python;
pub use symbol::{Span, Symbol, SymbolId, SymbolTable};

pub trait Extractor: Send + Sync {
    /// Stored in the database; never rename.
    fn name(&self) -> &'static str;
    /// Bump on any change to what `extract`, `qualify` or `qualify_attrs` returns for some input.
    fn version(&self) -> u32;
    /// Decides from the path alone, before any blob is read.
    fn claims(&self, path: &str) -> bool;
    /// Output must depend on `blob` alone: it is cached by blob id, and one blob sits at many paths.
    fn extract(&self, blob: &[u8]) -> Result<SymbolTable>;
    /// Qualname of a symbol found in the file at `path`; uncached, so path-derived names live here.
    fn qualify(&self, _path: &str, symbol: &Symbol) -> String {
        symbol.name.clone()
    }
    /// Attrs of a symbol found in the file at `path`; uncached like `qualify`, for references whose
    /// meaning depends on the path, such as an xmlid written without its module.
    fn qualify_attrs<'a>(&self, _path: &str, symbol: &'a Symbol) -> Cow<'a, BTreeMap<String, Value>> {
        Cow::Borrowed(&symbol.attrs)
    }
}

/// Extractors in name order, which is the order their events are written in.
pub struct Registry {
    extractors: Vec<Box<dyn Extractor>>,
}

impl Registry {
    pub fn new(mut extractors: Vec<Box<dyn Extractor>>) -> Result<Self> {
        extractors.sort_by_key(|e| e.name());
        if let Some(pair) = extractors.windows(2).find(|w| w[0].name() == w[1].name()) {
            anyhow::bail!("two extractors are named {}", pair[0].name());
        }
        Ok(Registry { extractors })
    }

    /// The extractors the binary ships with.
    pub fn builtin() -> Self {
        Registry::new(vec![Box::new(Data), Box::new(Js), Box::new(Manifest), Box::new(Python)])
            .expect("builtin extractor names are unique")
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Extractor> {
        self.extractors.iter().map(|e| e.as_ref())
    }

    pub fn is_empty(&self) -> bool {
        self.extractors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(&'static str);

    impl Extractor for Named {
        fn name(&self) -> &'static str {
            self.0
        }
        fn version(&self) -> u32 {
            1
        }
        fn claims(&self, _: &str) -> bool {
            true
        }
        fn extract(&self, _: &[u8]) -> Result<SymbolTable> {
            Ok(SymbolTable::default())
        }
    }

    #[test]
    fn registry_sorts_by_name_and_rejects_duplicates() {
        let registry = Registry::new(vec![Box::new(Named("xml")), Box::new(Named("py"))]).unwrap();
        let names: Vec<_> = registry.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["py", "xml"]);

        let err = Registry::new(vec![Box::new(Named("py")), Box::new(Named("py"))])
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "two extractors are named py");
    }
}
