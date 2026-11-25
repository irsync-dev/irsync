//! irsync's temporary names: `.irsync.<name>.part` for a file being
//! received and `.irsync.<name>.tmp` for an entry made and renamed into place at once. Every
//! place that makes or recognises one uses these functions.

/// The longest file name irsync creates, in bytes: `NAME_MAX` on Linux and macOS.
pub const NAME_MAX: usize = 255;

/// What every temporary name starts with.
const TEMP_PREFIX: &[u8] = b".irsync.";
/// The suffix of a partial file the receiver resumes into.
const PART_SUFFIX: &[u8] = b".part";
/// The suffix of an entry made under a temporary name and renamed into
/// place at once.
const TMP_SUFFIX: &[u8] = b".tmp";
/// Hex digits of the name hash in a shortened temporary name.
const TEMP_HASH_HEX: usize = 16;

/// irsync's temporary name for `name`: `.irsync.<name><suffix>`. When that
/// would exceed [`NAME_MAX`], `<name>` is cut (on a character boundary if it
/// is UTF-8) and `~` plus the first 16 hex digits of its BLAKE3 hash are put
/// before the suffix, so the result fits and two long names that share a
/// prefix still get distinct temporaries. Every temporary irsync makes
/// (`.part` and `.tmp`) comes from here, so the mapping is one place.
pub fn temp_name(name: &[u8], suffix: &[u8]) -> Vec<u8> {
    let plain = TEMP_PREFIX.len() + name.len() + suffix.len();
    let mut out = Vec::with_capacity(plain.min(NAME_MAX));
    out.extend_from_slice(TEMP_PREFIX);
    if plain <= NAME_MAX {
        out.extend_from_slice(name);
    } else {
        let keep = NAME_MAX - TEMP_PREFIX.len() - 1 - TEMP_HASH_HEX - suffix.len();
        let keep = match std::str::from_utf8(name) {
            Ok(s) => (0..=keep)
                .rev()
                .find(|&i| s.is_char_boundary(i))
                .unwrap_or(0),
            Err(_) => keep,
        };
        out.extend_from_slice(&name[..keep]);
        out.push(b'~');
        out.extend_from_slice(blake3::hash(name).to_hex()[..TEMP_HASH_HEX].as_bytes());
    }
    out.extend_from_slice(suffix);
    out
}

/// Whether a final path component is one of irsync's own temporary names
/// (`.irsync.<name>.part` / `.irsync.<name>.tmp`, possibly shortened). Such
/// names are never listed, never accepted from a peer and never created as
/// destinations.
pub fn is_temp_name(name: &[u8]) -> bool {
    name.starts_with(TEMP_PREFIX) && (name.ends_with(PART_SUFFIX) || name.ends_with(TMP_SUFFIX))
}

/// Whether a final path component has exactly the shape of a receiver's
/// partial file, `.irsync.<name>.part` (the GC removes only these).
pub(crate) fn is_part_name(name: &[u8]) -> bool {
    name.len() > TEMP_PREFIX.len() + PART_SUFFIX.len()
        && name.starts_with(TEMP_PREFIX)
        && name.ends_with(PART_SUFFIX)
}
