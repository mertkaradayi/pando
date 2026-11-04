//! Opening a project from the grove, and reading every project again:
//! both on workers, so neither a project's first `git worktree list` nor
//! a scan of the machine's processes holds a frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use crate::actions::{self, ProjectSummary};
use crate::paths::PandoPaths;
use crate::tui::app::App;

use super::{Grove, GroveEvent};

/// How often the column reads every project again when nothing asks
/// sooner: a server started or stopped in another terminal shows within
/// it.
pub const SUMMARIES_EVERY: Duration = Duration::from_secs(3);

impl Grove {
    /// Opens `summary`'s project on a worker; it lands as
    /// [`GroveEvent::Opened`].
    pub(super) fn request_open(&mut self, summary: &ProjectSummary) {
        if self.opening.contains(&summary.id) {
            return;
        }
        self.opening.push(summary.id.clone());
        self.say(super::deliver::opening_note(&summary.name), false);
        #[cfg(test)]
        {
            self.open_requests.push(summary.id.clone());
        }
        #[cfg(not(test))]
        {
            let (home, summary, tx) = (self.home.clone(), summary.clone(), self.tx.clone());
            std::thread::spawn(move || {
                let opened = open_project(&home, &summary).map_err(|e| format!("{e:#}"));
                let _ = tx.send(GroveEvent::Opened(summary.id, Box::new(opened)));
            });
        }
    }
}

/// What `pando` does in a project before its TUI starts, for a project
/// opened from the grove: found again where it was registered, its config
/// loaded and its write locations guarded, its home made. What loading
/// warns about is said on its header, which is where the TUI's stderr
/// would have gone.
pub fn open_project(home: &Path, summary: &ProjectSummary) -> anyhow::Result<App> {
    let project = crate::project::discover(&summary.root, home)?;
    if project.id != summary.id {
        anyhow::bail!(
            "{} is another project now ({}) — run pando in it to know it again",
            summary.root.display(),
            project.id
        );
    }
    let mut paths = PandoPaths::new(home.to_path_buf(), project);
    let loaded = crate::config::load(&paths)?;
    let mut warnings = loaded.warnings.clone();
    warnings.extend(loaded.config.apply_to(&mut paths.project));
    actions::guard_write_locations(&paths, &loaded.config)?;
    paths.ensure_home()?;
    let mut app = App::new(paths, loaded.config)?;
    for warning in warnings {
        app.set_error(warning);
    }
    Ok(app)
}

/// Reads every project under `home` every [`SUMMARIES_EVERY`], and at
/// once whenever the returned sender is poked, until the grove is gone.
pub fn spawn_summaries(home: PathBuf, tx: Sender<GroveEvent>) -> Sender<()> {
    let (poke, poked) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        loop {
            match poked.recv_timeout(SUMMARIES_EVERY) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            // A burst of pokes is one read.
            while poked.try_recv().is_ok() {}
            // Unread, what was read last stays, and the reason is said.
            let event = match actions::summaries(&home) {
                Ok(summaries) => GroveEvent::Summaries(summaries),
                Err(e) => GroveEvent::Unread(format!("{e:#}")),
            };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    poke
}
