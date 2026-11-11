//! The catalog's build: `catalog.next.sqlite` in the data directory, loaded from the dumps
//! and renamed over `catalog.sqlite` when complete, so queries never see a half-built
//! catalog (ADR 0002, 0022). It loads in WAL with `synchronous=OFF`: a commit costs no
//! sync, and a process that ends loses nothing committed; a crash of the OS can lose the
//! last commits or damage the file, which `quick_check` finds on the next run. Each
//! commit writes the dump's progress with its records, so a resume starts where the last
//! commit ended.
//!
//! The records here are the catalog's own copies of the parsers' (`sources` and `catalog`
//! can't import each other, docs/map.md), with the same fields; features moves one into
//! the other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::community::{LockError, SyncLock, lock_running};
use crate::discogs::{self, CatalogError, FILE_NAME, INDEXES, Imported, Reading};
use crate::sqlite;

/// The build's file name inside the data directory.
pub const BUILD_FILE_NAME: &str = "catalog.next.sqlite";

/// The lock file the process importing holds, exclusively, so two imports can't run at once.
const LOCK_FILE: &str = "dumps.lock";

/// The lock file the process importing also holds, exclusively, which [`running`] probes
/// (ADR 0021's pattern).
const RUNNING_FILE: &str = "dumps.running";

/// Statements kept prepared: one for each insert below, with room to spare.
const STATEMENT_CACHE: usize = 32;

/// The tables whose strings are stored once and referred to by id.
#[derive(Clone, Copy)]
enum Kind {
    Genre,
    Style,
    Role,
    Country,
    Format,
    Description,
}

impl Kind {
    const ALL: [Self; 6] = [
        Self::Genre,
        Self::Style,
        Self::Role,
        Self::Country,
        Self::Format,
        Self::Description,
    ];

    fn table(self) -> &'static str {
        match self {
            Self::Genre => "genre",
            Self::Style => "style",
            Self::Role => "role",
            Self::Country => "country",
            Self::Format => "format",
            Self::Description => "description",
        }
    }
}

/// The part of the catalog one dump loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The artists, their names and links.
    Artists,
    /// The labels.
    Labels,
    /// The masters and their artists, genres and styles.
    Masters,
    /// The releases and their rows.
    Releases,
}

impl Part {
    fn tables(self) -> &'static [&'static str] {
        match self {
            Self::Artists => &[
                "artist",
                "artist_namevariation",
                "artist_alias",
                "membership",
            ],
            Self::Labels => &["label"],
            Self::Masters => &["master", "master_artist"],
            Self::Releases => &[
                "release",
                "release_artist",
                "release_label",
                "credit",
                "release_format",
                "barcode",
            ],
        }
    }
}

/// An artist, as the build stores it.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct Artist {
    pub id: u64,
    pub name: String,
    pub realname: Option<String>,
    pub namevariations: Vec<String>,
    pub aliases: Vec<u64>,
    pub members: Vec<u64>,
    pub groups: Vec<u64>,
}

/// A label, as the build stores it.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct Label {
    pub id: u64,
    pub name: String,
    pub parent_id: Option<u64>,
}

/// A master, as the build stores it.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct Master {
    pub id: u64,
    pub main_release: u64,
    pub title: String,
    pub year: Option<u16>,
    pub artists: Vec<u64>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
}

/// A release, as the build stores it.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct Release {
    pub id: u64,
    pub title: String,
    pub master_id: u64,
    pub is_main_release: bool,
    pub released: Option<String>,
    pub year: Option<u16>,
    pub country: Option<String>,
    pub artists: Vec<u64>,
    pub labels: Vec<ReleaseLabel>,
    pub credits: Vec<Credit>,
    pub formats: Vec<Format>,
    pub genres: Vec<String>,
    pub styles: Vec<String>,
    pub barcodes: Vec<String>,
    pub durations: Vec<u16>,
}

/// One of a release's labels.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct ReleaseLabel {
    pub label_id: u64,
    pub catno: Option<String>,
}

/// One role of an artist on a release.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct Credit {
    pub artist_id: u64,
    pub role: String,
}

/// One of a release's formats.
#[expect(
    missing_docs,
    reason = "the fields are the parser's, named for the dump's"
)]
#[derive(Debug)]
pub struct Format {
    pub name: String,
    pub qty: u32,
    pub descriptions: Vec<String>,
}

/// How [`Build::open`] found the build file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    /// Sound, or new: the build goes on from what it holds.
    Continuing,
    /// Of another shape (another version wrote it): deleted and started again.
    OtherShape,
    /// Damaged: deleted and started again.
    Damaged,
}

/// Whether `error` is SQLite finding a file damaged or not a database at all.
fn damaged(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

/// A dump being loaded, from the `progress` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The dump's name.
    pub dump: String,
    /// The set's date, `YYYYMMDD`.
    pub date: String,
    /// The records committed.
    pub records_done: u64,
    /// The id of the last of them.
    pub last_id: u64,
}

/// What a build holds: the dumps loaded whole and the one being loaded.
#[derive(Debug, Default)]
pub struct State {
    /// The dumps loaded whole, in order.
    pub imported: Vec<Imported>,
    /// The dumps being loaded.
    pub progress: Vec<Progress>,
}

/// Takes the lock the one process importing holds: an exclusive lock on `dumps.lock` in
/// `data_dir` without waiting, and while it holds that an exclusive lock on
/// `dumps.running`, which [`running`] probes (ADR 0021's pattern).
///
/// # Errors
///
/// When another process holds it, or a lock file can't be opened or locked.
pub fn lock(data_dir: &Path) -> Result<SyncLock, LockError> {
    lock_running(data_dir.join(LOCK_FILE), data_dir.join(RUNNING_FILE))
}

/// Whether an import is running in `data_dir`, found without waiting and without creating
/// the lock file; two probes at once don't see each other as an import.
///
/// # Errors
///
/// When the lock file exists but can't be opened or locked.
pub fn running(data_dir: &Path) -> Result<bool, LockError> {
    crate::community::running(data_dir.join(RUNNING_FILE))
}

/// Deletes the build file in `data_dir`, with its `-wal` and `-shm`, without opening it.
///
/// # Errors
///
/// When a file can't be deleted.
pub fn remove(data_dir: &Path) -> Result<(), CatalogError> {
    let path = data_dir.join(BUILD_FILE_NAME);
    sqlite::remove(&path).map_err(|error| CatalogError::new(&path, error))
}

/// The state of the build in `data_dir`, read with SQLite's read-only flag, so the status
/// changes nothing an import finds. A missing file is `Reading::Missing`, one of another
/// shape `Reading::OtherShape`, which the next import deletes.
///
/// # Errors
///
/// When `data_dir` isn't a directory or the file can't be read.
pub fn read(data_dir: &Path) -> Result<Reading<State>, CatalogError> {
    let path = data_dir.join(BUILD_FILE_NAME);
    let reading = sqlite::open_read_only(
        data_dir,
        &discogs::spec(BUILD_FILE_NAME),
        sqlite::BUSY_TIMEOUT,
    )
    .map_err(|error| CatalogError::open(&path, error))?;
    match reading {
        Reading::Missing => Ok(Reading::Missing),
        Reading::OtherShape => Ok(Reading::OtherShape),
        Reading::Open(connection) => state(&connection)
            .map(Reading::Open)
            .map_err(|error| CatalogError::new(&path, error)),
    }
}

/// The build's `import` and `progress` rows.
fn state(connection: &Connection) -> rusqlite::Result<State> {
    let progress = connection
        .prepare("SELECT dump, date, records_done, last_id FROM progress ORDER BY rowid")?
        .query_map([], |row| {
            Ok(Progress {
                dump: row.get(0)?,
                date: row.get(1)?,
                records_done: row.get(2)?,
                last_id: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(State {
        imported: discogs::imported(connection)?,
        progress,
    })
}

/// The build file, open to load.
pub struct Build {
    data_dir: PathBuf,
    path: PathBuf,
    connection: Connection,
    /// Each interned table's strings and their ids, in `Kind::ALL`'s order.
    interned: [HashMap<String, i64>; 6],
}

impl Build {
    /// Opens the build file in `data_dir`, creating it if there is none, and says how it
    /// started. A file of another shape is deleted and created again, as the other
    /// databases' are. So is one SQLite finds damaged, opening it or in `quick_check`: a
    /// crash of the OS while it loaded with `synchronous=OFF` can leave any page wrong, so
    /// no part of it can be trusted. The strings already interned are read back from their
    /// tables.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, or the file can't be opened, checked or set up.
    pub fn open(data_dir: &Path) -> Result<(Self, Started), CatalogError> {
        let path = data_dir.join(BUILD_FILE_NAME);
        if let Some((build, reset)) = Self::open_sound(data_dir)? {
            if reset {
                return Ok((build, Started::OtherShape));
            }
            return Ok((build, Started::Continuing));
        }
        sqlite::remove(&path).map_err(|error| CatalogError::new(&path, error))?;
        let (build, _) = Self::open_sound(data_dir)?.ok_or_else(|| {
            CatalogError::new(&path, std::io::Error::other("damaged again when created"))
        })?;
        Ok((build, Started::Damaged))
    }

    /// Opens the build file, or `None` if SQLite finds it damaged; the flag is whether a
    /// file of another shape was deleted.
    fn open_sound(data_dir: &Path) -> Result<Option<(Self, bool)>, CatalogError> {
        let path = data_dir.join(BUILD_FILE_NAME);
        let fail = |error| CatalogError::new(&path, error);
        let opened = match sqlite::open(
            data_dir,
            &discogs::spec(BUILD_FILE_NAME),
            sqlite::BUSY_TIMEOUT,
        ) {
            Ok(opened) => opened,
            Err(sqlite::OpenError::Sqlite(error)) if damaged(&error) => return Ok(None),
            Err(error) => return Err(CatalogError::open(&path, error)),
        };
        let connection = opened.connection;
        let check: String = match connection.query_row("PRAGMA quick_check", [], |row| row.get(0)) {
            Ok(check) => check,
            Err(error) if damaged(&error) => return Ok(None),
            Err(error) => return Err(fail(error)),
        };
        if check != "ok" {
            return Ok(None);
        }
        // The build is thrown away if it fails, so a commit needn't reach the disk
        // (ADR 0002, 0022).
        connection
            .pragma_update(None, "synchronous", "OFF")
            .map_err(fail)?;
        connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
        let mut build = Self {
            data_dir: data_dir.to_path_buf(),
            path: path.clone(),
            connection,
            interned: Default::default(),
        };
        build.reload().map_err(fail)?;
        Ok(Some((build, opened.reset)))
    }

    /// Closes the build and deletes its file, to start again from nothing.
    ///
    /// # Errors
    ///
    /// When a file can't be deleted.
    pub fn delete(self) -> Result<(), CatalogError> {
        let Self {
            path, connection, ..
        } = self;
        connection
            .close()
            .map_err(|(_, error)| CatalogError::new(&path, error))?;
        sqlite::remove(&path).map_err(|error| CatalogError::new(&path, error))?;
        Ok(())
    }

    /// The dumps loaded whole and the one being loaded.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn state(&self) -> Result<State, CatalogError> {
        state(&self.connection).map_err(|error| self.fail(error))
    }

    /// Starts the transaction the next records go in.
    ///
    /// # Errors
    ///
    /// When it can't start.
    pub fn begin(&mut self) -> Result<(), CatalogError> {
        self.connection
            .execute_batch("BEGIN")
            .map_err(|error| self.fail(error))
    }

    /// Commits the records since the last commit with the dump's progress: `records_done`
    /// read in all, the last with the id `last_id`. Starts the next transaction.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn commit(
        &mut self,
        dump: &str,
        date: &str,
        records_done: u64,
        last_id: u64,
    ) -> Result<(), CatalogError> {
        self.connection
            .prepare_cached(
                "INSERT OR REPLACE INTO progress (dump, date, records_done, last_id)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .and_then(|mut insert| insert.execute(params![dump, date, records_done, last_id]))
            .and_then(|_| self.connection.execute_batch("COMMIT; BEGIN"))
            .map_err(|error| self.fail(error))
    }

    /// Commits the dump's last records and records it as loaded whole: its file matched
    /// `sha256`, its CHECKSUM line, after `records` records. `now` is in seconds since the
    /// Unix epoch.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn finish_dump(
        &mut self,
        dump: &str,
        date: &str,
        sha256: &str,
        records: u64,
        now: i64,
    ) -> Result<(), CatalogError> {
        self.connection
            .execute("DELETE FROM progress WHERE dump = ?1", [dump])
            .and_then(|_| {
                self.connection.execute(
                    "INSERT INTO import (dump, date, sha256, records, finished_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![dump, date, sha256, records, now],
                )
            })
            .and_then(|_| self.connection.execute_batch("COMMIT"))
            .map_err(|error| self.fail(error))
    }

    /// Throws away what the open transaction holds, if there is one, and starts the
    /// dump's part again from nothing: its rows and its progress row are deleted.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn clear(&mut self, part: Part, dump: &str) -> Result<(), CatalogError> {
        self.rollback()?;
        let deletes = part.tables().iter().fold(String::new(), |sql, table| {
            sql + "DELETE FROM " + table + ";"
        });
        // One transaction, so a run that ends in the middle leaves the progress row with the
        // rows it describes, or neither.
        let cleared = self.connection.transaction().and_then(|transaction| {
            transaction.execute_batch(&deletes)?;
            transaction.execute("DELETE FROM progress WHERE dump = ?1", [dump])?;
            transaction.commit()
        });
        cleared.map_err(|error| self.fail(error))
    }

    /// Throws away what the open transaction holds, if there is one.
    ///
    /// # Errors
    ///
    /// When the rollback fails.
    pub fn rollback(&mut self) -> Result<(), CatalogError> {
        if self.connection.is_autocommit() {
            return Ok(());
        }
        self.connection
            .execute_batch("ROLLBACK")
            .and_then(|()| self.reload())
            .map_err(|error| self.fail(error))
    }

    /// Adds an artist, its names, aliases and both sides of its groups.
    ///
    /// # Errors
    ///
    /// When the write fails, or the artist's id is already there.
    pub fn add_artist(&mut self, artist: &Artist) -> Result<(), CatalogError> {
        self.add_artist_rows(artist)
            .map_err(|error| self.fail(error))
    }

    fn add_artist_rows(&self, artist: &Artist) -> rusqlite::Result<()> {
        let c = &self.connection;
        c.prepare_cached("INSERT INTO artist (id, name, realname) VALUES (?1, ?2, ?3)")?
            .execute(params![artist.id, artist.name, artist.realname])?;
        let mut name = c.prepare_cached(
            "INSERT OR IGNORE INTO artist_namevariation (artist_id, name) VALUES (?1, ?2)",
        )?;
        for variation in &artist.namevariations {
            name.execute(params![artist.id, variation])?;
        }
        let mut alias = c.prepare_cached(
            "INSERT OR IGNORE INTO artist_alias (artist_id, alias_id) VALUES (?1, ?2)",
        )?;
        for id in &artist.aliases {
            alias.execute(params![artist.id, id])?;
        }
        let mut member =
            c.prepare_cached("INSERT INTO membership (group_id, member_id) VALUES (?1, ?2)")?;
        for id in &artist.members {
            member.execute(params![artist.id, id])?;
        }
        for id in &artist.groups {
            member.execute(params![id, artist.id])?;
        }
        Ok(())
    }

    /// Adds a label.
    ///
    /// # Errors
    ///
    /// When the write fails, or the label's id is already there.
    pub fn add_label(&mut self, label: &Label) -> Result<(), CatalogError> {
        self.connection
            .prepare_cached("INSERT INTO label (id, name, parent_id) VALUES (?1, ?2, ?3)")
            .and_then(|mut insert| insert.execute(params![label.id, label.name, label.parent_id]))
            .map(|_| ())
            .map_err(|error| self.fail(error))
    }

    /// Adds a master, its artists, genres and styles.
    ///
    /// # Errors
    ///
    /// When the write fails, or the master's id is already there.
    pub fn add_master(&mut self, master: &Master) -> Result<(), CatalogError> {
        self.add_master_rows(master)
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    fn add_master_rows(&mut self, master: &Master) -> rusqlite::Result<()> {
        let genres = self.intern_all(Kind::Genre, &master.genres)?;
        let styles = self.intern_all(Kind::Style, &master.styles)?;
        let c = &self.connection;
        c.prepare_cached(
            "INSERT INTO master (id, main_release, title, year, genres, styles)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![
            master.id,
            master.main_release,
            master.title,
            master.year,
            genres,
            styles
        ])?;
        let mut artist = c.prepare_cached(
            "INSERT INTO master_artist (master_id, position, artist_id) VALUES (?1, ?2, ?3)",
        )?;
        for (position, id) in master.artists.iter().enumerate() {
            artist.execute(params![master.id, position, id])?;
        }
        Ok(())
    }

    /// Adds a release and its rows.
    ///
    /// # Errors
    ///
    /// When the write fails, or the release's id is already there.
    pub fn add_release(&mut self, release: &Release) -> Result<(), CatalogError> {
        self.add_release_rows(release)
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    fn add_release_rows(&mut self, release: &Release) -> rusqlite::Result<()> {
        let country = release
            .country
            .as_deref()
            .map(|name| self.intern(Kind::Country, name))
            .transpose()?;
        let genres = self.intern_all(Kind::Genre, &release.genres)?;
        let styles = self.intern_all(Kind::Style, &release.styles)?;
        // The credits come sorted by artist, so each artist's roles are together.
        let mut credits: Vec<(u64, Vec<String>)> = Vec::new();
        for credit in &release.credits {
            match credits.last_mut() {
                Some((artist_id, roles)) if *artist_id == credit.artist_id => {
                    roles.push(credit.role.clone());
                }
                _ => credits.push((credit.artist_id, vec![credit.role.clone()])),
            }
        }
        let mut credit_rows = Vec::with_capacity(credits.len());
        for (artist_id, roles) in &credits {
            credit_rows.push((*artist_id, self.intern_all(Kind::Role, roles)?));
        }
        let mut formats = Vec::with_capacity(release.formats.len());
        for format in &release.formats {
            let name = self.intern(Kind::Format, &format.name)?;
            let descriptions = self.intern_all(Kind::Description, &format.descriptions)?;
            formats.push((name, format.qty, descriptions));
        }
        let c = &self.connection;
        let durations = release
            .durations
            .iter()
            .any(|&seconds| seconds != 0)
            .then(|| {
                release
                    .durations
                    .iter()
                    .flat_map(|seconds| seconds.to_le_bytes())
                    .collect::<Vec<u8>>()
            });
        c.prepare_cached(
            "INSERT INTO release (id, title, master_id, is_main_release, released, year,
             country_id, tracks, durations, genres, styles)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?
        .execute(params![
            release.id,
            release.title,
            release.master_id,
            release.is_main_release,
            release.released,
            release.year,
            country,
            release.durations.len(),
            durations,
            genres,
            styles,
        ])?;
        let mut artist = c.prepare_cached(
            "INSERT INTO release_artist (release_id, position, artist_id) VALUES (?1, ?2, ?3)",
        )?;
        for (position, id) in release.artists.iter().enumerate() {
            artist.execute(params![release.id, position, id])?;
        }
        let mut label = c.prepare_cached(
            "INSERT INTO release_label (release_id, position, label_id, catno)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (position, entry) in release.labels.iter().enumerate() {
            label.execute(params![release.id, position, entry.label_id, entry.catno])?;
        }
        let mut credit = c.prepare_cached(
            "INSERT INTO credit (release_id, artist_id, roles) VALUES (?1, ?2, ?3)",
        )?;
        for (artist_id, roles) in &credit_rows {
            credit.execute(params![release.id, artist_id, roles])?;
        }
        let mut format = c.prepare_cached(
            "INSERT INTO release_format (release_id, position, format_id, qty, descriptions)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (position, (name, qty, descriptions)) in formats.iter().enumerate() {
            format.execute(params![release.id, position, name, qty, descriptions])?;
        }
        let mut barcode =
            c.prepare_cached("INSERT OR IGNORE INTO barcode (release_id, value) VALUES (?1, ?2)")?;
        for value in &release.barcodes {
            barcode.execute(params![release.id, value])?;
        }
        Ok(())
    }

    /// Finishes the build once every dump is loaded and renames it over `catalog.sqlite`:
    /// `membership` deduplicated and the indexes created in one transaction; `ANALYZE` for
    /// the query planner, with the swap time, `now`; the WAL checkpointed into the file and
    /// the journal set back to `DELETE`, so the catalog is one file that readers open
    /// read-only. The load ran with `synchronous=OFF`, so these last steps run with it
    /// `FULL` and `fullfsync` on, and the directory is synced after the rename: the file
    /// swapped in, and the rename itself, are on the disk before the import says it is done.
    /// A process holding the old catalog open keeps reading it until it opens the new one.
    ///
    /// # Errors
    ///
    /// When a step fails, or the checkpoint couldn't move the whole WAL into the file (a
    /// reader held it for longer than the busy timeout); the build stays, and finishing again
    /// picks up where it ended.
    pub fn finish(self, now: i64) -> Result<(), CatalogError> {
        self.finish_waiting(now, sqlite::BUSY_TIMEOUT)
    }

    /// `finish`, waiting at most `busy` for readers of the build.
    fn finish_waiting(mut self, now: i64, busy: std::time::Duration) -> Result<(), CatalogError> {
        self.add_indexes()?;
        let Self {
            data_dir,
            path,
            connection,
            ..
        } = self;
        let fail = |error| CatalogError::new(&path, error);
        let refuse = |message: String| CatalogError::new(&path, std::io::Error::other(message));
        connection
            .busy_timeout(busy)
            .and_then(|()| {
                connection.execute_batch(
                    "PRAGMA synchronous = FULL; PRAGMA fullfsync = ON;
                     PRAGMA checkpoint_fullfsync = ON;",
                )
            })
            .and_then(|()| connection.execute("DELETE FROM swap", []))
            .and_then(|_| connection.execute("INSERT INTO swap (swapped_at) VALUES (?1)", [now]))
            .and_then(|_| connection.execute_batch("ANALYZE"))
            .map_err(fail)?;
        // A row of (busy, WAL frames, frames checkpointed); busy is 1 when a reader kept the
        // checkpoint from finishing.
        let busy: i64 = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(fail)?;
        if busy != 0 {
            return Err(refuse(
                "the WAL checkpoint was held back by a reader; not swapped in, run again"
                    .to_string(),
            ));
        }
        let mode: String = connection
            .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
            .map_err(fail)?;
        if !mode.eq_ignore_ascii_case("DELETE") {
            return Err(refuse(format!(
                "the journal stayed `{mode}` instead of `delete`; not swapped in, run again"
            )));
        }
        connection.close().map_err(|(_, error)| fail(error))?;
        std::fs::rename(&path, data_dir.join(FILE_NAME))
            .and_then(|()| std::fs::File::open(&data_dir)?.sync_all())
            .map_err(|error| CatalogError::new(&path, error))?;
        Ok(())
    }

    /// Deduplicates `membership` and creates the indexes, in one transaction.
    fn add_indexes(&mut self) -> Result<(), CatalogError> {
        self.connection
            .execute_batch(&format!("BEGIN; {INDEXES} COMMIT;"))
            .map_err(|error| self.fail(error))
    }

    /// Reads each interned table back into its map.
    fn reload(&mut self) -> rusqlite::Result<()> {
        for (kind, map) in Kind::ALL.iter().zip(&mut self.interned) {
            *map = self
                .connection
                .prepare(&format!("SELECT name, id FROM {}", kind.table()))?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(())
    }

    /// The id of `name` in `kind`'s table, adding it if it is new.
    fn intern(&mut self, kind: Kind, name: &str) -> rusqlite::Result<i64> {
        let map = &mut self.interned[kind as usize];
        if let Some(&id) = map.get(name) {
            return Ok(id);
        }
        let id = i64::try_from(map.len()).unwrap_or(i64::MAX) + 1;
        self.connection
            .prepare_cached(&format!(
                "INSERT INTO {} (id, name) VALUES (?1, ?2)",
                kind.table()
            ))?
            .execute(params![id, name])?;
        map.insert(name.to_string(), id);
        Ok(id)
    }

    /// The ids of `names` in `kind`'s table, adding the new ones, as a BLOB of
    /// little-endian `u32`s in their order; `None` for no names.
    fn intern_all(&mut self, kind: Kind, names: &[String]) -> rusqlite::Result<Option<Vec<u8>>> {
        if names.is_empty() {
            return Ok(None);
        }
        let mut blob = Vec::with_capacity(names.len() * 4);
        for name in names {
            let id = self.intern(kind, name)?;
            let id = u32::try_from(id)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            blob.extend(id.to_le_bytes());
        }
        Ok(Some(blob))
    }

    fn fail(&self, error: rusqlite::Error) -> CatalogError {
        CatalogError::new(&self.path, error)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::{Build, Credit, Format, Release, ReleaseLabel, Started};

    /// A fresh directory that is removed when it drops.
    pub(crate) struct TempDir(pub(crate) PathBuf);

    impl TempDir {
        pub(crate) fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("build-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A release with a row in each of its tables.
    pub(crate) fn release(id: u64) -> Release {
        Release {
            id,
            title: format!("Release {id}"),
            master_id: id / 3,
            is_main_release: id.is_multiple_of(3),
            released: Some("1999-03-00".to_string()),
            year: Some(1999),
            country: Some("UK".to_string()),
            artists: vec![id % 100_000 + 1],
            labels: vec![ReleaseLabel {
                label_id: id % 20_000 + 1,
                catno: Some(format!("EX {id}")),
            }],
            credits: vec![
                Credit {
                    artist_id: id % 70_000 + 1,
                    role: format!("Role {}", id % 400),
                },
                Credit {
                    artist_id: id % 90_001 + 1,
                    role: format!("Role {}", id % 7),
                },
            ],
            formats: vec![Format {
                name: "Vinyl".to_string(),
                qty: 1,
                descriptions: vec!["LP".to_string()],
            }],
            genres: vec!["Rock".to_string()],
            styles: vec![format!("Style {}", id % 300)],
            barcodes: vec![format!("50{id:011}")],
            durations: vec![285, 0, 190],
        }
    }

    /// A build in `dir` with the releases 1 to `count`, committed, and the dumps before
    /// them recorded as loaded.
    pub(crate) fn loaded(dir: &TempDir, count: u64) -> Build {
        let (mut build, _) = Build::open(&dir.0).unwrap();
        for dump in ["artists", "labels", "masters"] {
            build.begin().unwrap();
            build.finish_dump(dump, "20000101", "00", 0, 1).unwrap();
        }
        build.begin().unwrap();
        for id in 1..=count {
            build.add_release(&release(id)).unwrap();
        }
        build
            .finish_dump("releases", "20000101", "00", count, 2)
            .unwrap();
        build
    }

    #[test]
    fn a_build_interrupted_after_its_indexes_is_not_another_shape() {
        let dir = TempDir::new("indexed");
        let mut build = loaded(&dir, 3);
        build.add_indexes().unwrap();
        drop(build);
        let (build, started) = Build::open(&dir.0).unwrap();
        assert_eq!(started, Started::Continuing);
        assert_eq!(build.state().unwrap().imported.len(), 4);
        build.finish(3).unwrap();
    }

    #[test]
    fn a_checkpoint_a_reader_holds_back_fails_the_finish_and_keeps_the_build() {
        let dir = TempDir::new("held-wal");
        let build = loaded(&dir, 3);
        let path = dir.0.join(super::BUILD_FILE_NAME);
        let reader = rusqlite::Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT count(*) FROM release", [], |row| row.get(0))
            .unwrap();
        let error = build
            .finish_waiting(4, std::time::Duration::ZERO)
            .unwrap_err()
            .to_string();
        assert!(error.contains("held back by a reader"), "{error}");
        assert!(path.exists());
        assert!(!dir.0.join(crate::discogs::FILE_NAME).exists());
        drop(reader);
        let (build, _) = Build::open(&dir.0).unwrap();
        build.finish(5).unwrap();
        assert!(dir.0.join(crate::discogs::FILE_NAME).exists());
    }

    #[test]
    fn a_damaged_build_is_deleted_and_started_again() {
        let dir = TempDir::new("damaged");
        let build = loaded(&dir, 2000);
        let root: u64 = build
            .connection
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = 'release'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        build
            .connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        drop(build);
        let path = dir.0.join(super::BUILD_FILE_NAME);
        let mut bytes = std::fs::read(&path).unwrap();
        let page = usize::try_from(root - 1).unwrap() * 4096;
        bytes[page + 100..page + 4096].fill(0xA5);
        std::fs::write(&path, bytes).unwrap();
        let (build, started) = Build::open(&dir.0).unwrap();
        assert_eq!(started, Started::Damaged);
        assert_eq!(build.state().unwrap().imported, []);
    }
}
