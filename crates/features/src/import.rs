//! The import: the newest complete set of dumps in `[dumps].dir`, read in place, into a
//! catalog build that replaces `catalog.sqlite` when every dump is in (ADR 0002, 0022).
//!
//! It resumes. Each commit records how many records of the dump are in and the last one's
//! id, so a run after an interruption reads the dump again from the start (gzip can't seek),
//! passes over that many records at the tokenizer, and checks the last one's id before
//! going on; another id means another file, and the dump is loaded again from nothing. A
//! dump's SHA-256 covers the whole file on every pass and must match the CHECKSUM's line
//! before the dump counts as loaded; a mismatch throws its records away. A build begun on
//! an older set is started again on the newest. Until the swap, `catalog.sqlite` stays as
//! it was, so a failed import leaves the catalog in use.

use std::fmt;
use std::path::PathBuf;

use catalog::build::{self, Build, Part, Progress, Started};
use catalog::community::LockError;
use catalog::discogs::{Catalog, CatalogError, Reading};
use config::Config;
use sources::discogs::artist::Artist;
use sources::discogs::dump::{DumpError, Record, Stream};
use sources::discogs::label::Label;
use sources::discogs::master::Master;
use sources::discogs::release::Release;
use sources::discogs::set::{self, Dump, Set, SetError};

use crate::clock::{Clock, unix_seconds};

/// The records committed at a time, each commit with the dump's progress: about a second
/// of releases, and what a resume reads again at most.
pub const BATCH: u64 = 100_000;

/// What an import did.
#[derive(Debug)]
pub enum Summary {
    /// The catalog in use is from the newest set already.
    Current {
        /// The set's date.
        date: String,
    },
    /// A catalog was built from the set and swapped in.
    Built {
        /// The set's date.
        date: String,
        /// Each dump's records, in import order.
        records: Vec<(&'static str, u64)>,
    },
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Current { date } => {
                write!(f, "the catalog is already built from the set of {date}")
            }
            Self::Built { date, records } => {
                write!(f, "catalog built from the set of {date}: ")?;
                let counts: Vec<String> = records
                    .iter()
                    .map(|(dump, count)| format!("{count} {dump}"))
                    .collect();
                f.write_str(&counts.join(", "))
            }
        }
    }
}

/// Why an import couldn't run or stopped.
#[derive(Debug)]
pub enum ImportError {
    /// The config has no `[dumps]` table. Holds the config file's path.
    NoDumps(PathBuf),
    /// The dumps folder has no complete set. Holds the folder.
    NoSet(PathBuf),
    /// Another import holds the lock file.
    Busy(PathBuf),
    /// The lock file can't be opened or locked.
    Lock(PathBuf, std::io::Error),
    /// The dumps folder or a CHECKSUM can't be read.
    Set(SetError),
    /// A dump can't be read: truncated, not a dump's XML, or not its CHECKSUM's file.
    Dump(DumpError),
    /// The catalog or its build failed.
    Catalog(CatalogError),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDumps(config) => write!(
                f,
                "{}: no [dumps] table, so there are no dumps to import; add one with `dir`, \
                 the folder that holds them",
                config.display()
            ),
            Self::NoSet(dir) => write!(
                f,
                "{}: no complete set of dumps (a CHECKSUM file and the four dumps it lists)",
                dir.display()
            ),
            Self::Busy(path) => write!(
                f,
                "another import is running (it holds {}); wait for it to end",
                path.display()
            ),
            Self::Lock(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Set(error) => error.fmt(f),
            Self::Dump(error) => error.fmt(f),
            Self::Catalog(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<LockError> for ImportError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Busy(path) => Self::Busy(path),
            LockError::Io(path, error) => Self::Lock(path, error),
        }
    }
}

impl From<SetError> for ImportError {
    fn from(error: SetError) -> Self {
        Self::Set(error)
    }
}

impl From<DumpError> for ImportError {
    fn from(error: DumpError) -> Self {
        Self::Dump(error)
    }
}

impl From<CatalogError> for ImportError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

/// Imports the newest complete set in `[dumps].dir` into a new catalog and swaps it in,
/// committing every `batch` records (`BATCH` but in tests) and logging each commit. Nothing
/// is done when the catalog in use is from that set already.
///
/// # Errors
///
/// When the config has no `[dumps]` table, another import runs, the folder has no complete
/// set, a dump can't be read or doesn't match its CHECKSUM, or the build fails. What was
/// committed stays in the build for the next run.
pub fn run(
    config: &Config,
    clock: &dyn Clock,
    batch: u64,
    log: &mut dyn FnMut(&str),
) -> Result<Summary, ImportError> {
    let dumps = config
        .dumps
        .as_ref()
        .ok_or_else(|| ImportError::NoDumps(config.path.clone()))?;
    let data_dir = &config.data_dir;
    let _lock = build::lock(data_dir)?;
    let set =
        set::newest_complete(&dumps.dir)?.ok_or_else(|| ImportError::NoSet(dumps.dir.clone()))?;
    if let Reading::Open(catalog) = Catalog::open_read_only(data_dir)? {
        let imported = catalog.summary()?.imported;
        let current = Dump::ALL.iter().all(|dump| {
            imported
                .iter()
                .any(|i| i.dump == dump.name() && i.date == set.date)
        });
        if current {
            return Ok(Summary::Current { date: set.date });
        }
    }
    let mut build = open(config, log)?;
    let mut state = build.state()?;
    let begun = state
        .imported
        .iter()
        .map(|i| &i.date)
        .chain(state.progress.iter().map(|p| &p.date))
        .find(|&date| *date != set.date)
        .cloned();
    if let Some(older) = begun {
        log(&format!(
            "the build was begun on the set of {older}; starting again on the set of {}",
            set.date
        ));
        build.delete()?;
        build = open(config, log)?;
        state = build.state()?;
    }
    let mut records = Vec::new();
    for dump in Dump::ALL {
        let name = dump.name();
        if let Some(imported) = state.imported.iter().find(|i| i.dump == name) {
            log(&format!("{name}: loaded already"));
            records.push((name, imported.records));
            continue;
        }
        let progress = state.progress.iter().find(|p| p.dump == name);
        let mut load = Load {
            build: &mut build,
            set: &set,
            dump,
            batch,
            clock,
            log: &mut *log,
        };
        let count = match dump {
            Dump::Artists => load.run(progress, |b, a: Artist| b.add_artist(&artist(a)))?,
            Dump::Labels => load.run(progress, |b, l: Label| b.add_label(&label(l)))?,
            Dump::Masters => load.run(progress, |b, m: Master| b.add_master(&master(m)))?,
            Dump::Releases => load.run(progress, |b, r: Release| b.add_release(&release(r)))?,
        };
        records.push((name, count));
    }
    log("finishing: indexes, ANALYZE, then the swap");
    build.finish(unix_seconds(clock.now()))?;
    Ok(Summary::Built {
        date: set.date,
        records,
    })
}

/// Opens the build, logging how it started when that isn't where it left off.
fn open(config: &Config, log: &mut dyn FnMut(&str)) -> Result<Build, ImportError> {
    let (build, started) = Build::open(&config.data_dir)?;
    let path = config.data_dir.join(build::BUILD_FILE_NAME);
    match started {
        Started::Continuing => {}
        Started::OtherShape => log(&format!(
            "{}: its tables are from another version; deleted and started again",
            path.display()
        )),
        Started::Damaged => log(&format!(
            "{}: SQLite found it damaged (the Mac may have stopped during a load); deleted \
             and started again",
            path.display()
        )),
    }
    Ok(build)
}

/// One dump being loaded into the build.
struct Load<'a> {
    build: &'a mut Build,
    set: &'a Set,
    dump: Dump,
    batch: u64,
    clock: &'a dyn Clock,
    log: &'a mut dyn FnMut(&str),
}

impl Load<'_> {
    /// Loads the dump, resuming after `progress` if it is there, adding each record with
    /// `add`, and returns its records.
    fn run<R: Record>(
        &mut self,
        progress: Option<&Progress>,
        add: impl Fn(&mut Build, R) -> Result<(), CatalogError>,
    ) -> Result<u64, ImportError> {
        let name = self.dump.name();
        let path = self.set.path(self.dump);
        let sha256 = self.set.sha256(self.dump);
        let mut stream = Stream::<R>::open(&path, sha256)?;
        if let Some(progress) = progress {
            let found = stream.skip(progress.records_done)?;
            if found == Some(progress.last_id) {
                (self.log)(&format!(
                    "{name}: resuming after record {}",
                    progress.records_done
                ));
            } else {
                let found = found.map_or_else(|| "missing".to_string(), |id| id.to_string());
                (self.log)(&format!(
                    "{name}: record {} is {found}, but the build's is {}; loading {name} again \
                     from the start",
                    progress.records_done, progress.last_id
                ));
                self.build.clear(part(self.dump), name)?;
                stream = Stream::open(&path, sha256)?;
            }
        }
        self.build.begin()?;
        loop {
            let record = match stream.read() {
                Ok(Some(record)) => record,
                Ok(None) => break,
                Err(error) => {
                    if error.is_checksum() {
                        self.build.clear(part(self.dump), name)?;
                    } else {
                        self.build.rollback()?;
                    }
                    return Err(error.into());
                }
            };
            let id = record.id();
            add(self.build, record)?;
            let done = stream.records();
            if done % self.batch == 0 {
                self.build.commit(name, &self.set.date, done, id)?;
                (self.log)(&format!("{name}: {done} records"));
            }
        }
        let records = stream.records();
        let now = unix_seconds(self.clock.now());
        self.build
            .finish_dump(name, &self.set.date, sha256, records, now)?;
        (self.log)(&format!(
            "{name}: {records} records, all of {}, its checksum matched",
            path.display()
        ));
        Ok(records)
    }
}

/// The catalog's part each dump loads.
fn part(dump: Dump) -> Part {
    match dump {
        Dump::Artists => Part::Artists,
        Dump::Labels => Part::Labels,
        Dump::Masters => Part::Masters,
        Dump::Releases => Part::Releases,
    }
}

// The parsers' records moved into the catalog's copies (docs/map.md: sources and catalog
// can't import each other).

fn artist(a: Artist) -> build::Artist {
    build::Artist {
        id: a.id,
        name: a.name,
        realname: a.realname,
        namevariations: a.namevariations,
        aliases: a.aliases,
        members: a.members,
        groups: a.groups,
    }
}

fn label(l: Label) -> build::Label {
    build::Label {
        id: l.id,
        name: l.name,
        parent_id: l.parent_id,
    }
}

fn master(m: Master) -> build::Master {
    build::Master {
        id: m.id,
        main_release: m.main_release,
        title: m.title,
        year: m.year,
        artists: m.artists,
        genres: m.genres,
        styles: m.styles,
    }
}

fn release(r: Release) -> build::Release {
    build::Release {
        id: r.id,
        title: r.title,
        master_id: r.master_id,
        is_main_release: r.is_main_release,
        released: r.released,
        year: r.year,
        country: r.country,
        artists: r.artists,
        labels: r
            .labels
            .into_iter()
            .map(|l| build::ReleaseLabel {
                label_id: l.label_id,
                catno: l.catno,
            })
            .collect(),
        credits: r
            .credits
            .into_iter()
            .map(|c| build::Credit {
                artist_id: c.artist_id,
                role: c.role,
            })
            .collect(),
        formats: r
            .formats
            .into_iter()
            .map(|f| build::Format {
                name: f.name,
                qty: f.qty,
                descriptions: f.descriptions,
            })
            .collect(),
        genres: r.genres,
        styles: r.styles,
        barcodes: r.barcodes,
        durations: r.durations,
    }
}
