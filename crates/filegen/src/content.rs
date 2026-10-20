//! Deterministic file content streams.

use std::io::{self, Write};

use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::config::{ContentConfig, Pattern};

const WRITE_BUF: usize = 1 << 20;

#[derive(Debug)]
enum Source {
    Random {
        rng: Box<ChaCha8Rng>,
        carry: [u8; 8],
        carry_len: usize,
    },
    Zeros,
    RepeatByte(u8),
    RepeatBlock {
        block: Vec<u8>,
        pos: usize,
    },
}

/// A finite, seed-determined byte stream.
#[derive(Debug)]
pub struct Content {
    source: Source,
    remaining: u64,
}

impl Content {
    /// Draws a content stream of `len` bytes. Consumes exactly two `u64`s
    /// from `rng`, whatever the outcome.
    pub fn draw(rng: &mut ChaCha8Rng, cfg: &ContentConfig, len: u64) -> Content {
        let d1 = rng.next_u64();
        let d2 = rng.next_u64();
        let patterned = (d1 as f64 / u64::MAX as f64) < cfg.pattern_fraction;
        let source = if patterned && !cfg.patterns.is_empty() {
            match cfg.patterns[(d2 % cfg.patterns.len() as u64) as usize] {
                Pattern::Zeros => Source::Zeros,
                Pattern::RepeatByte => Source::RepeatByte((d2 & 0xff) as u8),
                Pattern::RepeatBlock => {
                    let n = usize::try_from(cfg.block_size.0)
                        .unwrap_or(usize::MAX)
                        .max(1);
                    let mut block = vec![0u8; n];
                    ChaCha8Rng::seed_from_u64(d2).fill_bytes(&mut block);
                    Source::RepeatBlock { block, pos: 0 }
                }
            }
        } else {
            Source::Random {
                rng: Box::new(ChaCha8Rng::seed_from_u64(d2)),
                carry: [0; 8],
                carry_len: 0,
            }
        };
        Content {
            source,
            remaining: len,
        }
    }

    pub fn label(&self) -> &'static str {
        match self.source {
            Source::Random { .. } => "random",
            Source::Zeros => "zeros",
            Source::RepeatByte(_) => "repeat_byte",
            Source::RepeatBlock { .. } => "repeat_block",
        }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    /// Writes up to `buf.len()` bytes of the stream; returns the count.
    /// The stream is independent of how it is chunked.
    pub fn fill(&mut self, buf: &mut [u8]) -> usize {
        let n = (buf.len() as u64).min(self.remaining) as usize;
        let out = &mut buf[..n];
        match &mut self.source {
            Source::Zeros => out.fill(0),
            Source::RepeatByte(b) => out.fill(*b),
            Source::RepeatBlock { block, pos } => {
                let mut done = 0;
                while done < n {
                    let take = (block.len() - *pos).min(n - done);
                    out[done..done + take].copy_from_slice(&block[*pos..*pos + take]);
                    *pos = (*pos + take) % block.len();
                    done += take;
                }
            }
            Source::Random {
                rng,
                carry,
                carry_len,
            } => {
                let mut done = 0;
                while done < n && *carry_len > 0 {
                    out[done] = carry[8 - *carry_len];
                    *carry_len -= 1;
                    done += 1;
                }
                let aligned = (n - done) & !7;
                rng.fill_bytes(&mut out[done..done + aligned]);
                done += aligned;
                if done < n {
                    *carry = rng.next_u64().to_le_bytes();
                    let rest = n - done;
                    out[done..].copy_from_slice(&carry[..rest]);
                    *carry_len = 8 - rest;
                }
            }
        }
        self.remaining -= n as u64;
        n
    }

    /// Writes the whole remaining stream; returns the bytes written.
    pub fn write_to<W: Write>(&mut self, w: &mut W) -> io::Result<u64> {
        let mut buf = vec![0u8; WRITE_BUF.min(self.remaining.max(1) as usize)];
        let mut total = 0u64;
        while self.remaining > 0 {
            let n = self.fill(&mut buf);
            w.write_all(&buf[..n])?;
            total += n as u64;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pattern_fraction: f64, patterns: Vec<Pattern>, block: u64) -> ContentConfig {
        ContentConfig {
            pattern_fraction,
            patterns,
            block_size: crate::cli::Size(block),
        }
    }

    #[test]
    fn content_deterministic_and_exact_length() {
        let cfg = ContentConfig::default();
        let mut a = ChaCha8Rng::seed_from_u64(5);
        let mut b = ChaCha8Rng::seed_from_u64(5);
        let mut ca = Content::draw(&mut a, &cfg, 3_000_001);
        let mut cb = Content::draw(&mut b, &cfg, 3_000_001);
        let (mut va, mut vb) = (Vec::new(), Vec::new());
        assert_eq!(ca.write_to(&mut va).unwrap(), 3_000_001);
        cb.write_to(&mut vb).unwrap();
        assert_eq!(va, vb);
        assert_eq!(va.len(), 3_000_001);
        assert_eq!(ca.remaining(), 0);
        assert_eq!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn draw_consumes_fixed_rng_regardless_of_pattern() {
        let zero = cfg(0.0, ContentConfig::default().patterns, 4096);
        let all = cfg(1.0, ContentConfig::default().patterns, 4096);
        let mut a = ChaCha8Rng::seed_from_u64(5);
        let mut b = ChaCha8Rng::seed_from_u64(5);
        let mut c = ChaCha8Rng::seed_from_u64(5);
        Content::draw(&mut a, &zero, 10);
        Content::draw(&mut b, &all, 10);
        c.next_u64();
        c.next_u64();
        let next = c.next_u64();
        assert_eq!(a.next_u64(), next);
        assert_eq!(b.next_u64(), next);
    }

    #[test]
    fn patterns_are_patterns() {
        let all = cfg(1.0, vec![Pattern::Zeros], 4096);
        let mut c = Content::draw(&mut ChaCha8Rng::seed_from_u64(1), &all, 100);
        let mut v = Vec::new();
        c.write_to(&mut v).unwrap();
        assert_eq!(c.label(), "zeros");
        assert_eq!(v.len(), 100);
        assert!(v.iter().all(|&b| b == 0));
    }

    #[test]
    fn repeat_byte_and_block_shapes() {
        let mut cfg = cfg(1.0, vec![Pattern::RepeatByte], 4096);
        let mut c = Content::draw(&mut ChaCha8Rng::seed_from_u64(2), &cfg, 50);
        let mut v = Vec::new();
        c.write_to(&mut v).unwrap();
        assert_eq!(c.label(), "repeat_byte");
        assert!(v.iter().all(|&b| b == v[0]));

        cfg.patterns = vec![Pattern::RepeatBlock];
        cfg.block_size = crate::cli::Size(16);
        let mut c = Content::draw(&mut ChaCha8Rng::seed_from_u64(3), &cfg, 100);
        let mut v = Vec::new();
        c.write_to(&mut v).unwrap();
        assert_eq!(c.label(), "repeat_block");
        assert_eq!(v.len(), 100);
        for i in 16..100 {
            assert_eq!(v[i], v[i - 16]);
        }
    }

    #[test]
    fn random_content_is_incompressible_enough() {
        let cfg = cfg(0.0, ContentConfig::default().patterns, 4096);
        let mut c = Content::draw(&mut ChaCha8Rng::seed_from_u64(9), &cfg, 65536);
        assert_eq!(c.label(), "random");
        let mut v = Vec::new();
        c.write_to(&mut v).unwrap();
        let mut seen = [false; 256];
        for &b in &v {
            seen[b as usize] = true;
        }
        assert!(seen.iter().filter(|&&s| s).count() >= 250);
    }

    #[test]
    fn fill_is_chunk_independent() {
        for pf in [0.0, 1.0] {
            let cfg = cfg(pf, ContentConfig::default().patterns, 10);
            let mut whole = Content::draw(&mut ChaCha8Rng::seed_from_u64(7), &cfg, 1000);
            let mut parts = Content::draw(&mut ChaCha8Rng::seed_from_u64(7), &cfg, 1000);
            let mut a = Vec::new();
            whole.write_to(&mut a).unwrap();
            let mut b = Vec::new();
            let mut buf = [0u8; 37];
            loop {
                let n = parts.fill(&mut buf);
                if n == 0 {
                    break;
                }
                b.extend_from_slice(&buf[..n]);
            }
            assert_eq!(a, b);
        }
    }
}
