//! Bakes the git commit and dirty flag into the binary (`VERGEN_GIT_SHA`,
//! `VERGEN_GIT_DIRTY`); see `src/build.rs`. Outside a checkout the
//! variables stay unset and the binary reports "unknown".

use vergen_gitcl::{Emitter, Gitcl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gitcl = Gitcl::builder().sha(true).dirty(false).build();
    Emitter::default().add_instructions(&gitcl)?.emit()?;
    Ok(())
}
