/// The first 16 bytes of `hasher`'s digest as lowercase hex: the 32-character hashes attrs hold.
pub(crate) fn digest(hasher: &blake3::Hasher) -> String {
    hasher.finalize().as_bytes()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
