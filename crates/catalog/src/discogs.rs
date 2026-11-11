//! The Discogs catalog: the four dumps' records as tables in `catalog.sqlite` in the data
//! directory, built from scratch each month by [`crate::build`] and read by the queries
//! (ADR 0002, 0022). Strings that repeat across millions of rows (genres, styles, roles,
//! countries, format names and descriptions) are stored once, in tables of their own, and
//! referred to by integer id. The queries read it with SQLite's read-only flag; nothing
//! writes `catalog.sqlite` once the build has renamed itself to it.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::sqlite;
pub use crate::sqlite::Reading;

/// The catalog's file name inside the data directory.
pub(crate) const FILE_NAME: &str = "catalog.sqlite";

/// The lock file `sqlite::open` holds while it checks, deletes and creates the build file.
pub(crate) const OPEN_LOCK_FILE: &str = "catalog.open.lock";

/// The tables, each loaded in the dumps' id order. A record's own rows (a release's
/// artists, labels, credits) are keyed by its id first, in tables `WITHOUT ROWID`, so a
/// release's rows are together on disk and loading appends to each table instead of
/// inserting at random. `position` keeps the dump's order where it means something (a
/// release's first artist is its main one).
///
/// Lists of interned ids that nothing looks rows up by (a release's genres, styles, a
/// format's descriptions, the roles of one artist's credit on a release) are BLOBs of
/// little-endian `u32` ids in the dump's order, NULL when empty: a row for each id would
/// take about a third of the file, and the catalog connections read them whole. A credit
/// is one row for each artist on a release, holding every role the artist has there, so
/// the lookup of an artist's credited releases is one index.
///
/// `membership` holds both sides of each group: an artist's `members` and its `groups`
/// say the same pair from either end, so it is loaded with repeats and deduplicated by
/// [`INDEXES`]'s first statement. `durations` holds a little-endian `u16` of seconds for
/// each track, 0 for one without, and is NULL when no track has one; `tracks` is its length.
/// `master_id` is 0 for a release without a master, as Discogs writes it.
///
/// `import` has a row for each dump loaded whole and checked against its CHECKSUM line.
/// `progress` has a row for a dump being loaded: the records committed and the last one's
/// id, written in the same transaction as the records, so a resume knows where to start
/// and can check the dump is the same. `swap` holds when the build was swapped in.
pub(crate) const TABLES: &str = "
CREATE TABLE IF NOT EXISTS genre (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS style (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS role (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS country (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS format (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS description (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS artist (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    realname TEXT
);
CREATE TABLE IF NOT EXISTS artist_namevariation (
    artist_id INTEGER NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (artist_id, name)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS artist_alias (
    artist_id INTEGER NOT NULL,
    alias_id INTEGER NOT NULL,
    PRIMARY KEY (artist_id, alias_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS membership (
    group_id INTEGER NOT NULL,
    member_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS label (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id INTEGER
);
CREATE TABLE IF NOT EXISTS master (
    id INTEGER PRIMARY KEY,
    main_release INTEGER NOT NULL,
    title TEXT NOT NULL,
    year INTEGER,
    genres BLOB,
    styles BLOB
);
CREATE TABLE IF NOT EXISTS master_artist (
    master_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    artist_id INTEGER NOT NULL,
    PRIMARY KEY (master_id, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    master_id INTEGER NOT NULL,
    is_main_release INTEGER NOT NULL,
    released TEXT,
    year INTEGER,
    country_id INTEGER REFERENCES country (id),
    tracks INTEGER NOT NULL,
    durations BLOB,
    genres BLOB,
    styles BLOB
);
CREATE TABLE IF NOT EXISTS release_artist (
    release_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    artist_id INTEGER NOT NULL,
    PRIMARY KEY (release_id, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release_label (
    release_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    label_id INTEGER NOT NULL,
    catno TEXT,
    PRIMARY KEY (release_id, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS credit (
    release_id INTEGER NOT NULL,
    artist_id INTEGER NOT NULL,
    roles BLOB NOT NULL,
    PRIMARY KEY (release_id, artist_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release_format (
    release_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    format_id INTEGER NOT NULL REFERENCES format (id),
    qty INTEGER NOT NULL,
    descriptions BLOB,
    PRIMARY KEY (release_id, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS barcode (
    release_id INTEGER NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (release_id, value)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS import (
    dump TEXT PRIMARY KEY,
    date TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    records INTEGER NOT NULL,
    finished_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS progress (
    dump TEXT PRIMARY KEY,
    date TEXT NOT NULL,
    records_done INTEGER NOT NULL,
    last_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS swap (swapped_at INTEGER NOT NULL);
";

/// What the build runs once every dump is loaded: `membership` deduplicated, then the
/// indexes the lookups by artist, label and master read. Created after the load, since
/// keeping them up to date row by row would make the load several times slower. One
/// transaction, so a build has all of them or none.
pub(crate) const INDEXES: &str = "
DELETE FROM membership WHERE rowid NOT IN
    (SELECT min(rowid) FROM membership GROUP BY group_id, member_id);
CREATE UNIQUE INDEX IF NOT EXISTS membership_pair ON membership (group_id, member_id);
CREATE INDEX IF NOT EXISTS membership_member ON membership (member_id);
CREATE INDEX IF NOT EXISTS release_master ON release (master_id);
CREATE INDEX IF NOT EXISTS release_artist_artist ON release_artist (artist_id, release_id);
CREATE INDEX IF NOT EXISTS release_label_label ON release_label (label_id, release_id);
CREATE INDEX IF NOT EXISTS credit_artist ON credit (artist_id, release_id);
";

/// Where a catalog file is and what it holds: the build file or the catalog, the same
/// shape, with or without the indexes.
pub(crate) fn spec(file_name: &str) -> sqlite::Spec<'_> {
    static EXPECTED: sqlite::Expected = sqlite::Expected::new();
    sqlite::Spec {
        file_name,
        lock_file: OPEN_LOCK_FILE,
        schema: TABLES,
        later: INDEXES,
        expected: &EXPECTED,
    }
}

/// A failed read or write, naming the database file.
#[derive(Debug)]
pub struct CatalogError {
    pub(crate) path: PathBuf,
    pub(crate) error: Box<dyn std::error::Error + Send + Sync>,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "catalog {}: {}", self.path.display(), self.error)
    }
}

impl std::error::Error for CatalogError {}

impl CatalogError {
    pub(crate) fn new(path: &Path, error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            path: path.to_path_buf(),
            error: Box::new(error),
        }
    }

    pub(crate) fn open(path: &Path, error: sqlite::OpenError) -> Self {
        match error {
            sqlite::OpenError::Sqlite(error) => Self::new(path, error),
            sqlite::OpenError::Io(error) => Self::new(path, error),
        }
    }
}

/// A dump loaded whole, from the `import` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The dump's name, as the import wrote it: `releases`.
    pub dump: String,
    /// The set's date, `YYYYMMDD`.
    pub date: String,
    /// The file's SHA-256, which matched its CHECKSUM line.
    pub sha256: String,
    /// The records read from it.
    pub records: u64,
    /// When it finished loading, in seconds since the Unix epoch.
    pub finished_at: i64,
}

/// What the status shows of the catalog in use.
#[derive(Debug, Default)]
pub struct Summary {
    /// The dumps it was built from, in the order they were loaded.
    pub imported: Vec<Imported>,
    /// When it was swapped in, in seconds since the Unix epoch.
    pub swapped_at: Option<i64>,
}

/// The `import` table's rows, in the order they were written.
pub(crate) fn imported(connection: &Connection) -> rusqlite::Result<Vec<Imported>> {
    connection
        .prepare("SELECT dump, date, sha256, records, finished_at FROM import ORDER BY rowid")?
        .query_map([], |row| {
            Ok(Imported {
                dump: row.get(0)?,
                date: row.get(1)?,
                sha256: row.get(2)?,
                records: row.get(3)?,
                finished_at: row.get(4)?,
            })
        })?
        .collect()
}

/// The statements the lookups below run, `?1` an id. Each must be served by an index, which
/// the tests check from these same constants.
mod lookup {
    pub(super) const RELEASE_MASTER: &str = "SELECT master_id FROM release WHERE id = ?1";
    pub(super) const FIRST_LABEL: &str =
        "SELECT label_id FROM release_label WHERE release_id = ?1 ORDER BY position LIMIT 1";
    pub(super) const MAIN_RELEASE: &str = "SELECT main_release FROM master WHERE id = ?1";
    pub(super) const FIRST_RELEASE: &str =
        "SELECT id FROM release WHERE master_id = ?1 ORDER BY id LIMIT 1";
    pub(super) const LINKS_RELEASE: &str =
        "SELECT id, master_id, is_main_release, year, styles FROM release WHERE id = ?1";
    pub(super) const LINKS_ARTISTS: &str =
        "SELECT artist_id FROM release_artist WHERE release_id = ?1 ORDER BY position";
    pub(super) const LINKS_LABELS: &str =
        "SELECT label_id FROM release_label WHERE release_id = ?1 ORDER BY position";
    pub(super) const LINKS_CREDITS: &str =
        "SELECT artist_id, roles FROM credit WHERE release_id = ?1";
    pub(super) const ARTIST_NAME: &str = "SELECT name FROM artist WHERE id = ?1";
    pub(super) const LABEL: &str = "SELECT name, parent_id FROM label WHERE id = ?1";
    pub(super) const ALIASES: &str = "SELECT alias_id FROM artist_alias WHERE artist_id = ?1";
    pub(super) const MEMBERS: &str = "SELECT member_id FROM membership WHERE group_id = ?1";
    pub(super) const GROUPS: &str = "SELECT group_id FROM membership WHERE member_id = ?1";
    pub(super) const ARTIST_AS_ARTIST: &str =
        "SELECT count(*) FROM release_artist WHERE artist_id = ?1";
    pub(super) const ARTIST_CREDITED: &str = "SELECT count(*) FROM credit WHERE artist_id = ?1";
    pub(super) const LABEL_RELEASES: &str =
        "SELECT count(*) FROM release_label WHERE label_id = ?1";
    pub(super) const MASTER_HEAD: &str = "SELECT title, year, styles FROM master WHERE id = ?1";
    pub(super) const MASTER_ARTISTS: &str =
        "SELECT m.artist_id, a.name FROM master_artist m LEFT JOIN artist a
         ON a.id = m.artist_id WHERE m.master_id = ?1 ORDER BY m.position";
    pub(super) const RELEASE_HEAD: &str = "SELECT title, year, styles FROM release WHERE id = ?1";
    pub(super) const RELEASE_ARTISTS: &str =
        "SELECT r.artist_id, a.name FROM release_artist r LEFT JOIN artist a
         ON a.id = r.artist_id WHERE r.release_id = ?1 ORDER BY r.position";

    /// Every lookup with a name for a test's message.
    #[cfg(test)]
    pub(super) const ALL: [(&str, &str); 20] = [
        ("a release's master", RELEASE_MASTER),
        ("a release's first label", FIRST_LABEL),
        ("a master's main release", MAIN_RELEASE),
        ("a master's first release", FIRST_RELEASE),
        ("a release's links", LINKS_RELEASE),
        ("a release's artists", LINKS_ARTISTS),
        ("a release's labels", LINKS_LABELS),
        ("a release's credits", LINKS_CREDITS),
        ("an artist by id", ARTIST_NAME),
        ("a label by id", LABEL),
        ("an artist's aliases", ALIASES),
        ("a group's members", MEMBERS),
        ("an artist's groups", GROUPS),
        ("an artist's release count", ARTIST_AS_ARTIST),
        ("an artist's credited release count", ARTIST_CREDITED),
        ("a label's release count", LABEL_RELEASES),
        ("a master's head", MASTER_HEAD),
        ("a master's artists", MASTER_ARTISTS),
        ("a release's head", RELEASE_HEAD),
        ("a release's entry artists", RELEASE_ARTISTS),
    ];
}

/// The catalog in use, opened to read.
pub struct Catalog {
    path: PathBuf,
    connection: Connection,
}

impl Catalog {
    /// The error for rows that contradict one another, as `problem` says, which no build
    /// writes. A reader reports it instead of panicking: the service keeps running, and the
    /// next import replaces the catalog.
    #[must_use]
    pub fn inconsistent(&self, problem: impl Into<String>) -> CatalogError {
        CatalogError {
            path: self.path.clone(),
            error: problem.into().into(),
        }
    }

    /// Opens `catalog.sqlite` in `data_dir` with SQLite's read-only flag: it creates no file
    /// and deletes none. A missing file is `Reading::Missing`, before the first import; one
    /// of another shape is `Reading::OtherShape`, left as it is until the next build
    /// replaces it.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory or the file can't be opened.
    pub fn open_read_only(data_dir: &Path) -> Result<Reading<Self>, CatalogError> {
        let path = data_dir.join(FILE_NAME);
        let reading = sqlite::open_read_only(data_dir, &spec(FILE_NAME), sqlite::BUSY_TIMEOUT)
            .map_err(|error| CatalogError::open(&path, error))?;
        Ok(reading.map(|connection| Self { path, connection }))
    }

    /// The dumps the catalog was built from and when it was swapped in.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn summary(&self) -> Result<Summary, CatalogError> {
        let fail = |error| CatalogError::new(&self.path, error);
        let imported = imported(&self.connection).map_err(fail)?;
        let swapped_at = self
            .connection
            .query_row("SELECT max(swapped_at) FROM swap", [], |row| row.get(0))
            .optional()
            .map_err(fail)?
            .flatten();
        Ok(Summary {
            imported,
            swapped_at,
        })
    }

    /// A release's `master_id`, 0 for a release without a master as Discogs writes it, or
    /// `None` when the catalog has no release `release_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn release_master(&self, release_id: u64) -> Result<Option<u64>, CatalogError> {
        self.one(lookup::RELEASE_MASTER, release_id)
    }

    /// The id of a release's first label, or `None` when it has none or the catalog has no
    /// release `release_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn first_label(&self, release_id: u64) -> Result<Option<u64>, CatalogError> {
        self.one(lookup::FIRST_LABEL, release_id)
    }

    /// A master's `main_release`, or `None` when the catalog has no master `master_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn main_release(&self, master_id: u64) -> Result<Option<u64>, CatalogError> {
        self.one(lookup::MAIN_RELEASE, master_id)
    }

    /// The lowest id among the releases of master `master_id`, or `None` when the catalog
    /// has none, which is a master with no releases in it.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn first_release(&self, master_id: u64) -> Result<Option<u64>, CatalogError> {
        self.one(lookup::FIRST_RELEASE, master_id)
    }

    /// The one value `sql` reads for the id `?1`, or `None` when it reads no row.
    fn one(&self, sql: &str, id: u64) -> Result<Option<u64>, CatalogError> {
        self.connection
            .prepare_cached(sql)
            .and_then(|mut select| select.query_row([id], |row| row.get(0)).optional())
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// The `count(*)` that `sql` reads for the id `?1`. An aggregate always reads a row, so
    /// no row is an error, not a count of 0: a huge artist would otherwise read as rare.
    fn count(&self, sql: &str, id: u64) -> Result<u64, CatalogError> {
        self.connection
            .prepare_cached(sql)
            .and_then(|mut select| select.query_row([id], |row| row.get(0)))
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// The values `sql` reads for the id `?1`, one a row, in its order.
    fn many(&self, sql: &str, id: u64) -> Result<Vec<u64>, CatalogError> {
        self.connection
            .prepare_cached(sql)
            .and_then(|mut select| select.query_map([id], |row| row.get(0))?.collect())
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// What the catalog connections read of release `release_id`, or `None` when the
    /// catalog has no such release.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn links(&self, release_id: u64) -> Result<Option<Links>, CatalogError> {
        let fail = |error| CatalogError::new(&self.path, error);
        let c = &self.connection;
        let Some(mut links) = c
            .prepare_cached(lookup::LINKS_RELEASE)
            .and_then(|mut select| select.query_row([release_id], read_release).optional())
            .map_err(fail)?
        else {
            return Ok(None);
        };
        links.artists = self.many(lookup::LINKS_ARTISTS, release_id)?;
        links.labels = self.many(lookup::LINKS_LABELS, release_id)?;
        links.credits = c
            .prepare_cached(lookup::LINKS_CREDITS)
            .and_then(|mut select| {
                select
                    .query_map([release_id], |row| {
                        let artist: u64 = row.get(0)?;
                        let roles = ids(row, 1, || {
                            format!("the roles of artist {artist} on release {release_id}")
                        })?;
                        Ok((artist, roles))
                    })?
                    .collect()
            })
            .map_err(fail)?;
        Ok(Some(links))
    }

    /// Calls `each` with every release's [`Links`], in id order: one pass over the release
    /// table and the three tables of its rows, each read in its primary key's order, so
    /// SQLite reads each from start to end and sorts nothing.
    ///
    /// # Errors
    ///
    /// When a read fails, or one of a release's tables has rows of a release the release
    /// table doesn't, which a build never writes.
    pub fn scan(&self, mut each: impl FnMut(&Links)) -> Result<(), CatalogError> {
        const ARTISTS: &str =
            "SELECT release_id, artist_id FROM release_artist ORDER BY release_id, position";
        const LABELS: &str =
            "SELECT release_id, label_id FROM release_label ORDER BY release_id, position";
        const CREDITS: &str =
            "SELECT release_id, artist_id, roles FROM credit ORDER BY release_id, artist_id";
        let fail = |error| CatalogError::new(&self.path, error);
        let c = &self.connection;
        let mut releases = c
            .prepare("SELECT id, master_id, is_main_release, year, styles FROM release ORDER BY id")
            .map_err(fail)?;
        let mut artists = c.prepare(ARTISTS).map_err(fail)?;
        let mut labels = c.prepare(LABELS).map_err(fail)?;
        let mut credits = c.prepare(CREDITS).map_err(fail)?;
        let rows = artists.query([]).map_err(fail)?;
        let mut artists = Ordered::new(ARTISTS, rows, |row| row.get(1)).map_err(fail)?;
        let rows = labels.query([]).map_err(fail)?;
        let mut labels = Ordered::new(LABELS, rows, |row| row.get(1)).map_err(fail)?;
        let rows = credits.query([]).map_err(fail)?;
        let mut credits = Ordered::new(CREDITS, rows, |row| {
            let (release, artist): (u64, u64) = (row.get(0)?, row.get(1)?);
            let roles = ids(row, 2, || {
                format!("the roles of artist {artist} on release {release}")
            })?;
            Ok((artist, roles))
        })
        .map_err(fail)?;
        let scan_fail = |error| CatalogError::new(&self.path, error);
        let mut rows = releases.query([]).map_err(fail)?;
        while let Some(row) = rows.next().map_err(fail)? {
            let mut links = read_release(row).map_err(fail)?;
            artists
                .of(links.id, &mut links.artists)
                .map_err(scan_fail)?;
            labels.of(links.id, &mut links.labels).map_err(scan_fail)?;
            credits
                .of(links.id, &mut links.credits)
                .map_err(scan_fail)?;
            each(&links);
        }
        if let Some((sql, release_id)) = [artists.left(), labels.left(), credits.left()]
            .into_iter()
            .flatten()
            .next()
        {
            return Err(scan_fail(ScanError::Orphan(sql, release_id)));
        }
        Ok(())
    }

    /// Each role's id and name.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn role_names(&self) -> Result<HashMap<u32, String>, CatalogError> {
        self.names("SELECT id, name FROM role")
    }

    /// Each style's id and name.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn style_names(&self) -> Result<HashMap<u32, String>, CatalogError> {
        self.names("SELECT id, name FROM style")
    }

    /// The ids and names `sql` reads.
    fn names(&self, sql: &str) -> Result<HashMap<u32, String>, CatalogError> {
        self.connection
            .prepare(sql)
            .and_then(|mut select| {
                select
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// An artist's name, or `None` when the catalog has no artist `artist_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn artist_name(&self, artist_id: u64) -> Result<Option<String>, CatalogError> {
        self.connection
            .prepare_cached(lookup::ARTIST_NAME)
            .and_then(|mut select| select.query_row([artist_id], |row| row.get(0)).optional())
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// A label's name and parent, or `None` when the catalog has no label `label_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn label(&self, label_id: u64) -> Result<Option<(String, Option<u64>)>, CatalogError> {
        self.connection
            .prepare_cached(lookup::LABEL)
            .and_then(|mut select| {
                select
                    .query_row([label_id], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()
            })
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// Every label with a parent, as `(label, parent)`: a pass over the label table, which
    /// has no index on the parent, so the sublabels of many labels are one read.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn sublabels(&self) -> Result<Vec<(u64, u64)>, CatalogError> {
        self.connection
            .prepare("SELECT id, parent_id FROM label WHERE parent_id IS NOT NULL")
            .and_then(|mut select| {
                select
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(|error| CatalogError::new(&self.path, error))
    }

    /// An artist's aliases, the members of it as a group and the groups it is in.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn relations(&self, artist_id: u64) -> Result<Relations, CatalogError> {
        Ok(Relations {
            aliases: self.many(lookup::ALIASES, artist_id)?,
            members: self.many(lookup::MEMBERS, artist_id)?,
            groups: self.many(lookup::GROUPS, artist_id)?,
        })
    }

    /// How many releases an artist is on: its rows as a release's artist and in the
    /// credits, so a release that names it in both counts twice.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn artist_releases(&self, artist_id: u64) -> Result<u64, CatalogError> {
        let as_artist = self.count(lookup::ARTIST_AS_ARTIST, artist_id)?;
        let credited = self.count(lookup::ARTIST_CREDITED, artist_id)?;
        Ok(as_artist + credited)
    }

    /// How many release rows name a label.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn label_releases(&self, label_id: u64) -> Result<u64, CatalogError> {
        self.count(lookup::LABEL_RELEASES, label_id)
    }

    /// A master's title, year, artists and styles, or `None` when the catalog has no master
    /// `master_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn master_entry(&self, master_id: u64) -> Result<Option<Entry>, CatalogError> {
        self.entry(
            "master",
            lookup::MASTER_HEAD,
            lookup::MASTER_ARTISTS,
            master_id,
        )
    }

    /// A release's title, year, artists and styles, or `None` when the catalog has no release
    /// `release_id`.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn release_entry(&self, release_id: u64) -> Result<Option<Entry>, CatalogError> {
        self.entry(
            "release",
            lookup::RELEASE_HEAD,
            lookup::RELEASE_ARTISTS,
            release_id,
        )
    }

    /// The entry `head` and `artists` read for the id `?1` of `table`.
    fn entry(
        &self,
        table: &str,
        head: &str,
        artists: &str,
        id: u64,
    ) -> Result<Option<Entry>, CatalogError> {
        let fail = |error| CatalogError::new(&self.path, error);
        let c = &self.connection;
        let Some((title, year, styles)) = c
            .prepare_cached(head)
            .and_then(|mut select| {
                select
                    .query_row([id], |row| {
                        let styles = ids(row, 2, || format!("the styles of {table} {id}"))?;
                        Ok((row.get(0)?, row.get(1)?, styles))
                    })
                    .optional()
            })
            .map_err(fail)?
        else {
            return Ok(None);
        };
        let mut artists: Vec<(u64, Option<String>)> = c
            .prepare_cached(artists)
            .and_then(|mut select| {
                select
                    .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            })
            .map_err(fail)?;
        // Each artist once, at its first place: Discogs lists one twice when its name
        // variations or join words differ.
        let mut seen = HashSet::new();
        artists.retain(|&(artist, _)| seen.insert(artist));
        Ok(Some(Entry {
            title,
            year,
            artists,
            styles,
        }))
    }
}

/// What the catalog connections read of a release: its master, year and styles, and the
/// artists, credits and labels on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Links {
    /// The release's id.
    pub id: u64,
    /// Its `master_id`, 0 for a release without a master.
    pub master_id: u64,
    /// Whether it is its master's main release.
    pub is_main_release: bool,
    /// Its year, when it has one.
    pub year: Option<u16>,
    /// Its styles' ids, in the dump's order.
    pub styles: Vec<u32>,
    /// Its artists' ids, in the dump's order.
    pub artists: Vec<u64>,
    /// Each artist credited on it, by id, with its roles' ids.
    pub credits: Vec<(u64, Vec<u32>)>,
    /// Its labels' ids, in the dump's order.
    pub labels: Vec<u64>,
}

/// An artist's aliases, members and groups, by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relations {
    /// The artist's aliases.
    pub aliases: Vec<u64>,
    /// The members of the artist, a group.
    pub members: Vec<u64>,
    /// The groups the artist is in.
    pub groups: Vec<u64>,
}

/// What names and shows a master or a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its title.
    pub title: String,
    /// Its year, when it has one.
    pub year: Option<u16>,
    /// Its artists in order, each once, an id and its name, `None` for an artist the
    /// catalog doesn't have.
    pub artists: Vec<(u64, Option<String>)>,
    /// Its styles' ids, in the dump's order.
    pub styles: Vec<u32>,
}

/// A release row's [`Links`], without the rows of the other tables.
fn read_release(row: &rusqlite::Row<'_>) -> rusqlite::Result<Links> {
    let id: u64 = row.get(0)?;
    Ok(Links {
        id,
        master_id: row.get(1)?,
        is_main_release: row.get(2)?,
        year: row.get(3)?,
        styles: ids(row, 4, || format!("the styles of release {id}"))?,
        artists: Vec::new(),
        credits: Vec::new(),
        labels: Vec::new(),
    })
}

/// The `u32` ids in column `index`'s BLOB of little-endian ids. NULL is none, as the build
/// writes a list without ids. A length that isn't a whole number of ids is an error saying
/// `what` it is of, called only then.
fn ids(
    row: &rusqlite::Row<'_>,
    index: usize,
    what: impl FnOnce() -> String,
) -> rusqlite::Result<Vec<u32>> {
    let blob = row.get_ref(index)?.as_blob_or_null()?.unwrap_or_default();
    let (ids, rest) = blob.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Blob,
            format!("{} is {} bytes, not whole 4-byte ids", what(), blob.len()).into(),
        ));
    }
    Ok(ids.iter().map(|id| u32::from_le_bytes(*id)).collect())
}

/// A table of a release's rows, read in release order beside the release table.
struct Ordered<'s, T> {
    /// The statement reading them, which names the table.
    sql: &'static str,
    rows: rusqlite::Rows<'s>,
    read: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    next: Option<(u64, T)>,
}

impl<'s, T> Ordered<'s, T> {
    fn new(
        sql: &'static str,
        rows: rusqlite::Rows<'s>,
        read: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Self> {
        let mut rows = Self {
            sql,
            rows,
            read,
            next: None,
        };
        rows.advance()?;
        Ok(rows)
    }

    fn advance(&mut self) -> rusqlite::Result<()> {
        self.next = match self.rows.next()? {
            Some(row) => Some((row.get(0)?, (self.read)(row)?)),
            None => None,
        };
        Ok(())
    }

    /// Moves the rows of release `release_id` into `into`. A row of an earlier release is
    /// one the release table doesn't have.
    fn of(&mut self, release_id: u64, into: &mut Vec<T>) -> Result<(), ScanError> {
        while let Some((id, _)) = &self.next {
            if *id > release_id {
                break;
            }
            if *id < release_id {
                return Err(ScanError::Orphan(self.sql, *id));
            }
            if let Some((_, value)) = self.next.take() {
                into.push(value);
            }
            self.advance().map_err(ScanError::Sqlite)?;
        }
        Ok(())
    }

    /// The statement and release of a row left after the last release, if any.
    fn left(&self) -> Option<(&'static str, u64)> {
        self.next.as_ref().map(|(id, _)| (self.sql, *id))
    }
}

/// Why the scan stopped.
#[derive(Debug)]
enum ScanError {
    Sqlite(rusqlite::Error),
    /// The statement that read a row of a release the release table doesn't have, and the
    /// release.
    Orphan(&'static str, u64),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => error.fmt(f),
            Self::Orphan(sql, release_id) => write!(
                f,
                "rows of release {release_id}, which isn't in release, from `{sql}`"
            ),
        }
    }
}

impl std::error::Error for ScanError {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use std::collections::HashMap;

    use rusqlite::Connection;

    use super::{Catalog, FILE_NAME, Reading, lookup};
    use crate::build::Master;
    use crate::build::tests::{TempDir, loaded, release};

    /// The lookups phase 4 reads the catalog with, `?1` an id: a release, its credits,
    /// labels and artists, and the releases of an artist (as an artist and in the credits),
    /// a label and a master. Each must be served by an index. The ones a method of `Catalog`
    /// runs are its statements, so the check and the method can't drift; the benchmark
    /// reads the entries by position.
    const LOOKUPS: [(&str, &str); 8] = [
        (
            "release by id",
            "SELECT title, master_id, is_main_release, released, year, country_id, tracks,
             durations FROM release WHERE id = ?1",
        ),
        ("a release's credits", lookup::LINKS_CREDITS),
        (
            "a release's labels",
            "SELECT label_id, catno FROM release_label WHERE release_id = ?1 ORDER BY position",
        ),
        ("a release's artists", lookup::LINKS_ARTISTS),
        (
            "an artist's releases",
            "SELECT release_id FROM release_artist WHERE artist_id = ?1",
        ),
        (
            "an artist's credited releases",
            "SELECT release_id, roles FROM credit WHERE artist_id = ?1",
        ),
        (
            "a label's releases",
            "SELECT release_id FROM release_label WHERE label_id = ?1",
        ),
        (
            "a master's releases",
            "SELECT id FROM release WHERE master_id = ?1",
        ),
    ];

    /// A catalog in a fresh directory with the releases 1 to `count`.
    fn finished(test: &str, count: u64) -> (TempDir, Catalog) {
        let dir = TempDir::new(test);
        loaded(&dir, count).finish(1).unwrap();
        let Reading::Open(catalog) = Catalog::open_read_only(&dir.0).unwrap() else {
            panic!("no catalog");
        };
        (dir, catalog)
    }

    #[test]
    fn the_scan_gives_each_release_with_its_rows_as_links_reads_it() {
        let (_dir, catalog) = finished("scan", 5);
        let mut scanned = Vec::new();
        catalog.scan(|links| scanned.push(links.clone())).unwrap();
        assert_eq!(
            scanned.iter().map(|links| links.id).collect::<Vec<_>>(),
            [1, 2, 3, 4, 5]
        );
        for links in &scanned {
            assert_eq!(catalog.links(links.id).unwrap().as_ref(), Some(links));
        }
        assert_eq!(catalog.links(6).unwrap(), None);
        let roles = catalog.role_names().unwrap();
        let styles = catalog.style_names().unwrap();
        let named = |names: &HashMap<u32, String>, ids: &[u32]| -> Vec<String> {
            ids.iter().map(|id| names[id].clone()).collect()
        };
        // `release` gives release 3 the master 1, its artist and its two credits artist 4,
        // label 4, "Role 3" twice and "Style 3".
        let third = &scanned[2];
        assert_eq!(
            (third.master_id, third.is_main_release, third.year),
            (1, true, Some(1999))
        );
        assert_eq!(
            (&third.artists[..], &third.labels[..]),
            (&[4][..], &[4][..])
        );
        assert_eq!(third.credits.len(), 1);
        assert_eq!(third.credits[0].0, 4);
        assert_eq!(named(&roles, &third.credits[0].1), ["Role 3", "Role 3"]);
        assert_eq!(named(&styles, &third.styles), ["Style 3"]);
    }

    #[test]
    fn an_entry_lists_an_artist_once_though_a_release_credits_it_twice() {
        let (dir, catalog) = finished("twice", 3);
        // Discogs lists an artist twice when its name variations or join words differ.
        Connection::open(dir.0.join(FILE_NAME))
            .unwrap()
            .execute(
                "INSERT INTO release_artist (release_id, position, artist_id)
                 VALUES (2, 1, 3), (2, 2, 9), (2, 3, 3)",
                [],
            )
            .unwrap();
        let entry = catalog.release_entry(2).unwrap().unwrap();
        assert_eq!(entry.artists, [(3, None), (9, None)]);
    }

    #[test]
    fn a_release_s_first_label_is_the_first_of_its_links() {
        let (_dir, catalog) = finished("first-label", 5);
        for id in 1..=5 {
            let links = catalog.links(id).unwrap().unwrap();
            // Each fixture release has a label.
            assert_eq!(catalog.first_label(id).unwrap(), Some(links.labels[0]));
        }
        assert_eq!(catalog.first_label(6).unwrap(), None);
    }

    #[test]
    fn an_id_blob_that_isn_t_whole_ids_is_an_error_naming_the_table_and_row() {
        let (dir, catalog) = finished("damaged-ids", 3);
        let connection = Connection::open(dir.0.join(FILE_NAME)).unwrap();
        // Five bytes: one id and a byte, which a build never writes.
        connection
            .execute("UPDATE release SET styles = x'0100000002' WHERE id = 2", [])
            .unwrap();
        connection
            .execute(
                "UPDATE credit SET roles = x'010000' WHERE release_id = 3",
                [],
            )
            .unwrap();
        // NULL is an empty list, as a build writes for a release without styles.
        connection
            .execute("UPDATE release SET styles = NULL WHERE id = 1", [])
            .unwrap();
        assert_eq!(catalog.links(1).unwrap().unwrap().styles, Vec::<u32>::new());
        let error = catalog.links(2).unwrap_err().to_string();
        assert!(error.contains("styles of release 2"), "{error}");
        assert!(error.contains("5 bytes"), "{error}");
        let error = catalog.release_entry(2).unwrap_err().to_string();
        assert!(error.contains("styles of release 2"), "{error}");
        let error = catalog.links(3).unwrap_err().to_string();
        assert!(error.contains("roles of artist"), "{error}");
        assert!(error.contains("release 3"), "{error}");
        let error = catalog.scan(|_| {}).unwrap_err().to_string();
        assert!(error.contains("styles of release 2"), "{error}");
    }

    #[test]
    fn the_scan_fails_on_a_row_of_a_release_the_catalog_doesn_t_have() {
        let (dir, catalog) = finished("orphan", 3);
        Connection::open(dir.0.join(FILE_NAME))
            .unwrap()
            .execute(
                "INSERT INTO credit (release_id, artist_id, roles) VALUES (9, 1, x'01000000')",
                [],
            )
            .unwrap();
        let error = catalog.scan(|_| {}).unwrap_err().to_string();
        assert!(
            error.contains("rows of release 9, which isn't in release, from `SELECT release_id, artist_id, roles FROM credit"),
            "{error}"
        );
    }

    #[test]
    fn reads_a_release_s_master_and_a_master_s_main_release() {
        let dir = TempDir::new("masters");
        let mut build = loaded(&dir, 7);
        build.begin().unwrap();
        build
            .add_master(&Master {
                id: 2,
                main_release: 6,
                title: "Master 2".to_string(),
                year: None,
                artists: vec![],
                genres: vec![],
                styles: vec![],
            })
            .unwrap();
        // `commit` starts the next transaction, which `finish` can't run inside.
        build.commit("masters", "20000101", 1, 2).unwrap();
        build.rollback().unwrap();
        build.finish(3).unwrap();
        let Reading::Open(catalog) = Catalog::open_read_only(&dir.0).unwrap() else {
            panic!("no catalog");
        };
        // `release` gives release 7 the master 7 / 3, and release 2 the master 0.
        assert_eq!(catalog.release_master(7).unwrap(), Some(2));
        assert_eq!(catalog.release_master(2).unwrap(), Some(0));
        assert_eq!(catalog.release_master(8).unwrap(), None);
        assert_eq!(catalog.main_release(2).unwrap(), Some(6));
        assert_eq!(catalog.main_release(1).unwrap(), None);
        // A master's first release is the lowest id with it, with or without a master row.
        let lowest = (1..=7).find(|&id| catalog.release_master(id).unwrap() == Some(2));
        assert!(lowest.is_some());
        assert_eq!(catalog.first_release(2).unwrap(), lowest);
        assert_eq!(catalog.first_release(999).unwrap(), None);
    }

    /// The plan SQLite chooses for `sql`, one line per step.
    fn plan(connection: &Connection, sql: &str) -> Vec<String> {
        connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([1], |row| row.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn each_lookup_reads_an_index() {
        let dir = TempDir::new("plans");
        loaded(&dir, 300).finish(1).unwrap();
        let connection = Connection::open(dir.0.join(FILE_NAME)).unwrap();
        // Not here: the passes over a whole table, which read every row and are meant to:
        // `scan` (the release table and the three tables of its rows, each in its primary
        // key's order), `sublabels`, and `role_names` and `style_names`.
        for (name, sql) in LOOKUPS.iter().chain(&lookup::ALL) {
            let steps = plan(&connection, sql);
            assert!(
                steps
                    .iter()
                    .all(|step| step.starts_with("SEARCH") && !step.contains("TEMP B-TREE")),
                "{name}: {steps:?}"
            );
        }
    }

    /// The 99th percentile of `times`.
    fn p99(mut times: Vec<Duration>) -> Duration {
        times.sort_unstable();
        times[times.len() * 99 / 100]
    }

    /// The lookups' budgets on 1M generated releases (plan, phase 3): a release by id under
    /// 1 ms, the others under 10 ms at the 99th percentile. Run with
    /// `cargo test --release -p catalog -- --ignored lookups --nocapture`.
    #[test]
    #[ignore = "a benchmark: writes 1M releases"]
    fn lookups_on_a_million_releases() {
        const RELEASES: u64 = 1_000_000;
        let dir = TempDir::new("lookups");
        let (mut build, _) = crate::build::Build::open(&dir.0).unwrap();
        let started = Instant::now();
        build.begin().unwrap();
        for id in 1..=RELEASES {
            build.add_release(&release(id)).unwrap();
            if id % 100_000 == 0 {
                build.commit("releases", "20000101", id, id).unwrap();
            }
        }
        build
            .finish_dump("releases", "20000101", "00", RELEASES, 1)
            .unwrap();
        let loaded_in = started.elapsed();
        build.finish(2).unwrap();
        let bytes = std::fs::metadata(dir.0.join(FILE_NAME)).unwrap().len();
        println!(
            "{RELEASES} releases written in {loaded_in:?}, finished in {:?}; {} bytes a release",
            started.elapsed().saturating_sub(loaded_in),
            bytes / RELEASES
        );
        let connection = Connection::open_with_flags(
            dir.0.join(FILE_NAME),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for (index, (name, sql)) in LOOKUPS.iter().enumerate() {
            let mut statement = connection.prepare(sql).unwrap();
            let times: Vec<Duration> = (0..10_000u64)
                .map(|n| {
                    // A release spread over the ids, or its artist, label or master.
                    let generated = release(n * 7919 % RELEASES + 1);
                    let key = match index {
                        4 => generated.artists[0],
                        5 => generated.credits[0].artist_id,
                        6 => generated.labels[0].label_id,
                        7 => generated.master_id,
                        _ => generated.id,
                    };
                    let start = Instant::now();
                    let rows = statement.query_map([key], |_| Ok(())).unwrap().count();
                    let took = start.elapsed();
                    assert!(rows > 0, "{name}: no rows for {key}");
                    took
                })
                .collect();
            println!("{name}: p99 {:?}", p99(times));
        }
    }
}
