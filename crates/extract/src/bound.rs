//! What keeps one file's output linear in its size: a bound on the text a symbol may repeat, and
//! a budget on the bytes a file may give.

use crate::Symbol;
use crate::hash::digest;

/// The most bytes of input-derived text an extractor repeats as written, per copy.
pub(crate) const MAX_TEXT: usize = 256;

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
        let size = serde_json::to_vec(symbol).map_or(usize::MAX, |b| b.len());
        self.afford(size, symbol.span.start as usize)
    }

    /// `output budget exceeded; <n> <items> dropped`, at the first dropped item's position.
    pub(crate) fn warning(&self, items: &str) -> Option<(usize, String)> {
        self.dropped
            .map(|(count, at)| (at, format!("output budget exceeded; {count} {items} dropped")))
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
