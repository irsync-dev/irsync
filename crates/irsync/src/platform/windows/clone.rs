//! Reflink clones of source files, for `--paranoid`: none on Windows.
//! Every entry point answers as on a Unix whose filesystem cannot clone:
//! [`reflink`] is `Unsupported` and [`Clones`] makes no clone, so
//! `--paranoid` hashes the file itself and checks it again, and a `.part`
//! that a clone would have seeded is written from a copy instead.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::fs::paths::RelPath;
use crate::hasher::Freshness;

/// The start of a clone folder's name; the walk prunes a directory named
/// so ([`crate::scan::is_clone_dir`]) on every system, as a tree may come
/// from a Unix machine.
pub const CLONE_DIR_PREFIX: &str = ".irsync-clones-";

/// Makes `dst` share `src`'s extents: not on Windows (`Unsupported`).
pub fn reflink(_src: &File, _dst: &File) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// A clone of a source file. None is ever made on Windows.
#[derive(Debug)]
pub struct Cloned {
    /// The clone, open.
    pub file: File,
    /// Where it is.
    pub path: PathBuf,
    /// The source's stat at the moment of cloning.
    pub stat: Freshness,
}

/// A session's clones: never any on Windows.
pub struct Clones;

impl Clones {
    pub fn new(_session_id: &str) -> Clones {
        Clones
    }

    /// Where clone folders may go: nowhere on Windows, so it changes
    /// nothing.
    pub fn within(self, _dir: PathBuf) -> Clones {
        self
    }

    /// No clone can be made: `None`.
    pub fn clone_file(&self, _full: &Path) -> io::Result<Option<Cloned>> {
        Ok(None)
    }

    /// No clone can be made: `None`.
    pub fn clone_file_beneath(&self, _base: &Path, _rel: &RelPath) -> io::Result<Option<Cloned>> {
        Ok(None)
    }

    /// Nothing to remove.
    pub fn close(&self) {}
}
