//! The downloads store: each Soulseek download job and its lifecycle, in `downloads.sqlite`
//! in the data directory (downloads.md, ADR 0009). The service is the one writer of a job;
//! the player's reports come in through [`Downloads::handoff`].
//!
//! [`next_state`] is the one place transitions are decided. Every change to a job takes the
//! store's next revision, which `list` pages by, and the store's epoch, made with the file,
//! tells a client its revisions are from another file. Every time is passed in, in seconds
//! since the Unix epoch.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::community::{LockError, SyncLock, try_lock};
use crate::error::DatabaseError;
use crate::sqlite;

/// The database's file name inside the data directory.
const FILE_NAME: &str = "downloads.sqlite";

/// The database's name in its errors.
const NAME: &str = "downloads";

/// The lock file `open` holds while it checks, deletes and creates the database file.
const OPEN_LOCK_FILE: &str = "downloads.open.lock";

/// The lock file the one download runner holds.
const RUNNER_LOCK_FILE: &str = "downloads.lock";

/// The lock file the one stager holds.
const STAGER_LOCK_FILE: &str = "staging.lock";

/// The tagging lease: renewed every 3 minutes while tagging, so a crash or sleep frees the
/// job within this.
pub const LEASE: Duration = Duration::from_mins(10);

/// The review claim, taken when the owner opens a row to review or approve it.
pub const REVIEW_CLAIM: Duration = Duration::from_hours(24);

/// A terminal job stays in `list` this long after it became terminal, then is purged.
pub const TERMINAL_KEPT: Duration = Duration::from_hours(30 * 24);

/// The most candidates kept for a job, best first.
pub const MAX_CANDIDATES: usize = 5;

/// The most jobs one `list` reply holds.
pub(crate) const PAGE: usize = 200;

/// `job` is one row per job. `active_release` is the release id while the job isn't terminal
/// and `NULL` after, so its `UNIQUE` allows one active job per release. `chosen` is the rank
/// of the candidate being downloaded. `tagging` and the other enum columns hold the API's
/// names. `track` is the snapshot's tracklists; `candidate` and `candidate_file` the folders
/// the search kept, best first; `staged_file` the stager's ledger of what it copied, which
/// replaces the candidate's files in the job from `staged` on; `ledger` every path under
/// slskd's downloads folder the job's files were adopted at, the only paths the service
/// deletes there; `tried_peer` every peer a download was started from, in the order first
/// tried, which no later search picks again; `event` each state a job entered. `meta` has
/// no column type, so integers stay integers.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS job (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    revision INTEGER NOT NULL UNIQUE,
    release_id INTEGER NOT NULL,
    active_release INTEGER UNIQUE,
    artist TEXT NOT NULL,
    title TEXT NOT NULL,
    year INTEGER,
    source TEXT NOT NULL,
    format_policy TEXT NOT NULL,
    state TEXT NOT NULL,
    failed_stage TEXT,
    reason TEXT,
    attempt INTEGER NOT NULL,
    next_attempt_at INTEGER,
    searched_again INTEGER NOT NULL,
    files_done INTEGER,
    files_total INTEGER,
    bytes_done INTEGER,
    bytes_total INTEGER,
    search_query TEXT,
    search_number INTEGER,
    search_count INTEGER,
    search_peers INTEGER,
    search_files INTEGER,
    search_started_at INTEGER,
    chosen INTEGER,
    staged_path TEXT,
    tagging TEXT NOT NULL,
    lease_machine TEXT,
    lease_expires_at INTEGER,
    review_machine TEXT,
    review_expires_at INTEGER,
    upgrade_quality TEXT,
    cleanup_pending INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    terminal_at INTEGER
);
CREATE TABLE IF NOT EXISTS track (
    job_id INTEGER NOT NULL,
    tracklist INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    position TEXT NOT NULL,
    title TEXT NOT NULL,
    seconds INTEGER,
    PRIMARY KEY (job_id, tracklist, seq)
);
CREATE TABLE IF NOT EXISTS old_path (
    job_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (job_id, seq)
);
CREATE TABLE IF NOT EXISTS tagging_reason (
    job_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    reason TEXT NOT NULL,
    PRIMARY KEY (job_id, seq)
);
CREATE TABLE IF NOT EXISTS candidate (
    job_id INTEGER NOT NULL,
    rank INTEGER NOT NULL,
    peer TEXT NOT NULL,
    format TEXT NOT NULL,
    bit_depth INTEGER,
    sample_rate INTEGER,
    bitrate INTEGER,
    variable_bitrate INTEGER,
    tracklist INTEGER NOT NULL,
    PRIMARY KEY (job_id, rank)
);
CREATE TABLE IF NOT EXISTS candidate_file (
    job_id INTEGER NOT NULL,
    rank INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    remote TEXT NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    position TEXT,
    PRIMARY KEY (job_id, rank, seq)
);
CREATE TABLE IF NOT EXISTS staged_file (
    job_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    position TEXT,
    PRIMARY KEY (job_id, seq)
);
CREATE TABLE IF NOT EXISTS ledger (
    job_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (job_id, path)
);
CREATE TABLE IF NOT EXISTS tried_peer (
    job_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    peer TEXT NOT NULL,
    PRIMARY KEY (job_id, seq),
    UNIQUE (job_id, peer)
);
CREATE TABLE IF NOT EXISTS event (
    id INTEGER PRIMARY KEY,
    job_id INTEGER NOT NULL,
    at INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    reason TEXT
);
CREATE INDEX IF NOT EXISTS event_job ON event (job_id, id);
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value NOT NULL
);
";

/// The objects `SCHEMA` creates, read once.
static EXPECTED: sqlite::Expected = sqlite::Expected::new();

/// The tables that hold a job's rows besides `job`, each keyed by `job_id`.
const JOB_TABLES: [&str; 9] = [
    "track",
    "old_path",
    "tagging_reason",
    "candidate",
    "candidate_file",
    "staged_file",
    "ledger",
    "tried_peer",
    "event",
];

const EPOCH: &str = "epoch";
const REVISION: &str = "revision";
/// The revision the last purge took; a client whose `after` is below it may hold a row
/// that is gone.
const PURGE_REVISION: &str = "purge_revision";

const SET_META: &str = "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)";

/// Declares an enum stored and sent by name, with `name` and `parse`.
macro_rules! named {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The name the store and the API use.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            /// The value with this name.
            #[must_use]
            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}
// Exported under another name: `named` is also the function that reads one back.
pub(crate) use named as named_enum;

named! {
    /// Which audio a job accepts (api.md, Format policies).
    Policy {
        /// FLAC files. The default.
        Flac = "flac",
        /// FLAC or ALAC.
        Lossless = "lossless",
        /// MP3 at 320 kbps constant, or variable averaging 220–270 kbps.
        Mp3320 = "mp3_320",
        /// Any audio format.
        Any = "any",
    }
}

impl Policy {
    /// The policy a job takes when none is given.
    pub const DEFAULT: Self = Self::Flac;

    /// Whether it accepts only lossless audio, as an upgrade needs.
    #[must_use]
    pub fn is_lossless(self) -> bool {
        matches!(self, Self::Flac | Self::Lossless)
    }
}

named! {
    /// Where a job came from.
    Source {
        /// Queued by hand.
        Manual = "manual",
        /// A lossless copy of a library album.
        Upgrade = "upgrade",
        /// From a recommendation. Nothing produces it yet.
        Recommendation = "recommendation",
    }
}

named! {
    /// A candidate folder's audio format.
    Format {
        /// FLAC.
        Flac = "flac",
        /// Apple Lossless, in an `m4a`.
        Alac = "alac",
        /// MP3.
        Mp3 = "mp3",
        /// Ogg Vorbis.
        Ogg = "ogg",
        /// Opus.
        Opus = "opus",
        /// WAV.
        Wav = "wav",
        /// Monkey's Audio.
        Ape = "ape",
        /// `WavPack`.
        Wv = "wv",
    }
}

named! {
    /// The step a failed job failed at.
    Stage {
        /// The search, the service's.
        Search = "search",
        /// The download, the service's.
        Download = "download",
        /// The copy into staging, the service's.
        Stage = "stage",
        /// The player's tag run.
        Tag = "tag",
        /// The player's move into the library.
        Move = "move",
        /// The player's delete of an upgrade's old files.
        Delete = "delete",
        /// The player's carrying of stats to an upgrade's new files.
        Carry = "carry",
    }
}

named! {
    /// The player's tagging of a staged job.
    TaggingStatus {
        /// Not reported yet.
        None = "none",
        /// A tag run is going.
        Running = "running",
        /// Matched and written cleanly.
        Clean = "clean",
        /// Something differs; see the reasons.
        Discrepancy = "discrepancy",
        /// The run failed; see the reasons.
        Error = "error",
    }
}

/// A job's place in its lifecycle (downloads.md, States).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    /// Waiting for its search.
    Queued,
    /// A search is running.
    Searching,
    /// No candidate passed; the next attempt is scheduled.
    NotFound,
    /// A candidate is chosen; the download is being enqueued.
    CandidateFound,
    /// Transfers are running.
    Downloading,
    /// Every file is complete in slskd's folder.
    Downloaded,
    /// Copied into staging and checked.
    Staged,
    /// The owner approved it.
    Approved,
    /// The player is moving files.
    Importing,
    /// Done.
    Imported,
    /// The owner rejected it.
    Rejected,
    /// Failed at a stage.
    Failed(Stage),
    /// Cancelled.
    Cancelled,
}

impl State {
    /// Every state, a failed one at each stage.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut states = vec![
            Self::Queued,
            Self::Searching,
            Self::NotFound,
            Self::CandidateFound,
            Self::Downloading,
            Self::Downloaded,
            Self::Staged,
            Self::Approved,
            Self::Importing,
            Self::Imported,
            Self::Rejected,
            Self::Cancelled,
        ];
        states.extend(Stage::ALL.iter().map(|&stage| Self::Failed(stage)));
        states
    }

    /// The name the store and the API use; a failed job's stage is apart, in `failed_stage`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Searching => "searching",
            Self::NotFound => "not_found",
            Self::CandidateFound => "candidate_found",
            Self::Downloading => "downloading",
            Self::Downloaded => "downloaded",
            Self::Staged => "staged",
            Self::Approved => "approved",
            Self::Importing => "importing",
            Self::Imported => "imported",
            Self::Rejected => "rejected",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The stage of a failed job, else `None`.
    #[must_use]
    pub fn failed_stage(self) -> Option<Stage> {
        match self {
            Self::Failed(stage) => Some(stage),
            _ => None,
        }
    }

    /// Imported, rejected and cancelled jobs change no more, apart from their cleanup.
    #[must_use]
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Imported | Self::Rejected | Self::Cancelled)
    }

    fn parse(name: &str, stage: Option<&str>) -> Option<Self> {
        if name == Self::Failed(Stage::Search).name() {
            return Stage::parse(stage?).map(Self::Failed);
        }
        Self::all()
            .into_iter()
            .find(|state| state.name() == name && state.failed_stage().is_none())
    }
}

/// What happens to a job, as [`next_state`] judges it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    /// The search starts.
    Search,
    /// The search kept no candidate.
    NotFound,
    /// The search kept candidates.
    Found,
    /// Back to the queue: the next attempt is due, every candidate has failed, or a restart
    /// found a search that was running.
    Requeue,
    /// The chosen candidate's files are enqueued.
    Download,
    /// The candidate failed; the next stored one is chosen.
    NextCandidate,
    /// The chosen peer stalled or went offline and no stored candidate is left: the next
    /// search waits.
    Dropped,
    /// Every file is complete.
    Downloaded,
    /// The stager copied and checked the files.
    Staged,
    /// The player: the owner approved it.
    Approved,
    /// The player: the import started.
    Importing,
    /// The player: the import is done.
    Imported,
    /// The player: the owner rejected it.
    Rejected,
    /// A stage failed.
    Fail(Stage),
    /// The owner asked for it again.
    Retry,
    /// The owner cancelled it.
    Cancel,
    /// The player: the files are for another release than the one queued.
    NewRelease,
    /// The owner threw the staged download away, to search again from another peer.
    Discard,
}

impl Event {
    /// Every event, a failure at each stage.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut events = vec![
            Self::Search,
            Self::NotFound,
            Self::Found,
            Self::Requeue,
            Self::Download,
            Self::NextCandidate,
            Self::Dropped,
            Self::Downloaded,
            Self::Staged,
            Self::Approved,
            Self::Importing,
            Self::Imported,
            Self::Rejected,
            Self::Retry,
            Self::Cancel,
            Self::NewRelease,
            Self::Discard,
        ];
        events.extend(Stage::ALL.iter().map(|&stage| Self::Fail(stage)));
        events
    }
}

/// The state `event` takes a job in `state` to, or `None` when it isn't allowed. An event
/// that would leave a terminal or player-set state as it is (a second `Approved`, a
/// `Cancel` of a cancelled job) is allowed and returns `state`, so a repeat is answered
/// rather than refused.
#[must_use]
#[expect(
    clippy::match_same_arms,
    reason = "one arm per transition, as the table in downloads.md reads"
)]
pub(crate) fn next_state(state: State, event: Event) -> Option<State> {
    use State as S;
    let next = match (event, state) {
        (Event::Search, S::Queued) => S::Searching,
        (Event::NotFound, S::Searching) => S::NotFound,
        (Event::Found, S::Searching) => S::CandidateFound,
        (Event::Requeue, S::Searching | S::NotFound | S::CandidateFound | S::Downloading) => {
            S::Queued
        }
        (Event::Download, S::CandidateFound) => S::Downloading,
        (Event::NextCandidate, S::CandidateFound | S::Downloading) => S::CandidateFound,
        (Event::Dropped, S::CandidateFound | S::Downloading) => S::NotFound,
        (Event::Downloaded, S::Downloading) => S::Downloaded,
        (Event::Staged, S::Downloaded) => S::Staged,
        (Event::Approved, S::Staged | S::Approved) => S::Approved,
        (Event::Importing, S::Approved | S::Importing) => S::Importing,
        (Event::Imported, S::Approved | S::Importing | S::Imported) => S::Imported,
        (Event::Rejected, S::Staged | S::Rejected) => S::Rejected,
        (Event::Fail(failed), _) if state == S::Failed(failed) => state,
        (Event::Fail(Stage::Search), S::Queued | S::Searching)
        | (Event::Fail(Stage::Download), S::CandidateFound | S::Downloading)
        | (Event::Fail(Stage::Stage), S::Downloaded)
        | (Event::Fail(Stage::Tag), S::Staged)
        | (Event::Fail(Stage::Move | Stage::Delete | Stage::Carry), S::Approved | S::Importing) => {
            S::Failed(event_stage(event))
        }
        (Event::NewRelease, S::Staged | S::Failed(Stage::Tag)) => state,
        (Event::Discard, S::Staged) => S::Queued,
        (Event::Retry, S::NotFound | S::Failed(Stage::Search | Stage::Download)) => S::Queued,
        (Event::Retry, S::Failed(Stage::Stage)) => S::Downloaded,
        (Event::Retry, S::Failed(Stage::Tag)) => S::Staged,
        (Event::Retry, S::Failed(Stage::Move | Stage::Delete | Stage::Carry)) => S::Approved,
        (
            Event::Cancel,
            S::Queued
            | S::Searching
            | S::NotFound
            | S::CandidateFound
            | S::Downloading
            | S::Downloaded
            | S::Staged
            | S::Failed(Stage::Search | Stage::Download | Stage::Stage | Stage::Tag)
            | S::Cancelled,
        ) => S::Cancelled,
        _ => return None,
    };
    Some(next)
}

fn event_stage(event: Event) -> Stage {
    match event {
        Event::Fail(stage) => stage,
        _ => unreachable!("only a failure has a stage"),
    }
}

/// A track of a snapshot's tracklist, as Discogs spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    /// As Discogs writes it: "A1", "CD2-4".
    pub position: String,
    /// The title, not folded.
    pub title: String,
    /// The length, if Discogs has one.
    pub seconds: Option<u32>,
}

/// The release a job is for, from its snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The Discogs release id.
    pub release_id: i64,
    /// As Discogs spells it; `Various` for a compilation.
    pub artist: String,
    /// As Discogs spells it.
    pub title: String,
    /// The release's year, if known.
    pub year: Option<i32>,
}

/// What an upgrade replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upgrade {
    /// The old album's quality, for the window.
    pub current_quality: String,
    /// The old album's files, relative to the library share.
    pub old_paths: Vec<String>,
}

/// A job as `create` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewJob {
    /// The release.
    pub release: Release,
    /// One or two tracklists, the player's flattenings, the preferred first.
    pub tracklists: Vec<Vec<Track>>,
    /// Which audio it accepts.
    pub format_policy: Policy,
    /// Where it came from.
    pub source: Source,
    /// For `Source::Upgrade`.
    pub upgrade: Option<Upgrade>,
}

/// A file of a candidate folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateFile {
    /// The peer's full path, as slskd reports it and takes it back to enqueue.
    pub remote: String,
    /// The path under the candidate folder, `/`-separated: `CD1/1-01 First Song.flac`.
    pub name: String,
    /// Bytes, as the peer reported them.
    pub size: u64,
    /// The release position it matched, or `None` for an extra file.
    pub position: Option<String>,
}

/// A candidate folder the search kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The Soulseek user.
    pub peer: String,
    /// The folder's audio format.
    pub format: Format,
    /// As the peer reported them.
    pub bit_depth: Option<u32>,
    /// Hz.
    pub sample_rate: Option<u32>,
    /// kbps.
    pub bitrate: Option<u32>,
    /// Whether the bitrate is variable.
    pub variable_bitrate: Option<bool>,
    /// The index into the snapshot's tracklists the files matched.
    pub tracklist: usize,
    /// In release order.
    pub files: Vec<CandidateFile>,
}

/// The candidate being downloaded, as a job shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// The Soulseek user.
    pub peer: String,
    /// The folder's audio format.
    pub format: Format,
    /// As the peer reported them.
    pub bit_depth: Option<u32>,
    /// Hz.
    pub sample_rate: Option<u32>,
    /// kbps.
    pub bitrate: Option<u32>,
    /// Whether the bitrate is variable.
    pub variable_bitrate: Option<bool>,
    /// The number of files.
    pub file_count: u64,
    /// Their bytes together.
    pub bytes: u64,
    /// The index into the snapshot's tracklists the files matched.
    pub tracklist: usize,
}

/// A job's file: the candidate's until staged, the stager's from then on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// The remote name under the candidate folder until staged; the path under
    /// `staged_path` after. `/`-separated.
    pub name: String,
    /// Bytes.
    pub size: u64,
    /// The matched release position, or `None` for an extra file.
    pub position: Option<String>,
}

/// A download's progress while `Downloading`, or its copy into staging while `Downloaded`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Files complete.
    pub files_done: u64,
    /// Files in the candidate.
    pub files_total: u64,
    /// Bytes received, or copied.
    pub bytes_done: u64,
    /// Bytes in the candidate.
    pub bytes_total: u64,
}

/// The query a `Searching` job's search is running, and what slskd has collected for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    /// The search text sent to slskd.
    pub query: String,
    /// Which of the attempt's queries it is, from 1.
    pub number: u32,
    /// How many queries the attempt has.
    pub count: u32,
    /// Peers that have answered so far.
    pub peers: u64,
    /// The files in their answers.
    pub files: u64,
    /// When slskd started the query.
    pub started_at: i64,
}

/// A lease or claim: who holds it and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    /// The Mac's name.
    pub machine: String,
    /// When it lapses.
    pub expires_at: i64,
}

/// The player's tagging report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tagging {
    /// Where the tag run is.
    pub status: TaggingStatus,
    /// For a discrepancy or an error.
    pub reasons: Vec<String>,
}

/// A job as it is now (api.md, The job).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Never reused.
    pub id: i64,
    /// The store's revision when the job last changed.
    pub revision: i64,
    /// Where it came from.
    pub source: Source,
    /// Which audio it accepts.
    pub format_policy: Policy,
    /// The snapshot's summary.
    pub release: Release,
    /// Its lifecycle state.
    pub state: State,
    /// One line for the window.
    pub reason: Option<String>,
    /// Search attempts so far, from 1.
    pub attempt: u32,
    /// With `NotFound`: when the next search runs.
    pub next_attempt_at: Option<i64>,
    /// The searches a dropped peer started since the job was queued or retried. Not in the
    /// API's job.
    pub searched_again: u32,
    /// With `Downloading`, and with `Downloaded` once the copy into staging starts.
    pub progress: Option<Progress>,
    /// With `Searching`, once its first query starts.
    pub search: Option<Search>,
    /// The candidate being downloaded.
    pub candidate: Option<Chosen>,
    /// In release order.
    pub files: Vec<File>,
    /// From `Staged` on: the folder under the staging root.
    pub staged_path: Option<String>,
    /// The player's report.
    pub tagging: Tagging,
    /// The live tagging lease.
    pub lease: Option<Hold>,
    /// The live review claim.
    pub review: Option<Hold>,
    /// For an upgrade.
    pub upgrade: Option<Upgrade>,
    /// A rejected or cancelled job's files are still being deleted.
    pub cleanup_pending: bool,
    /// When it was created.
    pub created_at: i64,
    /// When it last changed.
    pub updated_at: i64,
}

/// A page of `list`.
#[derive(Debug, PartialEq, Eq)]
pub struct Listing {
    /// The store's epoch.
    pub epoch: String,
    /// The store's current revision.
    pub revision: i64,
    /// The client must drop its rows; `jobs` pages from the start.
    pub reset: bool,
    /// Jobs remain past the last one in `jobs`.
    pub more: bool,
    /// Ascending by revision.
    pub jobs: Vec<Job>,
}

/// A report from the player for one job (api.md, handoff).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// Take or renew the tagging lease.
    Lease,
    /// Give up this machine's lease.
    ReleaseLease,
    /// Record the tagging; needs the lease.
    Tagging(Tagging),
    /// Take or renew the review claim; `take_over` resumes another Mac's import.
    Claim {
        /// The window's Resume Here.
        take_over: bool,
    },
    /// Give up this machine's review claim.
    Release,
    /// The owner approved it.
    Approved,
    /// The import started.
    Importing,
    /// The import is done.
    Imported,
    /// The owner rejected it.
    Rejected,
    /// A player stage failed.
    Failed {
        /// `Tag`, `Move`, `Delete` or `Carry`.
        stage: Stage,
        /// One line for the window.
        reason: String,
    },
}

/// Why a request was refused, for the API's error codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No job has this id.
    NoSuchJob,
    /// The job's state doesn't allow it.
    Illegal(State),
    /// Another machine holds the lease or claim it needs.
    Claimed(Hold),
    /// No one holds the lease or claim it needs.
    NotClaimed,
    /// A job for this release that isn't terminal exists; its id.
    AlreadyActive(i64),
    /// An upgrade needs a lossless policy; the one refused.
    LossyUpgrade(Policy),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchJob => f.write_str("no job has that id"),
            Self::Illegal(state) => write!(f, "a {} job can't take that", state.name()),
            Self::Claimed(hold) => write!(f, "{} holds this job", hold.machine),
            Self::NotClaimed => {
                f.write_str("no one holds the lease or claim this needs; take it first")
            }
            Self::AlreadyActive(id) => write!(f, "job {id} for this release isn't done"),
            Self::LossyUpgrade(policy) => {
                write!(
                    f,
                    "an upgrade needs a lossless policy, not {}",
                    policy.name()
                )
            }
        }
    }
}

/// An answer that may be refused.
pub type Answer<T> = Result<T, Refusal>;

/// A store's result: a database failure outside, a refusal inside.
pub type Outcome<T> = Result<Answer<T>, DatabaseError>;

/// An open downloads store.
pub struct Downloads {
    path: PathBuf,
    connection: Connection,
    epoch: String,
}

impl Downloads {
    /// Opens the store in `data_dir`, creating the file, its tables and its epoch if they
    /// don't exist. A file of another shape is started again with a new epoch, and a line
    /// says so on stderr.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, or the file can't be opened or set up.
    pub fn open(data_dir: &Path) -> Result<Self, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let spec = sqlite::Spec {
            file_name: FILE_NAME,
            lock_file: OPEN_LOCK_FILE,
            schema: SCHEMA,
            expected: &EXPECTED,
        };
        let opened = sqlite::open(data_dir, &spec, sqlite::BUSY_TIMEOUT)
            .map_err(|error| DatabaseError::new(NAME, &path, error.boxed()))?;
        if opened.reset {
            eprintln!(
                "downloads database {}: its tables are from another version, so the file was \
                 deleted and started again with no jobs",
                path.display()
            );
        }
        let mut connection = opened.connection;
        let fail = |error| failure(&path, error);
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(fail)?;
        let existing: Option<String> = meta(&transaction, EPOCH).map_err(fail)?;
        let epoch = if let Some(epoch) = existing {
            epoch
        } else {
            let epoch = format!("{:016x}", RandomState::new().hash_one(FILE_NAME));
            transaction
                .execute(SET_META, params![EPOCH, epoch])
                .map_err(fail)?;
            transaction
                .execute(SET_META, params![REVISION, 0])
                .map_err(fail)?;
            transaction
                .execute(SET_META, params![PURGE_REVISION, 0])
                .map_err(fail)?;
            epoch
        };
        transaction.commit().map_err(fail)?;
        Ok(Self {
            path,
            connection,
            epoch,
        })
    }

    /// Takes the exclusive lock the one download runner holds, on `downloads.lock` in
    /// `data_dir`, so two runners can't search and enqueue at once. It doesn't wait.
    ///
    /// # Errors
    ///
    /// When another process holds it, or the lock file can't be opened or locked.
    pub fn lock(data_dir: &Path) -> Result<SyncLock, LockError> {
        try_lock(data_dir.join(RUNNER_LOCK_FILE))
    }

    /// Takes the exclusive lock the one stager holds, on `staging.lock` in `data_dir`, so
    /// two stagers can't copy one job at once. It doesn't wait.
    ///
    /// # Errors
    ///
    /// When another process holds it, or the lock file can't be opened or locked.
    pub fn stager_lock(data_dir: &Path) -> Result<SyncLock, LockError> {
        try_lock(data_dir.join(STAGER_LOCK_FILE))
    }

    /// The ids of the jobs in `state`, oldest first.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn ids(&self, state: State) -> Result<Vec<i64>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare("SELECT id FROM job WHERE state = ?1 AND failed_stage IS ?2 ORDER BY id")
            .map_err(fail)?;
        statement
            .query_map(
                params![state.name(), state.failed_stage().map(Stage::name)],
                |row| row.get(0),
            )
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// The epoch, made with the file.
    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// Queues a release, or refuses with `AlreadyActive` when a job for it isn't terminal.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn create(&mut self, job: &NewJob, now: i64) -> Outcome<Job> {
        let path = self.path.clone();
        if job.upgrade.is_some() && !job.format_policy.is_lossless() {
            return Ok(Err(Refusal::LossyUpgrade(job.format_policy)));
        }
        self.write(|tx| {
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM job WHERE active_release = ?1",
                    [job.release.release_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = active {
                return Ok(Err(Refusal::AlreadyActive(id)));
            }
            let revision = bump(tx)?;
            tx.execute(
                "INSERT INTO job (revision, release_id, active_release, artist, title, year,
                     source, format_policy, state, attempt, searched_again, tagging,
                     upgrade_quality, cleanup_pending, created_at, updated_at)
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, 0, ?9, ?10, 0, ?11, ?11)",
                params![
                    revision,
                    job.release.release_id,
                    job.release.artist,
                    job.release.title,
                    job.release.year,
                    job.source.name(),
                    job.format_policy.name(),
                    State::Queued.name(),
                    TaggingStatus::None.name(),
                    job.upgrade.as_ref().map(|u| &u.current_quality),
                    now
                ],
            )?;
            let id = tx.last_insert_rowid();
            insert_tracklists(tx, id, &job.tracklists)?;
            for (seq, old) in job.upgrade.iter().flat_map(|u| &u.old_paths).enumerate() {
                tx.execute(
                    "INSERT INTO old_path (job_id, seq, path) VALUES (?1, ?2, ?3)",
                    params![id, seq, old],
                )?;
            }
            log(tx, id, now, revision, State::Queued, None)?;
            read_job(tx, id, now).map(|job| job.ok_or(Refusal::NoSuchJob))
        })
        .map_err(|error| failure(&path, error))
    }

    /// The job with this id as it is at `now`, or `None`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn job(&self, id: i64, now: i64) -> Result<Option<Job>, DatabaseError> {
        read_job(&self.connection, id, now).map_err(|error| failure(&self.path, error))
    }

    /// The job's snapshot tracklists.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn tracklists(&self, id: i64) -> Result<Vec<Vec<Track>>, DatabaseError> {
        read_tracklists(&self.connection, id).map_err(|error| failure(&self.path, error))
    }

    /// The candidates the search kept, best first.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn candidates(&self, id: i64) -> Result<Vec<Candidate>, DatabaseError> {
        read_candidates(&self.connection, id).map_err(|error| failure(&self.path, error))
    }

    /// The candidate being downloaded, with its files' remote paths, or `None` before one is
    /// chosen.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn chosen(&self, id: i64) -> Result<Option<Candidate>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let chosen: Option<i64> = self
            .connection
            .query_row("SELECT chosen FROM job WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(fail)?
            .flatten();
        let Some(rank) = chosen.and_then(|rank| usize::try_from(rank).ok()) else {
            return Ok(None);
        };
        Ok(self.candidates(id)?.into_iter().nth(rank))
    }

    /// The jobs changed after revision `after`, ascending, at most [`PAGE`]. `epoch` is the
    /// client's from its last reply, `None` on its first request. A reset, when the epoch
    /// isn't this store's, `after` is past the store's revision, or a purge happened after
    /// `after`, pages from the start.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn list(
        &self,
        epoch: Option<&str>,
        after: i64,
        now: i64,
    ) -> Result<Listing, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let revision: i64 = meta(&self.connection, REVISION).map_err(fail)?.unwrap_or(0);
        let purged: i64 = meta(&self.connection, PURGE_REVISION)
            .map_err(fail)?
            .unwrap_or(0);
        // An `after` of 0 already pages from the start, so a purge doesn't reset it; that
        // also keeps an empty store from resetting every poll.
        let reset = epoch.is_some_and(|epoch| {
            epoch != self.epoch || after > revision || (after > 0 && after < purged)
        });
        let from = if reset { 0 } else { after };
        let mut statement = self
            .connection
            .prepare("SELECT id FROM job WHERE revision > ?1 ORDER BY revision LIMIT ?2")
            .map_err(fail)?;
        let ids = statement
            .query_map(params![from, PAGE + 1], |row| row.get::<_, i64>(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(fail)?;
        let more = ids.len() > PAGE;
        let mut jobs = Vec::with_capacity(PAGE.min(ids.len()));
        for id in ids.into_iter().take(PAGE) {
            if let Some(job) = read_job(&self.connection, id, now).map_err(fail)? {
                jobs.push(job);
            }
        }
        Ok(Listing {
            epoch: self.epoch.clone(),
            revision,
            reset,
            more,
            jobs,
        })
    }

    /// Applies one of the service's events that carry nothing else: `Search`, `Requeue`,
    /// `Download`, `Downloaded` or `Cancel`. `Requeue` counts a new attempt, except from
    /// `Searching`, where the same attempt starts again, and clears the candidates.
    /// `Cancel` marks cleanup for a job that may have files.
    ///
    /// # Errors
    ///
    /// When the write fails.
    ///
    /// # Panics
    ///
    /// On another event: each of those has its own method, which stores what it carries,
    /// and the player's go through `handoff`, which checks the claim.
    pub fn transition(&mut self, id: i64, event: Event, now: i64) -> Outcome<Job> {
        assert!(
            matches!(
                event,
                Event::Search
                    | Event::Requeue
                    | Event::Download
                    | Event::Downloaded
                    | Event::Cancel
            ),
            "{event:?} has its own method"
        );
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, event) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            if next == job.state {
                return Ok(Ok(false));
            }
            if event == Event::Requeue {
                let attempt = if job.state == State::Searching {
                    job.attempt
                } else {
                    job.attempt + 1
                };
                clear_search(tx, id, attempt)?;
            }
            if event == Event::Cancel {
                let may_have_files = !matches!(
                    job.state,
                    State::Queued
                        | State::Searching
                        | State::NotFound
                        | State::Failed(Stage::Search)
                );
                // A requeued job may still hold its last staged copy.
                tx.execute(
                    "UPDATE job SET cleanup_pending = ?2 WHERE id = ?1",
                    params![id, may_have_files || job.cleanup_pending],
                )?;
            }
            // The last attempt's miss stays on the way back to the queue and through the
            // next search, so the window can say why it is searching again.
            let carried = matches!(
                (event, job.state),
                (Event::Search, State::Queued)
                    | (Event::Requeue, State::Searching | State::NotFound)
            );
            set_state(tx, id, next, job.reason.as_deref().filter(|_| carried))?;
            Ok(Ok(true))
        })
    }

    /// The search kept no candidate: `NotFound`, with the reason and the next attempt's
    /// time (`None` after the last).
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn not_found(
        &mut self,
        id: i64,
        reason: &str,
        next_attempt_at: Option<i64>,
        now: i64,
    ) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::NotFound) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            set_state(tx, id, next, Some(reason))?;
            tx.execute(
                "UPDATE job SET next_attempt_at = ?2 WHERE id = ?1",
                params![id, next_attempt_at],
            )?;
            Ok(Ok(true))
        })
    }

    /// The search kept `candidates`, best first, of which the first [`MAX_CANDIDATES`] are
    /// stored and the first is chosen: `CandidateFound`.
    ///
    /// # Errors
    ///
    /// When the write fails.
    ///
    /// # Panics
    ///
    /// When `candidates` is empty: that is `not_found`.
    pub fn found(&mut self, id: i64, candidates: &[Candidate], now: i64) -> Outcome<Job> {
        assert!(!candidates.is_empty(), "found with no candidates");
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::Found) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            clear_search(tx, id, job.attempt)?;
            for (rank, candidate) in candidates.iter().take(MAX_CANDIDATES).enumerate() {
                insert_candidate(tx, id, rank, candidate)?;
            }
            tx.execute("UPDATE job SET chosen = 0 WHERE id = ?1", [id])?;
            set_state(tx, id, next, None)?;
            Ok(Ok(true))
        })
    }

    /// The chosen candidate failed: the next stored one is chosen and the job is
    /// `CandidateFound` again. `None`, with the job unchanged, when none is left.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn next_candidate(&mut self, id: i64, now: i64) -> Outcome<Option<Job>> {
        let mut left = true;
        let outcome = self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::NextCandidate) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            let chosen: Option<i64> =
                tx.query_row("SELECT chosen FROM job WHERE id = ?1", [id], |row| {
                    row.get(0)
                })?;
            let following = chosen.map_or(0, |rank| rank + 1);
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM candidate WHERE job_id = ?1 AND rank = ?2)",
                params![id, following],
                |row| row.get(0),
            )?;
            if !exists {
                left = false;
                return Ok(Ok(false));
            }
            tx.execute(
                "UPDATE job SET chosen = ?2, files_done = NULL, files_total = NULL,
                     bytes_done = NULL, bytes_total = NULL WHERE id = ?1",
                params![id, following],
            )?;
            set_state(tx, id, next, None)?;
            Ok(Ok(true))
        })?;
        Ok(outcome.map(|job| left.then_some(job)))
    }

    /// The chosen peer stalled or went offline and no stored candidate is left: `NotFound`,
    /// with the reason and when the search runs again, and one more search again counted.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn search_again(
        &mut self,
        id: i64,
        reason: &str,
        next_attempt_at: i64,
        now: i64,
    ) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::Dropped) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            set_state(tx, id, next, Some(reason))?;
            tx.execute(
                "UPDATE job SET next_attempt_at = ?2, searched_again = searched_again + 1
                 WHERE id = ?1",
                params![id, next_attempt_at],
            )?;
            Ok(Ok(true))
        })
    }

    /// The player's report that a `Staged` job, or one failed at `tag`, holds another
    /// release than the one queued: the job's release and tracklists become `release` and
    /// `tracklists`. Its files, and their positions, stay as they were matched. Refused with
    /// `AlreadyActive` when another job that isn't terminal is for that release.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set_release(
        &mut self,
        id: i64,
        release: &Release,
        tracklists: &[Vec<Track>],
        now: i64,
    ) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            if next_state(job.state, Event::NewRelease).is_none() {
                return Ok(Err(Refusal::Illegal(job.state)));
            }
            let other: Option<i64> = tx
                .query_row(
                    "SELECT id FROM job WHERE active_release = ?1 AND id != ?2",
                    params![release.release_id, id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(other) = other {
                return Ok(Err(Refusal::AlreadyActive(other)));
            }
            if job.release == *release && read_tracklists(tx, id)? == tracklists {
                return Ok(Ok(false));
            }
            tx.execute(
                "UPDATE job SET release_id = ?2, active_release = ?2, artist = ?3, title = ?4,
                     year = ?5 WHERE id = ?1",
                params![
                    id,
                    release.release_id,
                    release.artist,
                    release.title,
                    release.year
                ],
            )?;
            tx.execute("DELETE FROM track WHERE job_id = ?1", [id])?;
            insert_tracklists(tx, id, tracklists)?;
            Ok(Ok(true))
        })
    }

    /// The service's stage failed: `Failed(stage)` with the reason.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn fail(&mut self, id: i64, stage: Stage, reason: &str, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::Fail(stage)) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            if next == job.state && job.reason.as_deref() == Some(reason) {
                return Ok(Ok(false));
            }
            set_state(tx, id, next, Some(reason))?;
            Ok(Ok(true))
        })
    }

    /// The owner's retry (api.md): back to the state the failed stage resumes from. A new
    /// `format_policy` is only for a retry that searches again, and an upgrade's must be
    /// lossless.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn retry(&mut self, id: i64, format_policy: Option<Policy>, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            if let Some(policy) = format_policy
                && job.source == Source::Upgrade
                && !policy.is_lossless()
            {
                return Ok(Err(Refusal::LossyUpgrade(policy)));
            }
            let Some(next) = next_state(job.state, Event::Retry) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            match next {
                State::Queued => {
                    clear_search(tx, id, job.attempt + 1)?;
                    tx.execute("UPDATE job SET searched_again = 0 WHERE id = ?1", [id])?;
                    if let Some(policy) = format_policy {
                        tx.execute(
                            "UPDATE job SET format_policy = ?2 WHERE id = ?1",
                            params![id, policy.name()],
                        )?;
                    }
                }
                _ if format_policy.is_some() => return Ok(Err(Refusal::Illegal(job.state))),
                State::Staged => {
                    set_tagging(
                        tx,
                        id,
                        &Tagging {
                            status: TaggingStatus::None,
                            reasons: Vec::new(),
                        },
                    )?;
                }
                _ => {}
            }
            let carried = job.state == State::NotFound;
            set_state(tx, id, next, job.reason.as_deref().filter(|_| carried))?;
            Ok(Ok(true))
        })
    }

    /// The owner's requeue (api.md): a `Staged` job goes back to `Queued` with its release
    /// and policy, to be searched again from a peer not tried yet. `attempt` stays: the
    /// search it found the copy in succeeded, so the miss schedule isn't spent on it. Its staged files stay as the ledger the runner deletes them by, with
    /// `cleanup_pending`, as for a rejected job. Refused with `Claimed` while anyone holds
    /// a live lease or review claim: the request names no machine, so the holder gives it
    /// up first.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn requeue(&mut self, id: i64, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::Discard) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            if let Some(hold) = job.lease.as_ref().or(job.review.as_ref()) {
                return Ok(Err(Refusal::Claimed(hold.clone())));
            }
            clear_search(tx, id, job.attempt)?;
            tx.execute(
                "UPDATE job SET searched_again = 0, staged_path = NULL, cleanup_pending = 1
                 WHERE id = ?1",
                [id],
            )?;
            set_tagging(
                tx,
                id,
                &Tagging {
                    status: TaggingStatus::None,
                    reasons: Vec::new(),
                },
            )?;
            clear_hold(tx, id, "lease")?;
            clear_hold(tx, id, "review")?;
            set_state(tx, id, next, None)?;
            Ok(Ok(true))
        })
    }

    /// The download's progress while `Downloading`, or the copy's while `Downloaded`.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn progress(&mut self, id: i64, progress: Progress, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            if !matches!(job.state, State::Downloading | State::Downloaded) {
                return Ok(Err(Refusal::Illegal(job.state)));
            }
            if job.progress == Some(progress) {
                return Ok(Ok(false));
            }
            tx.execute(
                "UPDATE job SET files_done = ?2, files_total = ?3, bytes_done = ?4,
                     bytes_total = ?5 WHERE id = ?1",
                params![
                    id,
                    progress.files_done,
                    progress.files_total,
                    progress.bytes_done,
                    progress.bytes_total
                ],
            )?;
            Ok(Ok(true))
        })
    }

    /// What a `Searching` job's running query has collected so far.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn search(&mut self, id: i64, search: &Search, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            if job.state != State::Searching {
                return Ok(Err(Refusal::Illegal(job.state)));
            }
            if job.search.as_ref() == Some(search) {
                return Ok(Ok(false));
            }
            tx.execute(
                "UPDATE job SET search_query = ?2, search_number = ?3, search_count = ?4,
                     search_peers = ?5, search_files = ?6, search_started_at = ?7
                 WHERE id = ?1",
                params![
                    id,
                    search.query,
                    search.number,
                    search.count,
                    search.peers,
                    search.files,
                    search.started_at
                ],
            )?;
            Ok(Ok(true))
        })
    }

    /// The stager copied and checked the files: `Staged`, under `staged_path`, with `files`
    /// as the job's files from now on.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn staged(&mut self, id: i64, staged_path: &str, files: &[File], now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            let Some(next) = next_state(job.state, Event::Staged) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            tx.execute("DELETE FROM staged_file WHERE job_id = ?1", [id])?;
            for (seq, file) in files.iter().enumerate() {
                tx.execute(
                    "INSERT INTO staged_file (job_id, seq, name, size, position)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, seq, file.name, file.size, file.position],
                )?;
            }
            tx.execute(
                "UPDATE job SET staged_path = ?2 WHERE id = ?1",
                params![id, staged_path],
            )?;
            set_state(tx, id, next, None)?;
            Ok(Ok(true))
        })
    }

    /// The player's report for a job, with the lease and claim checks of api.md's handoff
    /// table, in one transaction so two Macs can't both pass a check.
    ///
    /// # Errors
    ///
    /// When the write fails.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per row of api.md's handoff table"
    )]
    pub fn handoff(&mut self, id: i64, machine: &str, handoff: &Handoff, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            let holds = |hold: &Option<Hold>| -> Answer<()> {
                match hold {
                    Some(hold) if hold.machine == machine => Ok(()),
                    Some(hold) => Err(Refusal::Claimed(hold.clone())),
                    None => Err(Refusal::NotClaimed),
                }
            };
            let free_or_mine = |hold: &Option<Hold>| -> Answer<()> {
                match hold {
                    Some(hold) if hold.machine != machine => Err(Refusal::Claimed(hold.clone())),
                    _ => Ok(()),
                }
            };
            let event = match handoff {
                Handoff::Lease => {
                    if job.state != State::Staged {
                        return Ok(Err(Refusal::Illegal(job.state)));
                    }
                    if let Err(refusal) = free_or_mine(&job.lease) {
                        return Ok(Err(refusal));
                    }
                    set_hold(tx, id, "lease", machine, now + secs(LEASE))?;
                    return Ok(Ok(true));
                }
                Handoff::ReleaseLease => {
                    if holds(&job.lease).is_err() {
                        return Ok(Ok(false));
                    }
                    clear_hold(tx, id, "lease")?;
                    return Ok(Ok(true));
                }
                Handoff::Tagging(tagging) => {
                    if job.state != State::Staged {
                        return Ok(Err(Refusal::Illegal(job.state)));
                    }
                    if let Err(refusal) = holds(&job.lease) {
                        return Ok(Err(refusal));
                    }
                    if job.tagging == *tagging {
                        return Ok(Ok(false));
                    }
                    set_tagging(tx, id, tagging)?;
                    return Ok(Ok(true));
                }
                Handoff::Claim { take_over } => {
                    if !matches!(
                        job.state,
                        State::Staged | State::Approved | State::Importing
                    ) {
                        return Ok(Err(Refusal::Illegal(job.state)));
                    }
                    let resumable = matches!(job.state, State::Approved | State::Importing);
                    if !(*take_over && resumable)
                        && let Err(refusal) = free_or_mine(&job.review)
                    {
                        return Ok(Err(refusal));
                    }
                    set_hold(tx, id, "review", machine, now + secs(REVIEW_CLAIM))?;
                    return Ok(Ok(true));
                }
                Handoff::Release => {
                    if holds(&job.review).is_err() {
                        return Ok(Ok(false));
                    }
                    clear_hold(tx, id, "review")?;
                    return Ok(Ok(true));
                }
                Handoff::Approved => Event::Approved,
                Handoff::Importing => Event::Importing,
                Handoff::Imported => Event::Imported,
                Handoff::Rejected => Event::Rejected,
                Handoff::Failed { stage, .. } => {
                    if matches!(stage, Stage::Search | Stage::Download | Stage::Stage) {
                        return Ok(Err(Refusal::Illegal(job.state)));
                    }
                    Event::Fail(*stage)
                }
            };
            let Some(next) = next_state(job.state, event) else {
                return Ok(Err(Refusal::Illegal(job.state)));
            };
            if next == job.state {
                // A repeat: answered unchanged unless another machine holds the job now.
                return Ok(free_or_mine(&job.review).map(|()| false));
            }
            if event == Event::Approved && job.tagging.status != TaggingStatus::Clean {
                return Ok(Err(Refusal::Illegal(job.state)));
            }
            let hold = if event == Event::Fail(Stage::Tag) {
                &job.lease
            } else {
                &job.review
            };
            if let Err(refusal) = holds(hold) {
                return Ok(Err(refusal));
            }
            let reason = match handoff {
                Handoff::Failed { reason, .. } => Some(reason.as_str()),
                _ => None,
            };
            if event == Event::Rejected {
                tx.execute("UPDATE job SET cleanup_pending = 1 WHERE id = ?1", [id])?;
            }
            set_state(tx, id, next, reason)?;
            Ok(Ok(true))
        })
    }

    /// Adds a path the job's files were adopted at under slskd's downloads folder. A path
    /// already there is kept once.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn ledger_add(&mut self, id: i64, path: &str) -> Result<(), DatabaseError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO ledger (job_id, path) VALUES (?1, ?2)",
                params![id, path],
            )
            .map(drop)
            .map_err(|error| failure(&self.path, error))
    }

    /// The job's ledgered paths, sorted.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn ledger(&self, id: i64) -> Result<Vec<String>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare("SELECT path FROM ledger WHERE job_id = ?1 ORDER BY path")
            .map_err(fail)?;
        statement
            .query_map([id], |row| row.get(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// The jobs with a ledgered path inside `folder`, its subfolders included, by id.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn ledgered_in(&self, folder: &str) -> Result<Vec<i64>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let prefix = format!("{}/", folder.trim_end_matches('/'));
        let mut statement = self
            .connection
            .prepare(
                "SELECT DISTINCT job_id FROM ledger WHERE substr(path, 1, length(?1)) = ?1
                 ORDER BY job_id",
            )
            .map_err(fail)?;
        statement
            .query_map([prefix], |row| row.get(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// Removes a ledgered path once its file is deleted.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn ledger_remove(&mut self, id: i64, path: &str) -> Result<(), DatabaseError> {
        self.connection
            .execute(
                "DELETE FROM ledger WHERE job_id = ?1 AND path = ?2",
                params![id, path],
            )
            .map(drop)
            .map_err(|error| failure(&self.path, error))
    }

    /// Adds `peer` to the job's tried peers, as its download is about to be requested. A
    /// peer already there keeps its place.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn add_tried_peer(&mut self, id: i64, peer: &str) -> Result<(), DatabaseError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO tried_peer (job_id, seq, peer) VALUES (?1,
                     (SELECT COALESCE(MAX(seq), -1) + 1 FROM tried_peer WHERE job_id = ?1), ?2)",
                params![id, peer],
            )
            .map(drop)
            .map_err(|error| failure(&self.path, error))
    }

    /// The peers the job's downloads were started from, in the order first tried, or
    /// `NoSuchJob`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn tried_peers(&self, id: i64) -> Outcome<Vec<String>> {
        let fail = |error| failure(&self.path, error);
        let exists: bool = self
            .connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM job WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(fail)?;
        if !exists {
            return Ok(Err(Refusal::NoSuchJob));
        }
        let mut statement = self
            .connection
            .prepare("SELECT peer FROM tried_peer WHERE job_id = ?1 ORDER BY seq")
            .map_err(fail)?;
        statement
            .query_map([id], |row| row.get(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map(Ok)
            .map_err(fail)
    }

    /// The names the stager last copied for the job, under its staging folder, in release
    /// order: the files a requeued job's cleanup deletes, which its `files` no longer show.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn staged_files(&self, id: i64) -> Result<Vec<String>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare("SELECT name FROM staged_file WHERE job_id = ?1 ORDER BY seq")
            .map_err(fail)?;
        statement
            .query_map([id], |row| row.get(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// The jobs whose files are still to be deleted, oldest first.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn cleanup_pending(&self) -> Result<Vec<i64>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare("SELECT id FROM job WHERE cleanup_pending = 1 ORDER BY id")
            .map_err(fail)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// The job's files are gone: clears `cleanup_pending`, and a requeued job's ledger of
    /// the staged copy it deleted and the reason [`Self::cleanup_held`] gave.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn cleanup_done(&mut self, id: i64, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            if !job.cleanup_pending {
                return Ok(Ok(false));
            }
            tx.execute("UPDATE job SET cleanup_pending = 0 WHERE id = ?1", [id])?;
            if job.staged_path.is_none() {
                tx.execute("DELETE FROM staged_file WHERE job_id = ?1", [id])?;
            }
            if job.state == State::Queued {
                tx.execute("UPDATE job SET reason = NULL WHERE id = ?1", [id])?;
            }
            Ok(Ok(true))
        })
    }

    /// A requeued job's old staged copy can't be deleted yet: `reason` says why, so the
    /// window shows why the job isn't searched. Refused for a job that isn't `Queued` with
    /// `cleanup_pending`; a rejected or cancelled job keeps its own reason.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn cleanup_held(&mut self, id: i64, reason: &str, now: i64) -> Outcome<Job> {
        self.change(id, now, |tx, job| {
            if job.state != State::Queued || !job.cleanup_pending {
                return Ok(Err(Refusal::Illegal(job.state)));
            }
            if job.reason.as_deref() == Some(reason) {
                return Ok(Ok(false));
            }
            tx.execute(
                "UPDATE job SET reason = ?2 WHERE id = ?1",
                params![id, reason],
            )?;
            Ok(Ok(true))
        })
    }

    /// The events of a job: each state it entered, oldest first, as (time, state, reason).
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn events(&self, id: i64) -> Result<Vec<(i64, String, Option<String>)>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare("SELECT at, state, reason FROM event WHERE job_id = ?1 ORDER BY id")
            .map_err(fail)?;
        statement
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// Deletes the terminal jobs that became terminal [`TERMINAL_KEPT`] or more before `now`
    /// and have nothing left to clean up, and returns how many. A purge takes a revision and
    /// renumbers the remaining jobs above it, so a client holding a purged row resets once
    /// and, paging through, is past the purge.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn purge(&mut self, now: i64) -> Result<usize, DatabaseError> {
        let path = self.path.clone();
        self.write(|tx| {
            let cutoff = now - secs(TERMINAL_KEPT);
            let ids = tx
                .prepare(
                    "SELECT id FROM job WHERE terminal_at <= ?1 AND cleanup_pending = 0
                     ORDER BY id",
                )?
                .query_map([cutoff], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if ids.is_empty() {
                return Ok(0);
            }
            for &id in &ids {
                for table in JOB_TABLES {
                    tx.execute(&format!("DELETE FROM {table} WHERE job_id = ?1"), [id])?;
                }
                tx.execute("DELETE FROM job WHERE id = ?1", [id])?;
            }
            let revision = bump(tx)?;
            tx.execute(SET_META, params![PURGE_REVISION, revision])?;
            // Every survivor takes a revision above the purge, in its old order, so a
            // client that resets pages through at cursors past the purge and never
            // resets twice.
            let survivors = tx
                .prepare("SELECT id FROM job ORDER BY revision")?
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in survivors {
                let revision = bump(tx)?;
                tx.execute(
                    "UPDATE job SET revision = ?2 WHERE id = ?1",
                    params![id, revision],
                )?;
            }
            Ok(ids.len())
        })
        .map_err(|error| failure(&path, error))
    }

    /// Runs `body` in an immediate transaction and commits it.
    fn write<T>(
        &mut self,
        body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let value = body(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }

    /// Reads job `id` at `now`, runs `body` on it, and, when it answers `true`, gives the
    /// job the next revision and `now` as its change time. A refusal or `false` changes
    /// nothing. Answers the job as it then is.
    fn change(
        &mut self,
        id: i64,
        now: i64,
        body: impl FnOnce(&Transaction<'_>, &Job) -> rusqlite::Result<Answer<bool>>,
    ) -> Outcome<Job> {
        let path = self.path.clone();
        self.write(|tx| {
            let Some(job) = read_job(tx, id, now)? else {
                return Ok(Err(Refusal::NoSuchJob));
            };
            let changed = match body(tx, &job)? {
                Ok(changed) => changed,
                Err(refusal) => return Ok(Err(refusal)),
            };
            if !changed {
                return Ok(Ok(job));
            }
            let revision = bump(tx)?;
            tx.execute(
                "UPDATE job SET revision = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, revision, now],
            )?;
            let state: (String, Option<String>, Option<String>) = tx.query_row(
                "SELECT state, failed_stage, reason FROM job WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let now_state = State::parse(&state.0, state.1.as_deref())
                .ok_or_else(|| rusqlite::Error::InvalidColumnName(format!("state {}", state.0)))?;
            if now_state != job.state {
                log(tx, id, now, revision, now_state, state.2.as_deref())?;
                // No state leaves a terminal one, so this is set once.
                if now_state.is_terminal() {
                    tx.execute(
                        "UPDATE job SET terminal_at = ?2, active_release = NULL WHERE id = ?1",
                        params![id, now],
                    )?;
                }
            }
            read_job(tx, id, now).map(|job| job.ok_or(Refusal::NoSuchJob))
        })
        .map_err(|error| failure(&path, error))
    }
}

/// A duration in whole seconds, as the store's times are.
fn secs(duration: Duration) -> i64 {
    duration.as_secs().cast_signed()
}

/// Takes the store's next revision.
fn bump(tx: &Transaction<'_>) -> rusqlite::Result<i64> {
    let revision = meta::<i64>(tx, REVISION)?.unwrap_or(0) + 1;
    tx.execute(SET_META, params![REVISION, revision])?;
    Ok(revision)
}

fn meta<T: rusqlite::types::FromSql>(
    connection: &Connection,
    key: &str,
) -> rusqlite::Result<Option<T>> {
    connection
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
}

/// Sets the state, its failed stage and the reason line.
fn set_state(
    tx: &Transaction<'_>,
    id: i64,
    state: State,
    reason: Option<&str>,
) -> rusqlite::Result<()> {
    let leaves_downloading = state != State::Downloading;
    if state != State::Searching {
        tx.execute(
            "UPDATE job SET search_query = NULL, search_number = NULL, search_count = NULL,
                 search_peers = NULL, search_files = NULL, search_started_at = NULL
             WHERE id = ?1",
            [id],
        )?;
    }
    tx.execute(
        "UPDATE job SET state = ?2, failed_stage = ?3, reason = ?4,
             next_attempt_at = CASE WHEN ?2 = ?5 THEN next_attempt_at END
         WHERE id = ?1",
        params![
            id,
            state.name(),
            state.failed_stage().map(Stage::name),
            reason,
            State::NotFound.name()
        ],
    )?;
    if leaves_downloading {
        tx.execute(
            "UPDATE job SET files_done = NULL, files_total = NULL, bytes_done = NULL,
                 bytes_total = NULL WHERE id = ?1",
            [id],
        )?;
    }
    Ok(())
}

/// Starts a search attempt afresh: the candidates, their files and the choice gone.
fn clear_search(tx: &Transaction<'_>, id: i64, attempt: u32) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM candidate WHERE job_id = ?1", [id])?;
    tx.execute("DELETE FROM candidate_file WHERE job_id = ?1", [id])?;
    tx.execute(
        "UPDATE job SET chosen = NULL, attempt = ?2, next_attempt_at = NULL WHERE id = ?1",
        params![id, attempt],
    )?;
    Ok(())
}

/// A job's tracklists, in their order.
fn read_tracklists(connection: &Connection, id: i64) -> rusqlite::Result<Vec<Vec<Track>>> {
    let mut statement = connection.prepare_cached(
        "SELECT tracklist, position, title, seconds FROM track WHERE job_id = ?1
         ORDER BY tracklist, seq",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, usize>(0)?,
            Track {
                position: row.get(1)?,
                title: row.get(2)?,
                seconds: row.get(3)?,
            },
        ))
    })?;
    let mut lists: Vec<Vec<Track>> = Vec::new();
    for row in rows {
        let (list, track) = row?;
        if lists.len() <= list {
            lists.resize_with(list + 1, Vec::new);
        }
        lists[list].push(track);
    }
    Ok(lists)
}

fn insert_tracklists(
    tx: &Transaction<'_>,
    id: i64,
    tracklists: &[Vec<Track>],
) -> rusqlite::Result<()> {
    for (list, tracks) in tracklists.iter().enumerate() {
        for (seq, track) in tracks.iter().enumerate() {
            tx.execute(
                "INSERT INTO track (job_id, tracklist, seq, position, title, seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, list, seq, track.position, track.title, track.seconds],
            )?;
        }
    }
    Ok(())
}

fn insert_candidate(
    tx: &Transaction<'_>,
    id: i64,
    rank: usize,
    candidate: &Candidate,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO candidate (job_id, rank, peer, format, bit_depth, sample_rate, bitrate,
             variable_bitrate, tracklist)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            rank,
            candidate.peer,
            candidate.format.name(),
            candidate.bit_depth,
            candidate.sample_rate,
            candidate.bitrate,
            candidate.variable_bitrate,
            candidate.tracklist
        ],
    )?;
    for (seq, file) in candidate.files.iter().enumerate() {
        tx.execute(
            "INSERT INTO candidate_file (job_id, rank, seq, remote, name, size, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                rank,
                seq,
                file.remote,
                file.name,
                file.size,
                file.position
            ],
        )?;
    }
    Ok(())
}

fn set_tagging(tx: &Transaction<'_>, id: i64, tagging: &Tagging) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE job SET tagging = ?2 WHERE id = ?1",
        params![id, tagging.status.name()],
    )?;
    tx.execute("DELETE FROM tagging_reason WHERE job_id = ?1", [id])?;
    for (seq, reason) in tagging.reasons.iter().enumerate() {
        tx.execute(
            "INSERT INTO tagging_reason (job_id, seq, reason) VALUES (?1, ?2, ?3)",
            params![id, seq, reason],
        )?;
    }
    Ok(())
}

/// Sets the `lease` or `review` hold.
fn set_hold(
    tx: &Transaction<'_>,
    id: i64,
    hold: &str,
    machine: &str,
    expires_at: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        &format!("UPDATE job SET {hold}_machine = ?2, {hold}_expires_at = ?3 WHERE id = ?1"),
        params![id, machine, expires_at],
    )
    .map(drop)
}

fn clear_hold(tx: &Transaction<'_>, id: i64, hold: &str) -> rusqlite::Result<()> {
    tx.execute(
        &format!("UPDATE job SET {hold}_machine = NULL, {hold}_expires_at = NULL WHERE id = ?1"),
        [id],
    )
    .map(drop)
}

/// Records that a job entered `state`.
fn log(
    tx: &Transaction<'_>,
    id: i64,
    at: i64,
    revision: i64,
    state: State,
    reason: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO event (job_id, at, revision, state, reason) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, at, revision, state.name(), reason],
    )
    .map(drop)
}

/// A column holding one of an enum's names, read back or refused as corrupt.
pub(crate) fn named<T>(value: &str, parse: impl Fn(&str) -> Option<T>) -> rusqlite::Result<T> {
    parse(value).ok_or_else(|| rusqlite::Error::InvalidColumnName(format!("unknown name {value}")))
}

/// A hold that has lapsed by `now` reads as none.
fn live(machine: Option<String>, expires_at: Option<i64>, now: i64) -> Option<Hold> {
    match (machine, expires_at) {
        (Some(machine), Some(expires_at)) if expires_at > now => Some(Hold {
            machine,
            expires_at,
        }),
        _ => None,
    }
}

fn read_candidates(connection: &Connection, id: i64) -> rusqlite::Result<Vec<Candidate>> {
    let mut statement = connection.prepare(
        "SELECT rank, peer, format, bit_depth, sample_rate, bitrate, variable_bitrate,
             tracklist FROM candidate WHERE job_id = ?1 ORDER BY rank",
    )?;
    let rows = statement
        .query_map([id], |row| {
            let format: String = row.get(2)?;
            Ok((
                row.get::<_, i64>(0)?,
                Candidate {
                    peer: row.get(1)?,
                    format: named(&format, Format::parse)?,
                    bit_depth: row.get(3)?,
                    sample_rate: row.get(4)?,
                    bitrate: row.get(5)?,
                    variable_bitrate: row.get(6)?,
                    tracklist: row.get(7)?,
                    files: Vec::new(),
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut files = connection.prepare(
        "SELECT remote, name, size, position FROM candidate_file
         WHERE job_id = ?1 AND rank = ?2 ORDER BY seq",
    )?;
    rows.into_iter()
        .map(|(rank, mut candidate)| {
            candidate.files = files
                .query_map(params![id, rank], |row| {
                    Ok(CandidateFile {
                        remote: row.get(0)?,
                        name: row.get(1)?,
                        size: row.get(2)?,
                        position: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(candidate)
        })
        .collect()
}

/// The job as it is at `now`, or `None`.
#[expect(
    clippy::too_many_lines,
    reason = "one column after another, in the job's order"
)]
fn read_job(connection: &Connection, id: i64, now: i64) -> rusqlite::Result<Option<Job>> {
    let row = connection
        .query_row(
            "SELECT revision, source, format_policy, release_id, artist, title, year, state,
                 failed_stage, reason, attempt, next_attempt_at, files_done, files_total,
                 bytes_done, bytes_total, chosen, staged_path, tagging, lease_machine,
                 lease_expires_at, review_machine, review_expires_at, upgrade_quality,
                 cleanup_pending, created_at, updated_at, search_query, search_number,
                 search_count, search_peers, search_files, search_started_at, searched_again
             FROM job WHERE id = ?1",
            [id],
            |row| {
                let source: String = row.get(1)?;
                let policy: String = row.get(2)?;
                let state: String = row.get(7)?;
                let failed_stage: Option<String> = row.get(8)?;
                let tagging: String = row.get(18)?;
                let files_done: Option<u64> = row.get(12)?;
                let progress = match files_done {
                    Some(files_done) => Some(Progress {
                        files_done,
                        files_total: row.get(13)?,
                        bytes_done: row.get(14)?,
                        bytes_total: row.get(15)?,
                    }),
                    None => None,
                };
                let query: Option<String> = row.get(27)?;
                let search = match query {
                    Some(query) => Some(Search {
                        query,
                        number: row.get(28)?,
                        count: row.get(29)?,
                        peers: row.get(30)?,
                        files: row.get(31)?,
                        started_at: row.get(32)?,
                    }),
                    None => None,
                };
                Ok((
                    Job {
                        id,
                        revision: row.get(0)?,
                        source: named(&source, Source::parse)?,
                        format_policy: named(&policy, Policy::parse)?,
                        release: Release {
                            release_id: row.get(3)?,
                            artist: row.get(4)?,
                            title: row.get(5)?,
                            year: row.get(6)?,
                        },
                        state: named(&state, |name| State::parse(name, failed_stage.as_deref()))?,
                        reason: row.get(9)?,
                        attempt: row.get(10)?,
                        next_attempt_at: row.get(11)?,
                        searched_again: row.get(33)?,
                        progress,
                        search,
                        candidate: None,
                        files: Vec::new(),
                        staged_path: row.get(17)?,
                        tagging: Tagging {
                            status: named(&tagging, TaggingStatus::parse)?,
                            reasons: Vec::new(),
                        },
                        lease: live(row.get(19)?, row.get(20)?, now),
                        review: live(row.get(21)?, row.get(22)?, now),
                        upgrade: row
                            .get::<_, Option<String>>(23)?
                            .map(|current_quality| Upgrade {
                                current_quality,
                                old_paths: Vec::new(),
                            }),
                        cleanup_pending: row.get(24)?,
                        created_at: row.get(25)?,
                        updated_at: row.get(26)?,
                    },
                    row.get::<_, Option<i64>>(16)?,
                ))
            },
        )
        .optional()?;
    let Some((mut job, chosen)) = row else {
        return Ok(None);
    };
    let strings = |sql: &str| -> rusqlite::Result<Vec<String>> {
        connection
            .prepare_cached(sql)?
            .query_map([id], |row| row.get(0))?
            .collect()
    };
    job.tagging.reasons =
        strings("SELECT reason FROM tagging_reason WHERE job_id = ?1 ORDER BY seq")?;
    if let Some(upgrade) = &mut job.upgrade {
        upgrade.old_paths = strings("SELECT path FROM old_path WHERE job_id = ?1 ORDER BY seq")?;
    }
    if let Some(rank) = chosen {
        let candidates = read_candidates(connection, id)?;
        if let Some(candidate) = usize::try_from(rank).ok().and_then(|r| candidates.get(r)) {
            job.candidate = Some(Chosen {
                peer: candidate.peer.clone(),
                format: candidate.format,
                bit_depth: candidate.bit_depth,
                sample_rate: candidate.sample_rate,
                bitrate: candidate.bitrate,
                variable_bitrate: candidate.variable_bitrate,
                file_count: candidate.files.len() as u64,
                bytes: candidate.files.iter().map(|f| f.size).sum(),
                tracklist: candidate.tracklist,
            });
            if job.staged_path.is_none() {
                job.files = candidate
                    .files
                    .iter()
                    .map(|file| File {
                        name: file.name.clone(),
                        size: file.size,
                        position: file.position.clone(),
                    })
                    .collect();
            }
        }
    }
    if job.staged_path.is_some() {
        let mut statement = connection.prepare_cached(
            "SELECT name, size, position FROM staged_file WHERE job_id = ?1 ORDER BY seq",
        )?;
        job.files = statement
            .query_map([id], |row| {
                Ok(File {
                    name: row.get(0)?,
                    size: row.get(1)?,
                    position: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(Some(job))
}

fn failure(path: &Path, error: rusqlite::Error) -> DatabaseError {
    DatabaseError::new(NAME, path, error)
}

#[cfg(test)]
mod tests {
    use super::{Event, Stage, State, next_state};

    /// Every legal (state, event) pair and where it goes; every other pair is illegal.
    #[test]
    fn next_state_allows_exactly_the_table() {
        use State as S;
        let f = S::Failed;
        let mut legal = vec![
            (S::Queued, Event::Search, S::Searching),
            (S::Searching, Event::NotFound, S::NotFound),
            (S::Searching, Event::Found, S::CandidateFound),
            (S::Searching, Event::Requeue, S::Queued),
            (S::NotFound, Event::Requeue, S::Queued),
            (S::CandidateFound, Event::Requeue, S::Queued),
            (S::Downloading, Event::Requeue, S::Queued),
            (S::CandidateFound, Event::Download, S::Downloading),
            (S::CandidateFound, Event::NextCandidate, S::CandidateFound),
            (S::Downloading, Event::NextCandidate, S::CandidateFound),
            (S::CandidateFound, Event::Dropped, S::NotFound),
            (S::Downloading, Event::Dropped, S::NotFound),
            (S::Staged, Event::NewRelease, S::Staged),
            (f(Stage::Tag), Event::NewRelease, f(Stage::Tag)),
            (S::Staged, Event::Discard, S::Queued),
            (S::Downloading, Event::Downloaded, S::Downloaded),
            (S::Downloaded, Event::Staged, S::Staged),
            (S::Staged, Event::Approved, S::Approved),
            (S::Approved, Event::Approved, S::Approved),
            (S::Approved, Event::Importing, S::Importing),
            (S::Importing, Event::Importing, S::Importing),
            (S::Approved, Event::Imported, S::Imported),
            (S::Importing, Event::Imported, S::Imported),
            (S::Imported, Event::Imported, S::Imported),
            (S::Staged, Event::Rejected, S::Rejected),
            (S::Rejected, Event::Rejected, S::Rejected),
            (S::Queued, Event::Fail(Stage::Search), f(Stage::Search)),
            (S::Searching, Event::Fail(Stage::Search), f(Stage::Search)),
            (
                S::CandidateFound,
                Event::Fail(Stage::Download),
                f(Stage::Download),
            ),
            (
                S::Downloading,
                Event::Fail(Stage::Download),
                f(Stage::Download),
            ),
            (S::Downloaded, Event::Fail(Stage::Stage), f(Stage::Stage)),
            (S::Staged, Event::Fail(Stage::Tag), f(Stage::Tag)),
            (S::NotFound, Event::Retry, S::Queued),
            (f(Stage::Search), Event::Retry, S::Queued),
            (f(Stage::Download), Event::Retry, S::Queued),
            (f(Stage::Stage), Event::Retry, S::Downloaded),
            (f(Stage::Tag), Event::Retry, S::Staged),
            (S::Cancelled, Event::Cancel, S::Cancelled),
        ];
        for stage in Stage::ALL {
            legal.push((f(*stage), Event::Fail(*stage), f(*stage)));
        }
        for stage in [Stage::Move, Stage::Delete, Stage::Carry] {
            legal.push((S::Approved, Event::Fail(stage), f(stage)));
            legal.push((S::Importing, Event::Fail(stage), f(stage)));
            legal.push((f(stage), Event::Retry, S::Approved));
        }
        for state in [
            S::Queued,
            S::Searching,
            S::NotFound,
            S::CandidateFound,
            S::Downloading,
            S::Downloaded,
            S::Staged,
            f(Stage::Search),
            f(Stage::Download),
            f(Stage::Stage),
            f(Stage::Tag),
        ] {
            legal.push((state, Event::Cancel, S::Cancelled));
        }
        for state in State::all() {
            for event in Event::all() {
                let expected = legal
                    .iter()
                    .find(|(s, e, _)| *s == state && *e == event)
                    .map(|(_, _, to)| *to);
                assert_eq!(next_state(state, event), expected, "{state:?} on {event:?}");
            }
        }
    }

    #[test]
    fn every_state_reads_back_from_its_name_and_stage() {
        for state in State::all() {
            let stage = state.failed_stage().map(Stage::name);
            assert_eq!(State::parse(state.name(), stage), Some(state));
        }
        assert_eq!(State::parse("failed", None), None);
        assert_eq!(State::parse("lost", None), None);
    }
}
