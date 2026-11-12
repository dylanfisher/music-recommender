//! The lists the owner asked for (ADR 0030): the whole library's and each seed list kept in
//! `seed-lists/` (ADR 0028). The representatives of each list's first 200 candidates not
//! fetched yet are queued after the library and before the sweep, the most recently asked
//! list first, so a list the owner is looking at gets its values next.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

use catalog::community::{Community, Request, Tier};
use catalog::discogs::Reading;
use catalog::score::{self, Filter, Scores};
use catalog::seed_list::{self, Kept};

use crate::clock::unix_seconds;
use crate::ratings;
use crate::sync::SyncError;

/// How many of a list's candidates not fetched yet are queued: four pages of the player's 50.
const TOP: usize = 200;

/// A list asked for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Asked {
    /// The whole library's, asked for when it was stored: the player shows it first.
    Library,
    /// A seed list, by its file's name in `seed-lists/`.
    Seed(String),
}

/// The lists asked for, each with when, oldest first: the whole library's list by its file's
/// modification time, and each kept seed list by its own, which each ask sets (ADR 0028).
///
/// # Errors
///
/// When the scores file's or the seed lists' times can't be read.
pub(crate) fn lists(data_dir: &Path) -> Result<Vec<(SystemTime, Asked)>, SyncError> {
    let mut lists: Vec<(SystemTime, Asked)> = seed_list::asked_lists(data_dir)?
        .into_iter()
        .map(|(at, name)| (at, Asked::Seed(name)))
        .collect();
    let path = score::path(data_dir);
    match std::fs::metadata(&path).and_then(|metadata| metadata.modified()) {
        Ok(at) => lists.push((at, Asked::Library)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(SyncError::Io(path, error)),
    }
    lists.sort();
    Ok(lists)
}

/// Queues the first `TOP` candidates not fetched of each of `lists`, oldest first, in the
/// asked tier at the time each was asked for, so the latest ask's time is a candidate's
/// priority and its list goes first. A list that queues candidates not queued before is said
/// on `log`, and so is one that can't be read now, which is left for the next ask.
///
/// # Errors
///
/// When the community database or a list fails.
pub(crate) fn queue(
    db: &mut Community,
    data_dir: &Path,
    lists: &[(SystemTime, Asked)],
    log: &mut dyn FnMut(&str),
) -> Result<(), SyncError> {
    let fetched: HashSet<u64> = db
        .queued_releases()?
        .into_iter()
        .filter_map(|(id, done)| done.then_some(id))
        .collect();
    for (at, asked) in lists {
        let scores = match asked {
            Asked::Library => match Scores::open_read_only(data_dir)? {
                Reading::Open(scores) => scores,
                Reading::Missing | Reading::OtherShape => continue,
            },
            Asked::Seed(name) => match seed_list::open_named(data_dir, name) {
                Kept::Usable(scores, _) => scores,
                Kept::Unusable(why) => {
                    log(&format!("asked lists: skipping {name}, which {why}"));
                    continue;
                }
                Kept::Failed(error) => {
                    log(&format!(
                        "asked lists: skipping {name}, which can't be opened now ({error})"
                    ));
                    continue;
                }
            },
        };
        let mut top = Vec::new();
        let mut failed = None;
        scores.walk(&Filter::default(), false, &mut |head| {
            match ratings::representative(db, &head) {
                Ok(Some(release)) if !fetched.contains(&release) => {
                    top.push(release);
                }
                Ok(_) => {}
                Err(error) => {
                    failed = Some(error);
                    return false;
                }
            }
            top.len() < TOP
        })?;
        if let Some(error) = failed {
            return Err(error.into());
        }
        #[expect(clippy::cast_precision_loss, reason = "seconds since the epoch")]
        let priority = unix_seconds(*at) as f64;
        let requests: Vec<(Request, f64)> = top
            .iter()
            .map(|&release| (Request::release(release), priority))
            .collect();
        let added = db.queue(&requests, Tier::Asked)?;
        if added == 0 {
            continue;
        }
        log(&format!(
            "asked lists: {} candidates of {} queued ({added} new)",
            requests.len(),
            match asked {
                Asked::Library => "the whole library's list",
                Asked::Seed(name) => name,
            }
        ));
    }
    Ok(())
}
