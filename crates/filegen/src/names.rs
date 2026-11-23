//! Name generation: weighted name classes, NFC, fold-unique per directory.

use rand::distr::weighted::WeightedIndex;
use rand::RngExt;
use rand_chacha::ChaCha8Rng;
use unicode_normalization::UnicodeNormalization;

use crate::config::NamesConfig;
use crate::model::{Model, NodeId};
use crate::platform;
use crate::wtf8;

/// Attempts per [`draw_name`] call before giving up.
const MAX_ATTEMPTS: usize = 64;

/// Characters of a plain word.
const PLAIN: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
/// Padding for long names.
const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

/// Latin letters with diacritics (precomposed).
const LATIN: &[char] = &[
    'à', 'á', 'â', 'ä', 'ã', 'å', 'æ', 'ç', 'è', 'é', 'ê', 'ë', 'ì', 'í', 'î', 'ï', 'ñ', 'ò', 'ó',
    'ô', 'ö', 'õ', 'ø', 'ù', 'ú', 'û', 'ü', 'ý', 'ÿ', 'ß', 'Ä', 'Ö', 'Ü', 'É', 'Å', 'Ç', 'ł', 'ś',
    'ż', 'č', 'ř', 'š', 'ž', 'ő', 'ű',
];
/// CJK ideographs and kana.
const CJK: &[char] = &[
    '日', '本', '語', '中', '文', '字', '漢', '東', '京', '北', '山', '川', '水', '火', '木', '金',
    '土', '月', '人', '学', 'あ', 'い', 'う', 'か', 'の', 'が', 'ア', 'カ', 'ガ',
];
/// Emoji, astral and BMP.
const EMOJI: &[char] = &[
    '😀', '🎉', '🚀', '🐍', '🦀', '🌍', '🔥', '✨', '📁', '💾', '🍕', '🎵',
];
/// Reserved device names for edge names: valid names on NTFS, which a path
/// not in the extended form (`\\?\`) may take for the device.
const DEVICES: &[&str] = &["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"];
/// Shell-awkward characters for the awkward class.
const SHELL: &[char] = &['$', '`', '\''];

/// The class a generated name was drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NameClass {
    Plain,
    Spaces,
    Dotfile,
    Unicode,
    Long,
    Awkward,
    /// A name that is valid on this platform but awkward there (see
    /// [`edge`]); drawn only where `platform::EDGE_NAMES` is set.
    Edge,
}

/// Draw order for the class weights; matches the `NameClassWeights` fields.
const CLASSES: [NameClass; 6] = [
    NameClass::Plain,
    NameClass::Spaces,
    NameClass::Dotfile,
    NameClass::Unicode,
    NameClass::Long,
    NameClass::Awkward,
];

/// Draw a new name for an entry in `dir`, as its stored bytes: NFC, at
/// most `max_bytes` bytes and `cfg.max_len` units as the platform counts a
/// name ([`platform::name_units`]), never `.`/`..`, no `/` or NUL, not
/// starting with `.fileset`, and unique under [`Model::fold`] among `dir`'s
/// children. Each attempt redraws the class too; `None` after 64 rejected
/// attempts (or if every class weight is zero).
pub fn draw_name(
    rng: &mut ChaCha8Rng,
    cfg: &NamesConfig,
    model: &Model,
    dir: NodeId,
    max_bytes: usize,
) -> Option<(Vec<u8>, NameClass)> {
    let w = &cfg.classes;
    let index =
        WeightedIndex::new([w.plain, w.spaces, w.dotfile, w.unicode, w.long, w.awkward]).ok()?;
    let max_len = cfg.max_len as usize;
    let limit = max_len.min(max_bytes);
    for _ in 0..MAX_ATTEMPTS {
        // Where edge names are off (Unix) this draws nothing, so the names
        // are the ones the seed always gave there.
        if platform::EDGE_NAMES && rng.random::<f64>() < cfg.edge_rate {
            let name = edge(rng, cfg);
            let text = String::from_utf8_lossy(&name);
            // U+FFFD has three bytes, as an unpaired surrogate does, so the
            // text has the length of the stored name.
            if acceptable(&text, max_len, max_bytes) && free(model, dir, &name) {
                return Some((name, NameClass::Edge));
            }
            continue;
        }
        let class = CLASSES[rng.sample(&index)];
        let name: String = build(rng, cfg, class, limit).nfc().collect();
        if acceptable(&name, max_len, max_bytes) && !model.has_folded_name(dir, &Model::fold(&name))
        {
            return Some((name.into_bytes(), class));
        }
    }
    None
}

/// The structural rules, independent of siblings: at most `max_len` units
/// as the platform counts them, and at most `max_bytes` stored bytes.
fn acceptable(name: &str, max_len: usize, max_bytes: usize) -> bool {
    !name.is_empty()
        && platform::name_units(name) <= max_len
        && name.len() <= max_bytes
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
        && !name.as_bytes().starts_with(b".fileset")
        && platform::name_ok(name)
}

/// Raw (pre-NFC) content for one class.
fn build(rng: &mut ChaCha8Rng, cfg: &NamesConfig, class: NameClass, limit: usize) -> String {
    match class {
        NameClass::Plain => plain(rng, cfg),
        NameClass::Spaces => {
            let n = rng.random_range(2..=3);
            let words: Vec<String> = (0..n).map(|_| word(rng)).collect();
            words.join(" ") + &extension(rng, cfg)
        }
        NameClass::Dotfile => format!(".{}", plain(rng, cfg)),
        NameClass::Unicode => {
            let n = rng.random_range(1..=12);
            (0..n)
                .map(|_| {
                    let alphabet = [LATIN, CJK, EMOJI][rng.random_range(0..3)];
                    alphabet[rng.random_range(0..alphabet.len())]
                })
                .collect()
        }
        NameClass::Long => {
            // 200..=255 bytes, or exactly the cap when it is below 200.
            let target = if limit < 200 {
                limit
            } else {
                rng.random_range(200..=limit.min(255))
            };
            let mut base = word(rng);
            let ext = extension(rng, cfg);
            while base.len() + ext.len() < target {
                base.push(LOWER[rng.random_range(0..LOWER.len())] as char);
            }
            let mut name = base + &ext;
            // The extension may be multi-byte; cut on a char boundary.
            while name.len() > target {
                name.pop();
            }
            name
        }
        NameClass::Awkward => match rng.random_range(0..4) {
            0 => format!("-{}", plain(rng, cfg)),
            1 => format!("{} ", plain(rng, cfg)),
            2 => format!("{}.", plain(rng, cfg)),
            _ => {
                let mut w = word(rng);
                let c = SHELL[rng.random_range(0..SHELL.len())];
                w.insert(rng.random_range(0..=w.len()), c);
                w + &extension(rng, cfg)
            }
        },
        NameClass::Edge => unreachable!("an edge name is never drawn from the class weights"),
    }
}

/// A name that is valid on Windows but awkward there, as its stored
/// (WTF-8) bytes; one of two kinds, drawn uniformly: a word with one
/// unpaired surrogate inserted at a drawn position, or a reserved device
/// name, with a drawn extension half the time.
pub(crate) fn edge(rng: &mut ChaCha8Rng, cfg: &NamesConfig) -> Vec<u8> {
    if rng.random::<bool>() {
        let mut units: Vec<u16> = word(rng).encode_utf16().collect();
        let at = rng.random_range(0..=units.len());
        units.insert(at, rng.random_range(0xD800..=0xDFFF));
        // The word is ASCII, so the surrogate has no partner next to it.
        wtf8::encode(&units)
    } else {
        let device = DEVICES[rng.random_range(0..DEVICES.len())];
        let ext = if rng.random::<bool>() {
            extension(rng, cfg)
        } else {
            String::new()
        };
        (device.to_string() + &ext).into_bytes()
    }
}

/// Whether no child of `dir` takes the stored name `name`: neither the
/// model's own key for those bytes nor the fold of their text (an unpaired
/// surrogate read as U+FFFD), so a name differing only by case or
/// normalization from one already there is refused too.
fn free(model: &Model, dir: NodeId, name: &[u8]) -> bool {
    !model.has_folded_name(dir, &Model::fold_bytes(name))
        && !model.has_folded_name(dir, &Model::fold(&String::from_utf8_lossy(name)))
}

/// A word plus an extension from the list.
fn plain(rng: &mut ChaCha8Rng, cfg: &NamesConfig) -> String {
    word(rng) + &extension(rng, cfg)
}

/// 1-24 characters of `[a-z0-9_-]`.
fn word(rng: &mut ChaCha8Rng) -> String {
    let n = rng.random_range(1..=24);
    (0..n)
        .map(|_| PLAIN[rng.random_range(0..PLAIN.len())] as char)
        .collect()
}

/// `.ext` for a drawn extension, or nothing for `""` or an empty list.
fn extension(rng: &mut ChaCha8Rng, cfg: &NamesConfig) -> String {
    if cfg.extensions.is_empty() {
        return String::new();
    }
    match cfg.extensions[rng.random_range(0..cfg.extensions.len())].as_str() {
        "" => String::new(),
        e => format!(".{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NameClassWeights;
    use crate::model::NodeKind;
    use rand::SeedableRng;
    use std::collections::BTreeSet;
    use unicode_normalization::UnicodeNormalization;

    /// The text of a drawn name: the name itself where it is UTF-8 (every
    /// name on Unix), with U+FFFD for an unpaired surrogate otherwise.
    fn text(n: &[u8]) -> String {
        String::from_utf8_lossy(n).into_owned()
    }

    /// The rules every drawn name keeps: at most `max_len` units as the
    /// platform counts them and at most `max_bytes` bytes (on Unix both
    /// count bytes).
    fn assert_valid(n: &[u8], max_len: usize, max_bytes: usize) {
        let n = &text(n);
        assert!(!n.is_empty(), "empty name");
        assert!(!n.contains('/') && !n.contains('\0'), "{n:?}");
        assert!(n != "." && n != "..", "{n:?}");
        assert!(!n.starts_with(".fileset"), "{n:?}");
        assert!(n.len() <= max_bytes, "{n:?} longer than {max_bytes} bytes");
        assert!(
            platform::name_units(n) <= max_len,
            "{n:?} longer than {max_len}"
        );
        assert_eq!(n, &n.nfc().collect::<String>());
    }

    /// Only `class`, and no edge names (which only some platforms draw).
    fn only(class: NameClass) -> NamesConfig {
        let w = |c| if c == class { 1.0 } else { 0.0 };
        NamesConfig {
            classes: NameClassWeights {
                plain: w(NameClass::Plain),
                spaces: w(NameClass::Spaces),
                dotfile: w(NameClass::Dotfile),
                unicode: w(NameClass::Unicode),
                long: w(NameClass::Long),
                awkward: w(NameClass::Awkward),
            },
            edge_rate: 0.0,
            ..NamesConfig::default()
        }
    }

    #[test]
    fn names_respect_rules_and_are_unique() {
        let cfg = NamesConfig::default();
        let mut m = Model::new();
        let r = m.root();
        let mut rng = ChaCha8Rng::seed_from_u64(9);
        let mut classes = BTreeSet::new();
        for _ in 0..2000 {
            let (n, c) = draw_name(&mut rng, &cfg, &m, r, 255).unwrap();
            classes.insert(c);
            let t = text(&n);
            assert!(!t.contains('/') && !t.contains('\0') && t != "." && t != "..");
            assert!(!t.starts_with(".fileset"));
            assert!(n.len() <= 255);
            assert_eq!(t, t.nfc().collect::<String>());
            assert!(!m.has_folded_name(r, &Model::fold(&t)));
            m.insert(r, n, NodeKind::File, 0, 0);
        }
        // The six classes, and edge names where the platform draws them.
        assert_eq!(classes.len(), 6 + usize::from(platform::EDGE_NAMES));
    }

    #[test]
    fn names_honour_max_bytes() {
        let cfg = NamesConfig::default();
        let mut m = Model::new();
        let r = m.root();
        let mut rng = ChaCha8Rng::seed_from_u64(10);
        let mut classes = BTreeSet::new();
        for _ in 0..500 {
            let (n, c) = draw_name(&mut rng, &cfg, &m, r, 10).unwrap();
            classes.insert(c);
            assert_valid(&n, 255, 10);
            assert!(!m.has_folded_name(r, &Model::fold(&text(&n))));
            m.insert(r, n, NodeKind::File, 0, 0);
        }
        // Long still occurs: it shrinks to the cap.
        assert!(classes.contains(&NameClass::Long));
    }

    #[test]
    fn names_honour_cfg_max_len() {
        let cfg = NamesConfig {
            max_len: 40,
            ..NamesConfig::default()
        };
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(11);
        for _ in 0..500 {
            let (n, _) = draw_name(&mut rng, &cfg, &m, m.root(), 255).unwrap();
            assert_valid(&n, 40, 255);
        }
    }

    #[test]
    fn long_names_are_long() {
        let cfg = only(NameClass::Long);
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(12);
        for _ in 0..200 {
            let (n, c) = draw_name(&mut rng, &cfg, &m, m.root(), 255).unwrap();
            assert_eq!(c, NameClass::Long);
            assert_valid(&n, 255, 255);
            assert!((200..=255).contains(&n.len()), "{}", n.len());
        }
    }

    #[test]
    fn unicode_names_are_non_ascii() {
        let cfg = only(NameClass::Unicode);
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(13);
        for _ in 0..200 {
            let (n, _) = draw_name(&mut rng, &cfg, &m, m.root(), 255).unwrap();
            assert_valid(&n, 255, 255);
            let n = text(&n);
            assert!(!n.is_ascii(), "{n:?}");
            assert!((1..=12).contains(&n.chars().count()));
        }
    }

    #[test]
    fn class_shapes() {
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(16);
        let mut draw = |class| {
            let (n, _) = draw_name(&mut rng, &only(class), &m, m.root(), 255).unwrap();
            String::from_utf8(n).unwrap()
        };
        for _ in 0..200 {
            let n = draw(NameClass::Spaces);
            assert!(n.contains(' '), "{n:?}");
            let n = draw(NameClass::Dotfile);
            assert!(n.starts_with('.'), "{n:?}");
            let n = draw(NameClass::Awkward);
            assert!(
                n.starts_with('-')
                    || n.ends_with(' ')
                    || n.ends_with('.')
                    || n.contains(['$', '`', '\'']),
                "{n:?}"
            );
            let n = draw(NameClass::Plain);
            assert!(n
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b)));
        }
    }

    #[test]
    fn fold_collision_is_redrawn() {
        // Long names capped at one byte: 38 possible names. The model holds
        // the upper-case form of each name drawn so far, so a fold-blind
        // generator would soon repeat one.
        let cfg = NamesConfig {
            max_len: 1,
            ..only(NameClass::Long)
        };
        let mut m = Model::new();
        let r = m.root();
        let mut rng = ChaCha8Rng::seed_from_u64(14);
        let mut seen = BTreeSet::new();
        for _ in 0..30 {
            let (n, _) = draw_name(&mut rng, &cfg, &m, r, 255).unwrap();
            assert_valid(&n, 1, 255);
            let n = String::from_utf8(n).unwrap();
            assert!(seen.insert(Model::fold(&n)), "duplicate {n:?}");
            m.insert(r, n.to_uppercase().into_bytes(), NodeKind::File, 0, 0);
        }
    }

    #[test]
    fn exhausted_space_gives_none() {
        let cfg = NamesConfig {
            max_len: 1,
            ..NamesConfig::default()
        };
        let mut m = Model::new();
        let r = m.root();
        for b in b"abcdefghijklmnopqrstuvwxyz0123456789_-$`'. " {
            m.insert(r, vec![*b], NodeKind::File, 0, 0);
        }
        // Where a name's length is counted in 16-bit units (Windows), a
        // single letter of the Unicode class fits too.
        for c in LATIN.iter().chain(CJK).chain(EMOJI) {
            let s = c.to_string();
            if platform::name_units(&s) <= 1 {
                m.insert(r, s.into_bytes(), NodeKind::File, 0, 0);
            }
        }
        let mut rng = ChaCha8Rng::seed_from_u64(17);
        assert!(draw_name(&mut rng, &cfg, &m, r, 255).is_none());
    }

    #[test]
    fn zero_weights_give_none() {
        let mut cfg = only(NameClass::Plain);
        cfg.classes.plain = 0.0;
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(15);
        assert!(draw_name(&mut rng, &cfg, &m, m.root(), 255).is_none());
    }

    #[test]
    fn names_deterministic() {
        let cfg = NamesConfig::default();
        let draw = |seed| {
            let mut m = Model::new();
            let r = m.root();
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            (0..100)
                .map(|_| {
                    let (n, c) = draw_name(&mut rng, &cfg, &m, r, 255).unwrap();
                    m.insert(r, n.clone(), NodeKind::File, 0, 0);
                    (n, c)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
    }

    /// The reserved device names the brief names for [`edge`].
    const EXPECTED_DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"];

    #[test]
    fn edge_names_are_valid_stored_names_of_the_two_kinds() {
        let cfg = NamesConfig {
            edge_rate: 1.0,
            ..NamesConfig::default()
        };
        let mut rng = ChaCha8Rng::seed_from_u64(18);
        let (mut surrogates, mut devices) = (0, 0);
        for _ in 0..2000 {
            let n = edge(&mut rng, &cfg);
            let units = wtf8::decode(&n).unwrap_or_else(|e| panic!("{n:02X?}: {e:?}"));
            let lone = units
                .iter()
                .filter(|u| (0xD800..=0xDFFF).contains(*u))
                .count();
            if lone > 0 {
                // One surrogate among ASCII: never part of a pair.
                assert_eq!(lone, 1, "{n:02X?}");
                assert!(
                    units
                        .iter()
                        .all(|&u| u < 0x80 || (0xD800..=0xDFFF).contains(&u)),
                    "{n:02X?}"
                );
                surrogates += 1;
            } else {
                let n = String::from_utf8(n).unwrap();
                let stem = n.split('.').next().unwrap();
                assert!(EXPECTED_DEVICES.contains(&stem), "{n:?}");
                devices += 1;
            }
        }
        assert!(surrogates > 800 && devices > 800, "{surrogates} {devices}");
    }

    #[test]
    fn no_edge_draw_where_edge_names_are_off() {
        // Where the platform draws no edge names (Unix), the rate is never
        // read and costs no draw: the same seed gives the same names.
        let draw = |edge_rate| {
            let cfg = NamesConfig {
                edge_rate,
                ..NamesConfig::default()
            };
            let mut m = Model::new();
            let r = m.root();
            let mut rng = ChaCha8Rng::seed_from_u64(19);
            (0..500)
                .map(|_| {
                    let (n, c) = draw_name(&mut rng, &cfg, &m, r, 255).unwrap();
                    m.insert(r, n.clone(), NodeKind::File, 0, 0);
                    (n, c)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(0.0) == draw(1.0), !platform::EDGE_NAMES);
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_avoid_the_forbidden_characters() {
        let cfg = NamesConfig::default();
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(20);
        let mut edges = 0;
        for _ in 0..5000 {
            let (n, c) = draw_name(&mut rng, &cfg, &m, m.root(), 255).unwrap();
            edges += usize::from(c == NameClass::Edge);
            let units = wtf8::decode(&n).unwrap();
            assert!(units.len() <= 255, "{n:02X?}");
            let t = text(&n);
            assert!(
                !t.chars()
                    .any(|c| c < '\u{20}'
                        || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\')),
                "{t:?}"
            );
        }
        assert!(edges > 0, "no edge name in 5000 draws");
    }
}
