//! Name generation: weighted name classes, NFC, fold-unique per directory.

use rand::distr::weighted::WeightedIndex;
use rand::RngExt;
use rand_chacha::ChaCha8Rng;
use unicode_normalization::UnicodeNormalization;

use crate::config::NamesConfig;
use crate::model::{Model, NodeId};

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

/// Draw a new name for an entry in `dir`: NFC, at most
/// `min(cfg.max_len, max_bytes)` bytes, never `.`/`..`, no `/` or NUL, not
/// starting with `.fileset`, and unique under [`Model::fold`] among `dir`'s
/// children. Each attempt redraws the class too; `None` after 64 rejected
/// attempts (or if every class weight is zero).
pub fn draw_name(
    rng: &mut ChaCha8Rng,
    cfg: &NamesConfig,
    model: &Model,
    dir: NodeId,
    max_bytes: usize,
) -> Option<(String, NameClass)> {
    let w = &cfg.classes;
    let index =
        WeightedIndex::new([w.plain, w.spaces, w.dotfile, w.unicode, w.long, w.awkward]).ok()?;
    let limit = (cfg.max_len as usize).min(max_bytes);
    for _ in 0..MAX_ATTEMPTS {
        let class = CLASSES[rng.sample(&index)];
        let name: String = build(rng, cfg, class, limit).nfc().collect();
        if acceptable(&name, limit) && !model.has_folded_name(dir, &Model::fold(&name)) {
            return Some((name, class));
        }
    }
    None
}

/// The structural rules, independent of siblings.
fn acceptable(name: &str, limit: usize) -> bool {
    !name.is_empty()
        && name.len() <= limit
        && name != "."
        && name != ".."
        && !name.contains(['/', '\0'])
        && !name.as_bytes().starts_with(b".fileset")
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
    }
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

    fn assert_valid(n: &str, limit: usize) {
        assert!(!n.is_empty(), "empty name");
        assert!(!n.contains('/') && !n.contains('\0'), "{n:?}");
        assert!(n != "." && n != "..", "{n:?}");
        assert!(!n.starts_with(".fileset"), "{n:?}");
        assert!(n.len() <= limit, "{n:?} longer than {limit}");
        assert_eq!(n, n.nfc().collect::<String>());
    }

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
            assert!(!n.contains('/') && !n.contains('\0') && n != "." && n != "..");
            assert!(!n.starts_with(".fileset"));
            assert!(n.len() <= 255);
            assert_eq!(n, n.nfc().collect::<String>());
            assert!(!m.has_folded_name(r, &Model::fold(&n)));
            m.insert(r, n.into_bytes(), NodeKind::File, 0, 0);
        }
        assert_eq!(classes.len(), 6);
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
            assert_valid(&n, 10);
            assert!(!m.has_folded_name(r, &Model::fold(&n)));
            m.insert(r, n.into_bytes(), NodeKind::File, 0, 0);
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
            assert_valid(&n, 40);
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
            assert_valid(&n, 255);
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
            assert_valid(&n, 255);
            assert!(!n.is_ascii(), "{n:?}");
            assert!((1..=12).contains(&n.chars().count()));
        }
    }

    #[test]
    fn class_shapes() {
        let m = Model::new();
        let mut rng = ChaCha8Rng::seed_from_u64(16);
        for _ in 0..200 {
            let (n, _) = draw_name(&mut rng, &only(NameClass::Spaces), &m, m.root(), 255).unwrap();
            assert!(n.contains(' '), "{n:?}");
            let (n, _) = draw_name(&mut rng, &only(NameClass::Dotfile), &m, m.root(), 255).unwrap();
            assert!(n.starts_with('.'), "{n:?}");
            let (n, _) = draw_name(&mut rng, &only(NameClass::Awkward), &m, m.root(), 255).unwrap();
            assert!(
                n.starts_with('-')
                    || n.ends_with(' ')
                    || n.ends_with('.')
                    || n.contains(['$', '`', '\'']),
                "{n:?}"
            );
            let (n, _) = draw_name(&mut rng, &only(NameClass::Plain), &m, m.root(), 255).unwrap();
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
            assert_valid(&n, 1);
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
                    m.insert(r, n.clone().into_bytes(), NodeKind::File, 0, 0);
                    (n, c)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(7), draw(7));
        assert_ne!(draw(7), draw(8));
    }
}
