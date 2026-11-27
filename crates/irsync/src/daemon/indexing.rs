//! The daemon's indexing runs: the latest run of each export path, started
//! by a client over the admin session or by `index_at_start`, with its
//! counts as it goes and how it ended. Kept in memory only.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use irsync_proto::v1;

use crate::manage::admin::escape_controls;
use crate::warm::{Progress, WarmReport};

/// Who started a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StartedBy {
    /// A client over the admin session, by its audit label.
    Client(String),
    /// The export's `index_at_start`.
    AtStart,
}

/// One run: in progress until `ended_at` is set.
struct Run {
    export: String,
    by: StartedBy,
    started_at: SystemTime,
    ended_at: Option<SystemTime>,
    progress: Arc<Progress>,
    incomplete: bool,
    error: Option<String>,
}

impl Run {
    fn status(&self, path: &Path) -> v1::IndexRun {
        let (started_by, at_start) = match &self.by {
            StartedBy::Client(who) => (who.clone(), false),
            StartedBy::AtStart => (String::new(), true),
        };
        let p = &self.progress;
        v1::IndexRun {
            export: self.export.clone(),
            path: path.display().to_string(),
            started_by,
            at_start,
            started_at: unix(self.started_at),
            ended_at: self.ended_at.map_or(0, unix),
            examined: p.examined.load(Relaxed),
            hashed: p.hashed.load(Relaxed),
            bytes: p.bytes.load(Relaxed),
            failed: p.failed.load(Relaxed),
            incomplete: self.incomplete,
            error: self.error.clone().unwrap_or_default(),
        }
    }
}

/// Seconds since the Unix epoch; at least 1, so that 0 can mean "not yet".
fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_secs())
        .max(1)
}

/// The latest run of each export path since the daemon started.
#[derive(Default)]
pub(super) struct Runs {
    by_path: Mutex<BTreeMap<PathBuf, Run>>,
}

impl Runs {
    /// Starts a run for `path`, replacing the one that ended there: its
    /// counters and its status. `Err` with the run in progress when one is.
    pub(super) fn claim(
        &self,
        path: &Path,
        export: &str,
        by: StartedBy,
    ) -> Result<(Arc<Progress>, v1::IndexRun), Box<v1::IndexRun>> {
        let mut runs = self.by_path.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(run) = runs.get(path).filter(|r| r.ended_at.is_none()) {
            return Err(Box::new(run.status(path)));
        }
        let run = Run {
            export: export.to_string(),
            by,
            started_at: SystemTime::now(),
            ended_at: None,
            progress: Arc::default(),
            incomplete: false,
            error: None,
        };
        let out = (run.progress.clone(), run.status(path));
        runs.insert(path.to_path_buf(), run);
        Ok(out)
    }

    /// Ends the run of `path` with `result`: its final counts, or why it
    /// failed as a whole.
    pub(super) fn finish(&self, path: &Path, result: &io::Result<WarmReport>) {
        let mut runs = self.by_path.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(run) = runs.get_mut(path) else {
            return;
        };
        run.ended_at = Some(SystemTime::now());
        match result {
            Ok(r) => {
                let p = &run.progress;
                p.examined.store(r.examined, Relaxed);
                p.hashed.store(r.hashed, Relaxed);
                p.bytes.store(r.bytes, Relaxed);
                p.failed.store(r.errors.len() as u64, Relaxed);
                run.incomplete = r.incomplete;
            }
            Err(e) => run.error = Some(e.to_string()),
        }
    }

    /// Every path's latest run, in path order.
    pub(super) fn list(&self) -> Vec<v1::IndexRun> {
        let runs = self.by_path.lock().unwrap_or_else(PoisonError::into_inner);
        runs.iter().map(|(p, r)| r.status(p)).collect()
    }
}

/// The log line for a file a run could not read; the client's (or the
/// file system's) text cannot start a log line of its own.
pub(super) fn failed_line(file: &str, msg: &str) -> String {
    format!(
        "cannot index {}: {}",
        escape_controls(file),
        escape_controls(msg)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_while_running_is_refused() {
        let runs = Runs::default();
        let p = Path::new("/srv/photos");
        assert!(runs.claim(p, "photos", StartedBy::AtStart).is_ok());
        let run = runs
            .claim(p, "photos", StartedBy::Client("laptop".into()))
            .unwrap_err();
        assert!(run.at_start);
        assert_eq!(run.ended_at, 0);
        assert_eq!(run.export, "photos");
        assert_eq!(run.path, "/srv/photos");
    }

    #[test]
    fn a_claim_after_the_end_replaces_the_run() {
        let runs = Runs::default();
        let p = Path::new("/srv/photos");
        runs.claim(p, "photos", StartedBy::AtStart).unwrap();
        let report = WarmReport {
            examined: 3,
            hashed: 2,
            bytes: 10,
            ..WarmReport::default()
        };
        runs.finish(p, &Ok(report));
        let done = runs.list();
        assert_eq!(done.len(), 1);
        assert!(done[0].ended_at > 0);
        assert_eq!(
            (done[0].examined, done[0].hashed, done[0].bytes),
            (3, 2, 10)
        );
        let (_, run) = runs
            .claim(p, "pictures", StartedBy::Client("laptop".into()))
            .unwrap();
        assert_eq!(run.started_by, "laptop");
        assert!(!run.at_start);
        let list = runs.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].started_by, "laptop");
        assert_eq!(list[0].export, "pictures");
        assert_eq!(list[0].ended_at, 0);
        assert_eq!(list[0].examined, 0);
    }

    #[test]
    fn a_failed_run_ends_with_its_error() {
        let runs = Runs::default();
        let p = Path::new("/srv/photos");
        runs.claim(p, "photos", StartedBy::AtStart).unwrap();
        runs.finish(p, &Err(io::Error::other("gone")));
        let list = runs.list();
        assert!(list[0].ended_at > 0);
        assert_eq!(list[0].error, "gone");
    }

    #[test]
    fn the_counts_show_while_running() {
        let runs = Runs::default();
        let p = Path::new("/srv/photos");
        let (progress, _) = runs.claim(p, "photos", StartedBy::AtStart).unwrap();
        progress
            .examined
            .store(5, std::sync::atomic::Ordering::SeqCst);
        progress
            .failed
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let list = runs.list();
        assert_eq!((list[0].examined, list[0].failed), (5, 1));
        assert_eq!(list[0].ended_at, 0);
    }

    #[test]
    fn two_paths_are_two_runs() {
        let runs = Runs::default();
        runs.claim(Path::new("/srv/b"), "b", StartedBy::AtStart)
            .unwrap();
        runs.claim(Path::new("/srv/a"), "a", StartedBy::AtStart)
            .unwrap();
        let names: Vec<_> = runs.list().into_iter().map(|r| r.export).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn a_control_character_in_a_failed_path_is_escaped() {
        assert_eq!(failed_line("a\nb", "denied"), "cannot index a\\nb: denied");
    }
}
