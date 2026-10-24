//! Small sketches for telling how alike two contents are once their hashes differ: b-bit MinHash
//! over shingles of three tokens for a body, or over the items of a set. They are deterministic
//! across runs and machines, since every hash here is a fixed function of the bytes.

/// The minima kept. Two sketches estimate the Jaccard similarity of their shingle sets within
/// about 0.08 at 0.8.
const SLOTS: usize = 32;
/// Shingles below which a body is too short for its sketch to tell anything.
const MIN_SHINGLES: usize = 8;
/// The chance that two unrelated minima agree on their low byte.
const CHANCE: f64 = 1.0 / 256.0;

pub(crate) struct Sketcher {
    /// The hashes of the two tokens before the next one.
    window: [u64; 2],
    tokens: usize,
    minima: [u64; SLOTS],
}

impl Sketcher {
    pub(crate) fn new() -> Self {
        Sketcher {
            window: [0; 2],
            tokens: 0,
            minima: [u64::MAX; SLOTS],
        }
    }

    pub(crate) fn push(&mut self, token: &str) {
        let hash = fnv(token.as_bytes());
        if self.tokens >= 2 {
            let shingle = mix(self.window[0] ^ self.window[1].rotate_left(21) ^ hash.rotate_left(42));
            for (slot, min) in self.minima.iter_mut().enumerate() {
                *min = (*min).min(mix(shingle ^ SEEDS[slot]));
            }
        }
        self.window = [self.window[1], hash];
        self.tokens += 1;
    }

    /// The low byte of each minimum as hex, or None for a body under `MIN_SHINGLES` shingles.
    pub(crate) fn finish(&self) -> Option<String> {
        (self.tokens >= MIN_SHINGLES + 2).then(|| self.minima.iter().map(|m| format!("{:02x}", m & 0xff)).collect())
    }
}

/// A sketch of a set of items rather than of a run of tokens, for content whose order says little,
/// such as a record's field values or the elements of an arch; None under two distinct items.
/// Two such sketches compare with `similarity` like token sketches.
pub(crate) fn set<I: AsRef<[u8]>>(items: impl IntoIterator<Item = I>) -> Option<String> {
    let mut minima = [u64::MAX; SLOTS];
    let mut distinct = std::collections::BTreeSet::new();
    for item in items {
        let hash = fnv(item.as_ref());
        distinct.insert(hash);
        for (slot, min) in minima.iter_mut().enumerate() {
            *min = (*min).min(mix(hash ^ SEEDS[slot]));
        }
    }
    (distinct.len() >= 2).then(|| minima.iter().map(|m| format!("{:02x}", m & 0xff)).collect())
}

/// The Jaccard similarity two sketches estimate, from 0 to 1, or None when one is malformed.
pub(crate) fn similarity(a: &str, b: &str) -> Option<f64> {
    if a.len() != SLOTS * 2 || b.len() != SLOTS * 2 {
        return None;
    }
    let (a, b) = (a.as_bytes().chunks(2), b.as_bytes().chunks(2));
    let agree = a.zip(b).filter(|(x, y)| x == y).count() as f64 / SLOTS as f64;
    Some(((agree - CHANCE) / (1.0 - CHANCE)).clamp(0.0, 1.0))
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// splitmix64's finalizer.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// One seed per slot: splitmix64's sequence from 0.
const SEEDS: [u64; SLOTS] = {
    let mut seeds = [0; SLOTS];
    let mut state: u64 = 0;
    let mut i = 0;
    while i < SLOTS {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        seeds[i] = z ^ (z >> 31);
        i += 1;
    }
    seeds
};

#[cfg(test)]
mod tests {
    use super::*;

    fn sketch(text: &str) -> Option<String> {
        let mut s = Sketcher::new();
        text.split_whitespace().for_each(|t| s.push(t));
        s.finish()
    }

    #[test]
    fn alike_bodies_score_high_and_unrelated_ones_low() {
        let body =
            "for line in self . order_line : if line . qty > 0 : total += line . price * line . qty return total";
        let edited = body.replace("total += line . price", "total += line . price_unit");
        let other =
            "partner = self . env [ 'res.partner' ] . search ( [ ( 'name' , '=' , name ) ] ) return partner . id";
        let (a, b, c) = (sketch(body).unwrap(), sketch(&edited).unwrap(), sketch(other).unwrap());
        assert_eq!(similarity(&a, &a), Some(1.0));
        assert!(similarity(&a, &b).unwrap() >= 0.5, "{:?}", similarity(&a, &b));
        assert!(similarity(&a, &c).unwrap() <= 0.3, "{:?}", similarity(&a, &c));
        assert_eq!(sketch(body), sketch(body));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn set_sketches_tell_shared_items_apart_from_unrelated_ones() {
        let items: Vec<String> = (0..10).map(|i| format!("field_{i}")).collect();
        let mut edited = items.clone();
        edited[0] = "changed".into();
        let other: Vec<String> = (0..10).map(|i| format!("other_{i}")).collect();
        let (a, b, c) = (set(&items).unwrap(), set(&edited).unwrap(), set(&other).unwrap());
        assert_eq!(similarity(&a, &a), Some(1.0));
        assert!(similarity(&a, &b).unwrap() >= 0.6, "{:?}", similarity(&a, &b));
        assert!(similarity(&a, &c).unwrap() <= 0.2, "{:?}", similarity(&a, &c));
        assert_eq!(set(["one", "one"]), None);
    }

    #[test]
    fn short_bodies_have_no_sketch() {
        assert_eq!(sketch("return self . x"), None);
        assert_eq!(similarity("ab", "ab"), None);
    }
}
