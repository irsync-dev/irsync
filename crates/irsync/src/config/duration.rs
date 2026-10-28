use super::ConfigError;
use std::time::Duration;

/// Parse `<unsigned int>[s|m|h|d]` (no suffix = seconds).
pub fn parse(s: &str) -> Result<Duration, ConfigError> {
    let bad = || ConfigError::BadDuration(s.to_string());
    let (digits, mult) = match s.chars().last().ok_or_else(bad)? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        'd' => (&s[..s.len() - 1], 86400),
        _ => (s, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(mult).map(Duration::from_secs).ok_or_else(bad)
}

/// A duration as the config spells it: the largest of `d`, `h`, `m`, `s`
/// that divides it evenly (`1800s` is `30m`; `90s` stays `90s`).
pub fn format(d: Duration) -> String {
    let secs = d.as_secs();
    for (unit, mult) in [('d', 86400), ('h', 3600), ('m', 60)] {
        if secs != 0 && secs % mult == 0 {
            return format!("{}{unit}", secs / mult);
        }
    }
    format!("{secs}s")
}
