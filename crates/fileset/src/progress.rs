//! Progress reporting hooks for long-running engine work.

pub trait Progress: Sync {
    /// One entry was recorded or compared.
    fn entry(&self);
    /// `n` more bytes were hashed.
    fn hashed_bytes(&self, n: u64);
    fn finish(&self);
}

/// Reports nothing.
pub struct NoProgress;

impl Progress for NoProgress {
    fn entry(&self) {}
    fn hashed_bytes(&self, _n: u64) {}
    fn finish(&self) {}
}
