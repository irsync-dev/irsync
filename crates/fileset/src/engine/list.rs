//! `list`: print what the manifest holds, one line per entry. Reads only the manifest;
//! the tree is not looked at.

use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use anyhow::Context;

use super::{cannot_open, canonical_root, usable_here, Outcome};
use crate::glob::Pattern;
use crate::platform::Platform;
use crate::report::line_listed;
use crate::store::{self, OpenMode};

pub struct ListOpts {
    /// Root of the tree. With `manifest` given it need not exist.
    pub root: PathBuf,
    pub manifest: Option<PathBuf>,
    /// List only the entries whose stored path matches this glob (see [`crate::glob`]).
    pub pattern: Option<Vec<u8>>,
    /// Also print owner, link count, device number and extended attributes.
    pub verbose: bool,
}

/// Prints the manifest's entries that the pattern accepts, in the order the store keeps
/// them. `Ok(Outcome::Clean)` also when nothing matched, and when `out` was closed early
/// (a broken pipe: `fileset list ... | head`).
pub fn list(opts: &ListOpts, out: &mut dyn Write) -> anyhow::Result<Outcome> {
    // A manifest given by path can be listed without its tree, a copy of one for example.
    let root = match &opts.manifest {
        Some(_) if opts.root.symlink_metadata().is_err() => opts.root.clone(),
        _ => canonical_root(&opts.root)?,
    };
    let loc = store::locate(&root, opts.manifest.as_deref(), None, false)?;
    let store = store::open(&loc.path, loc.backend, OpenMode::ReadOnly)
        .with_context(|| cannot_open(&loc))?;
    usable_here(store.meta()?.platform, Platform::CURRENT)?;
    let pattern = opts.pattern.as_deref().map(Pattern::new);
    let mut closed = false;
    let walked = store.for_each(&mut |path, entry| {
        if pattern.as_ref().is_some_and(|p| !p.matches(path)) {
            return Ok(());
        }
        match writeln!(out, "{}", line_listed(path, entry, opts.verbose)) {
            Ok(()) => Ok(()),
            Err(e) => {
                closed = e.kind() == ErrorKind::BrokenPipe;
                Err(e.into())
            }
        }
    });
    match walked {
        // The reader went away: there is nobody left to list for.
        Err(_) if closed => {}
        other => other?,
    }
    Ok(Outcome::Clean)
}
