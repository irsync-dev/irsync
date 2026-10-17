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
