//! Names of servers and clients: one rule for both tables.

use crate::identity::parse_endpoint_id;

/// A name is non-empty, has no `:` or `/`, is not itself an EndpointId,
/// and is neither `all` nor `*` (the words that mean every peer).
pub fn valid_alias(s: &str) -> bool {
    !s.is_empty()
        && !s.contains([':', '/'])
        && s != "all"
        && s != "*"
        && parse_endpoint_id(s).is_err()
}

/// A machine's host name as an alias: `raw` up to its first `.`, when
/// that says something about the machine. `None` for a name that is not
/// a valid alias, for `localhost`, for one made of digits only (the start
/// of an IPv4 address), and for one with control characters.
pub fn short_host_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    let name = name.split_once('.').map_or(name, |(first, _)| first).trim();
    let usable = valid_alias(name)
        && !name.eq_ignore_ascii_case("localhost")
        && !name.bytes().all(|b| b.is_ascii_digit())
        && !name.chars().any(char::is_control);
    usable.then(|| name.to_string())
}

/// The message for a server or client name that [`valid_alias`] refuses.
pub(crate) fn invalid_name(name: &str) -> String {
    format!(
        "invalid name {name:?}: must be non-empty, contain no ':' or '/', and be neither all, * nor an EndpointId"
    )
}
