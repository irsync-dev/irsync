use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// Makes `link` a symlink to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
/// How long the swapping thread leaves each state (the link, then the
/// directory) in place: long enough that an operation running in the
/// other thread meets it, short enough for many swaps a second.
const HOLD: Duration = Duration::from_micros(100);

/// A thread that swaps `dir` for a link to `outside` and back until the
/// returned [`Swap`] is dropped. Dropping it waits for the thread, which
/// always stops with `dir` back in its place.
pub fn swap_for_link(dir: &Path, outside: &Path) -> Swap {
    let mut aside = dir.as_os_str().to_owned();
    aside.push(".swapped-aside");
    let (dir, aside, outside) = (
        dir.to_path_buf(),
        PathBuf::from(aside),
        outside.to_path_buf(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let swaps = Arc::new(AtomicUsize::new(0));
    let thread = std::thread::spawn({
        let (stop, swaps) = (stop.clone(), swaps.clone());
        move || {
            // `stop` is only looked at here, with `dir` in its place.
            while !stop.load(Ordering::Relaxed) {
                // Fails while the other thread holds the tree in some
                // way a rename cannot pass; then try again.
                if std::fs::rename(&dir, &aside).is_err() {
                    std::thread::yield_now();
                    continue;
                }
                // Fails when the other thread made a directory at the
                // free name meanwhile: no swap this time.
                if symlink(&outside, &dir).is_ok() {
                    swaps.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(HOLD);
                }
                put_back(&dir, &aside);
                std::thread::sleep(HOLD);
            }
        }
    });
    Swap {
        stop,
        swaps,
        thread: Some(thread),
    }
}

/// Puts `aside` back at `dir`, removing what is there: the link, or a
/// directory the other thread made at the name while it was free.
fn put_back(dir: &Path, aside: &Path) {
    loop {
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.is_dir() => {
                let _ = std::fs::remove_dir_all(dir);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(dir);
            }
            Err(_) => {}
        }
        if std::fs::rename(aside, dir).is_ok() {
            return;
        }
        std::thread::yield_now();
    }
}

/// The running swap of [`swap_for_link`].
#[must_use = "the swapping stops when this is dropped"]
pub struct Swap {
    stop: Arc<AtomicBool>,
    swaps: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Swap {
    /// How many times the link has been put in place so far.
    pub fn swaps(&self) -> usize {
        self.swaps.load(Ordering::Relaxed)
    }
}

impl Drop for Swap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            if t.join().is_err() && !std::thread::panicking() {
                panic!("the thread that swaps a directory for a link panicked");
            }
        }
    }
}
