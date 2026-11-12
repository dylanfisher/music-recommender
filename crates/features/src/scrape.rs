//! What the sync queues after the library (ADR 0030), worked out between requests from the
//! scores, the catalog and what the community database holds, so the sync never runs out of
//! work: the popularity sweep once for each list the rebuild computes.
//!
//! Each look starts from the stored list, so with no catalog or no list nothing is queued
//! here and the sync fetches the library and its refresh alone.

use std::path::Path;
use std::time::SystemTime;

use catalog::community::{Community, Request, RequestKind, Tier};
use catalog::discogs::{Catalog, Reading};
use catalog::score::{self, Scores};

use crate::profile;
use crate::seed::Seeds;
use crate::sweep;
use crate::sync::SyncError;

/// What the planner last looked at, so a look that would find nothing new costs a `stat`.
#[derive(Debug, Default)]
pub(crate) struct Planner {
    /// The scores file's modification time at the last look; `None` before the first, or
    /// when there was no file.
    scores: Option<SystemTime>,
}

impl Planner {
    /// Whether a look now could queue something the last didn't: the list was rebuilt.
    ///
    /// # Errors
    ///
    /// When the scores file exists but can't be read.
    pub(crate) fn changed(&self, data_dir: &Path) -> Result<bool, SyncError> {
        Ok(modified(&score::path(data_dir))? != self.scores)
    }

    /// Queues what the scores, the catalog and the profile call for that isn't queued yet:
    /// the sweep of a list not swept before. `log` gets a line for each thing queued.
    ///
    /// # Errors
    ///
    /// When the community database, the scores, the catalog or the kept profile fail.
    pub(crate) fn plan(
        &mut self,
        db: &mut Community,
        data_dir: &Path,
        log: &mut dyn FnMut(&str),
    ) -> Result<(), SyncError> {
        // A rebuild renames a new file in, so its time changes with the list.
        let scores_modified = modified(&score::path(data_dir))?;
        if scores_modified == self.scores {
            return Ok(());
        }
        self.scores = scores_modified;
        let Reading::Open(scores) = Scores::open_read_only(data_dir)? else {
            return Ok(());
        };
        let computed = scores.computed()?;
        if db.swept()? == Some(computed.finished_at) {
            return Ok(());
        }
        let Reading::Open(catalog) = Catalog::open_read_only(data_dir)? else {
            return Ok(());
        };
        let Some(profile) = profile::kept(data_dir)? else {
            return Ok(());
        };
        let seeds = Seeds::read(&profile, &catalog)?;
        let artists = sweep::artists(&seeds, &catalog)?;
        let masters = sweep::masters(&scores, &catalog)?;
        let list = |kind, items: &[(u64, f64)]| -> Vec<(Request, f64)> {
            items
                .iter()
                .map(|&(id, priority)| (Request { kind, id, page: 1 }, priority))
                .collect()
        };
        let added_artists =
            db.queue(&list(RequestKind::ArtistReleases, &artists), Tier::Artists)?;
        let added_masters =
            db.queue(&list(RequestKind::MasterVersions, &masters), Tier::Masters)?;
        db.set_swept(computed.finished_at)?;
        log(&format!(
            "sweep: {} artists ({added_artists} new) and {} masters ({added_masters} new) queued",
            artists.len(),
            masters.len()
        ));
        Ok(())
    }
}

/// A file's modification time, `None` when there is no file.
fn modified(path: &Path) -> Result<Option<SystemTime>, SyncError> {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata
            .modified()
            .map(Some)
            .map_err(|error| SyncError::Io(path.to_path_buf(), error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SyncError::Io(path.to_path_buf(), error)),
    }
}
