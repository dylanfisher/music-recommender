//! The stager (downloads.md, Staging and the NAS; ADRs 0011 and 0013): copies each
//! `downloaded` job's files into the staging root on a thread of its own, so a copy to the
//! NAS, minutes for a large album, never holds up the runner's rounds. It writes the copy's
//! progress as it goes and `staged` once the copy is checked; the runner's next round
//! deletes slskd's copies.
//!
//! It writes only `downloaded` jobs, and the store refuses a write to a job that changed
//! meanwhile, so it and the runner never undo each other's changes.

use std::collections::HashSet;
use std::path::PathBuf;

use catalog::community::SyncLock;
use catalog::download::{Answer, DownloadsError};
use config::Config;

use crate::clock::{Clock, unix_seconds};
use crate::download::{
    self, Downloads, File, FreeSpace, Job, POLL, Progress, RunError, SERVE_RETRY, Stage, State,
    located,
};
use crate::staging::{self, STOPPED, job_folder};

/// The stager: its own connection to the downloads store, the clock, and the two folders.
pub struct Stager<'a> {
    store: Downloads,
    clock: &'a dyn Clock,
    free_space: &'a FreeSpace,
    log: &'a mut dyn FnMut(&str),
    /// `slskd.downloads_dir` and `staging.dir`.
    downloads_dir: PathBuf,
    staging_dir: PathBuf,
    _lock: SyncLock,
}

impl<'a> Stager<'a> {
    /// Takes the stager's lock in the data directory, opens the store, and removes what a
    /// stopped copy left in `.incoming`.
    ///
    /// # Errors
    ///
    /// When the downloads are off, another stager holds the lock, the database fails, or
    /// `.incoming` can't be cleared.
    pub fn start(
        config: &'a Config,
        clock: &'a dyn Clock,
        free_space: &'a FreeSpace,
        log: &'a mut dyn FnMut(&str),
    ) -> Result<Self, RunError> {
        let settings = config
            .downloads
            .as_ref()
            .ok_or_else(|| RunError::Off(config.path.clone()))?;
        let lock = Downloads::stager_lock(&config.data_dir)?;
        let stager = Self {
            store: Downloads::open(&config.data_dir)?,
            clock,
            free_space,
            log,
            downloads_dir: settings.slskd_downloads_dir.clone(),
            staging_dir: settings.staging_dir.clone(),
            _lock: lock,
        };
        staging::clear_incoming(&stager.staging_dir).map_err(|error| {
            RunError::Disk(format!(
                "can't clear the copies a stopped stager left in {}: {error}",
                stager.staging_dir.display()
            ))
        })?;
        Ok(stager)
    }

    /// One round: each `downloaded` job staged, oldest first, one at a time.
    ///
    /// # Errors
    ///
    /// When the database fails; the job carries on in the next round.
    pub fn tick(&mut self) -> Result<(), RunError> {
        for id in self.store.ids(State::Downloaded)? {
            self.stage(id)?;
        }
        Ok(())
    }

    fn now(&self) -> i64 {
        unix_seconds(self.clock.now())
    }

    /// Copies job `id`'s downloaded files into its staging folder, writing the progress as
    /// it goes, checks their sizes, and records the staged files. A file missing or of
    /// another size, too little room, or a staging root that isn't mounted fails the stage,
    /// and Retry stages it again. A job cancelled or retried during the copy stops it.
    fn stage(&mut self, id: i64) -> Result<(), RunError> {
        let Some(candidate) = self.store.chosen(id)? else {
            return Ok(());
        };
        let ledger: HashSet<String> = self.store.ledger(id)?.into_iter().collect();
        let mut sources = Vec::with_capacity(candidate.files.len());
        for file in &candidate.files {
            match located(&self.downloads_dir, id, file) {
                Some((path, _)) if ledger.contains(&path) => sources.push(staging::Source {
                    path: PathBuf::from(path),
                    name: file.name.clone(),
                    size: file.size,
                }),
                _ => {
                    let reason = format!(
                        "{} is no longer in slskd's downloads folder with its size",
                        file.name
                    );
                    return self.stage_failed(id, &reason);
                }
            }
        }
        let total: u64 = sources.iter().map(|source| source.size).sum();
        if let Ok(free) = (self.free_space)(&self.staging_dir)
            && free < total
        {
            let reason = format!(
                "{} MB free in the staging folder, and the album needs {} MB",
                free >> 20,
                total.div_ceil(1 << 20)
            );
            return self.stage_failed(id, &reason);
        }
        let none_yet = Progress {
            files_done: 0,
            files_total: sources.len() as u64,
            bytes_done: 0,
            bytes_total: total,
        };
        let started = self.store.progress(id, none_yet, self.now())?;
        if self.settle(id, started).is_none() {
            return Ok(());
        }
        let folder = job_folder(id);
        let mut refused = None;
        let mut failed: Option<DownloadsError> = None;
        let store = &mut self.store;
        let clock = self.clock;
        let mut report = |progress: Progress| {
            match store.progress(id, progress, unix_seconds(clock.now())) {
                Ok(Ok(_)) => return true,
                Ok(Err(refusal)) => refused = Some(refusal),
                Err(error) => failed = Some(error),
            }
            false
        };
        let copied = staging::stage(&self.staging_dir, &folder, &sources, &mut report);
        if let Some(error) = failed {
            return Err(error.into());
        }
        if let Some(refusal) = refused {
            self.settle(id, Err(refusal));
            return Ok(());
        }
        let names = match copied {
            Ok(names) => names,
            Err(reason) => {
                debug_assert_ne!(reason, STOPPED, "only a refused report stops the copy");
                return self.stage_failed(id, &reason);
            }
        };
        let files: Vec<File> = candidate
            .files
            .iter()
            .zip(names)
            .map(|(file, name)| File {
                name,
                size: file.size,
                position: file.position.clone(),
            })
            .collect();
        let staged = self.store.staged(id, &folder, &files, self.now())?;
        if let Some(job) = self.settle(id, staged) {
            self.log_job(&job, &format!("staged in {folder}"));
        } else if let Err(error) = staging::discard(&self.staging_dir, &folder) {
            // No job records the folder, so nothing else would delete it.
            (self.log)(&format!(
                "job {id}: can't remove {folder} in the staging folder: {error}"
            ));
        }
        Ok(())
    }

    fn stage_failed(&mut self, id: i64, reason: &str) -> Result<(), RunError> {
        let failed = self.store.fail(id, Stage::Stage, reason, self.now())?;
        if let Some(job) = self.settle(id, failed) {
            self.log_job(&job, &format!("staging failed: {reason}"));
        }
        Ok(())
    }

    fn settle(&mut self, id: i64, answer: Answer<Job>) -> Option<Job> {
        download::settle(self.log, id, answer)
    }

    fn log_job(&mut self, job: &Job, what: &str) {
        download::log_job(self.log, job, what);
    }
}

/// The launch agent's staging loop, on its own thread beside the runner's: starts the
/// stager and runs a round every 5 s, waiting a minute after a round that failed or a
/// start that couldn't take the lock. Never returns.
pub fn serve(
    config: &Config,
    clock: &dyn Clock,
    free_space: &FreeSpace,
    log: &mut dyn FnMut(&str),
) -> ! {
    loop {
        let error = match Stager::start(config, clock, free_space, log) {
            Ok(mut stager) => loop {
                match stager.tick() {
                    Ok(()) => clock.sleep(POLL),
                    Err(error) => {
                        (stager.log)(&format!("staging: error: {error}"));
                        clock.sleep(SERVE_RETRY);
                    }
                }
            },
            Err(error) => error,
        };
        log(&format!("staging: error: {error}"));
        clock.sleep(SERVE_RETRY);
    }
}
