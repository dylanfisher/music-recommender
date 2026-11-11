//! The Discogs catalog: the four dumps' records as tables in `catalog.sqlite` in the data
//! directory, built from scratch each month by [`crate::build`] and read by the queries
//! (ADR 0002, 0022). Strings that repeat across millions of rows (genres, styles, roles,
//! countries, format names and descriptions) are stored once, in tables of their own, and
//! referred to by integer id. The queries read it with SQLite's read-only flag; nothing
//! writes `catalog.sqlite` once the build has renamed itself to it.

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
    year INTEGER
);
CREATE TABLE IF NOT EXISTS master_artist (
    master_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    artist_id INTEGER NOT NULL,
    PRIMARY KEY (master_id, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS master_genre (
    master_id INTEGER NOT NULL,
    genre_id INTEGER NOT NULL REFERENCES genre (id),
    PRIMARY KEY (master_id, genre_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS master_style (
    master_id INTEGER NOT NULL,
    style_id INTEGER NOT NULL REFERENCES style (id),
    PRIMARY KEY (master_id, style_id)
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
    durations BLOB
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
    role_id INTEGER NOT NULL REFERENCES role (id),
    PRIMARY KEY (release_id, artist_id, role_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release_format (
    release_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    format_id INTEGER NOT NULL REFERENCES format (id),
    qty INTEGER NOT NULL,
    PRIMARY KEY (release_id, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release_format_description (
    release_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    description_id INTEGER NOT NULL REFERENCES description (id),
    PRIMARY KEY (release_id, position, description_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release_genre (
    release_id INTEGER NOT NULL,
    genre_id INTEGER NOT NULL REFERENCES genre (id),
    PRIMARY KEY (release_id, genre_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS release_style (
    release_id INTEGER NOT NULL,
    style_id INTEGER NOT NULL REFERENCES style (id),
    PRIMARY KEY (release_id, style_id)
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

/// The catalog in use, opened to read.
pub struct Catalog {
    path: PathBuf,
    connection: Connection,
}

impl Catalog {
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
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rusqlite::Connection;

    use super::FILE_NAME;
    use crate::build::tests::{TempDir, loaded, release};

    /// The lookups phase 4 reads the catalog with, `?1` an id: a release, its credits,
    /// labels and artists, and the releases of an artist (as an artist and in the credits),
    /// a label and a master. Each must be served by an index.
    const LOOKUPS: [(&str, &str); 8] = [
        (
            "release by id",
            "SELECT title, master_id, is_main_release, released, year, country_id, tracks,
             durations FROM release WHERE id = ?1",
        ),
        (
            "a release's credits",
            "SELECT c.artist_id, r.name FROM credit c JOIN role r ON r.id = c.role_id
             WHERE c.release_id = ?1",
        ),
        (
            "a release's labels",
            "SELECT label_id, catno FROM release_label WHERE release_id = ?1 ORDER BY position",
        ),
        (
            "a release's artists",
            "SELECT artist_id FROM release_artist WHERE release_id = ?1 ORDER BY position",
        ),
        (
            "an artist's releases",
            "SELECT release_id FROM release_artist WHERE artist_id = ?1",
        ),
        (
            "an artist's credited releases",
            "SELECT release_id, role_id FROM credit WHERE artist_id = ?1",
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
        for (name, sql) in LOOKUPS {
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
