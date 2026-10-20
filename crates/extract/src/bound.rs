//! What keeps one file's output linear in its size: a bound on the text a symbol may repeat, and
//! a budget on the bytes a file may give.

use crate::hash::digest;
use crate::{Symbol, SymbolTable};

/// The most bytes of input-derived text an extractor repeats as written, per copy.
pub(crate) const MAX_TEXT: usize = 256;

/// An upper estimate of a symbol's serialized bytes beyond its name and attr texts, for
/// charging the budget before a symbol is built.
pub(crate) const ENVELOPE: usize = 128;

/// The bytes `text` takes as a JSON string, escapes included, for charging the budget before a
/// symbol is built.
pub(crate) fn json_len(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX, |json| json.len())
}

/// Warnings past this many are summed up in one by `warn_capped`.
const MAX_WARNINGS: usize = 10;

/// The longest last segment `bounded` keeps.
const MAX_TAIL: usize = 64;

/// `text`, or past `MAX_TEXT` bytes `~<hash>` of it followed by its last `#`, `.` or `/`
/// segment when that is short, for name text a symbol may repeat. The segment keeps what a
/// name ends with, such as `.registry` or `#patch`, which readers recognize names by.
pub(crate) fn bounded(text: String) -> String {
    if text.len() <= MAX_TEXT {
        return text;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    let tail = text.rfind(['#', '.', '/']).map_or("", |at| &text[at..]);
    let tail = if tail.len() <= MAX_TAIL { tail } else { "" };
    format!("~{}{tail}", digest(&hasher))
}

/// The serialized bytes one file may still give, and once they run out, how many items were
/// dropped and the position of the first, an offset or a line as the extractor orders warnings.
/// A backstop for any path that would make the output grow faster than the input: past it,
/// every later item is dropped and one warning says so.
pub(crate) struct Budget {
    left: usize,
    dropped: Option<(usize, usize)>,
}

impl Budget {
    /// `per_byte` bytes for each byte of `blob`, plus `slack`.
    pub(crate) fn new(blob: &[u8], per_byte: usize, slack: usize) -> Self {
        Budget {
            left: per_byte.saturating_mul(blob.len()).saturating_add(slack),
            dropped: None,
        }
    }

    /// Spends `size` bytes, or counts the item at position `at` dropped once the budget ran out,
    /// after which every later item is dropped too.
    pub(crate) fn afford(&mut self, size: usize, at: usize) -> bool {
        if let Some((count, _)) = &mut self.dropped {
            *count += 1;
            return false;
        }
        match self.left.checked_sub(size) {
            Some(left) => {
                self.left = left;
                true
            }
            None => {
                self.dropped = Some((1, at));
                false
            }
        }
    }

    /// `afford` for `symbol`'s serialized size, at its span.
    pub(crate) fn symbol(&mut self, symbol: &Symbol) -> bool {
        self.symbol_at(symbol, symbol.span.start as usize)
    }

    /// `afford` for `symbol`'s serialized size, at position `at`.
    pub(crate) fn symbol_at(&mut self, symbol: &Symbol, at: usize) -> bool {
        let size = serde_json::to_vec(symbol).map_or(usize::MAX, |b| b.len());
        self.afford(size, at)
    }

    /// `output budget exceeded; <n> <items> dropped`, at the first dropped item's position.
    pub(crate) fn warning(&self, items: &str) -> Option<(usize, String)> {
        self.dropped
            .map(|(count, at)| (at, format!("output budget exceeded; {count} {items} dropped")))
    }
}

/// Records `warnings` in position order, past `MAX_WARNINGS` summed up in one, then the
/// budget's own warning whatever their number, since it says symbols are missing.
pub(crate) fn warn_capped(table: &mut SymbolTable, mut warnings: Vec<(usize, String)>, budget: &Budget, items: &str) {
    warnings.sort_by_key(|(at, _)| *at);
    let more = warnings.len().saturating_sub(MAX_WARNINGS);
    for (_, warning) in warnings.into_iter().take(MAX_WARNINGS) {
        table.warn(warning);
    }
    if more > 0 {
        table.warn(format!("{more} more warnings"));
    }
    if let Some((_, warning)) = budget.warning(items) {
        table.warn(warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_hashes_long_text_and_keeps_a_short_tail() {
        assert_eq!(bounded("web.FormView".into()), "web.FormView");
        let long = bounded(format!("{}.registry", "a".repeat(300)));
        assert!(long.starts_with('~') && long.ends_with(".registry"), "{long}");
        assert_eq!(long.len(), 1 + 32 + ".registry".len());
        assert_eq!(bounded(format!("{}/{}", "a".repeat(300), "b".repeat(100))).len(), 33);
    }

    #[test]
    fn capped_warnings_keep_the_budgets_own() {
        let mut budget = Budget::new(b"", 0, 0);
        assert!(!budget.afford(1, 0));
        let warnings = (0..20).rev().map(|i| (i, format!("w{i}"))).collect();
        let mut table = SymbolTable::default();
        warn_capped(&mut table, warnings, &budget, "symbols");
        let got = table.warnings();
        assert_eq!((got.len(), got[0].as_str(), got[9].as_str()), (12, "w0", "w9"));
        assert_eq!(
            got[10..],
            ["10 more warnings", "output budget exceeded; 1 symbols dropped"]
        );
    }

    #[test]
    fn budget_drops_everything_after_the_first_item_it_cannot_afford() {
        let mut budget = Budget::new(b"ab", 2, 1);
        assert!(budget.afford(4, 0));
        assert!(!budget.afford(2, 7));
        assert!(!budget.afford(0, 9));
        assert_eq!(
            budget.warning("symbols"),
            Some((7, "output budget exceeded; 2 symbols dropped".into()))
        );
    }
}
