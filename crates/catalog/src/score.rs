//! The scores: the whole library's list of candidates with their reasons, scored from the
//! catalog and the kept profile, in `scores.sqlite` in the data directory, and what the list
//! was computed from (ADR 0026). Beside the catalog, not in it, so the catalog stays
//! read-only once it is swapped in (ADR 0002, 0022).
//!
//! A rebuild writes `scores.next.sqlite` whole, in one transaction, and renames it over
//! `scores.sqlite`, so a reader sees the old list or the new one and never part of either. A
//! rebuild that stops before the rename leaves the old file in use; its `.next` is deleted
//! when the next rebuild starts. The lists in a reason (a credit's roles on the seeds, a
//! styles reason's styles) are rows of `reason_item`. The artists the list rolls up to, best
//! first, are rows of `artist`.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::community::{LockError, SyncLock, lock_running_waiting};
use crate::download::{named, named_enum};
use crate::error::DatabaseError;
use crate::feedback::Kind;
use crate::sqlite::{self, Reading};
use crate::swap::{SwapError, swap_in};

/// The list in use, inside the data directory.
const FILE_NAME: &str = "scores.sqlite";

/// The list being written, renamed to `FILE_NAME` when complete.
const NEXT_FILE_NAME: &str = "scores.next.sqlite";

/// The lock file `sqlite::open_read_only` would wait on while a file is reset. Nothing
/// creates it: the scores are never opened to write in place, only renamed in whole.
const OPEN_LOCK_FILE: &str = "scores.open.lock";

/// The lock file a rebuild holds, so two can't run at once in any process.
const LOCK_FILE: &str = "scores.lock";

/// The lock file a rebuild also holds, which [`running`] probes (ADR 0021's pattern).
const RUNNING_FILE: &str = "scores.running";

/// The database's name in its errors.
const NAME: &str = "scores";

/// `computed` has the one row the rebuild wrote last. `candidate` is the list, `rank` 1 the
/// best; `reason` and `reason_item` are keyed by the candidate's rank. Which of a reason's
/// columns are set depends on its kind, as [`Reason`] says. `artist` is the roll-up of the
/// candidates, `rank` 1 the best, with the rank of the candidate that is the artist's best.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS computed (
    catalog_set TEXT NOT NULL,
    catalog_swapped_at INTEGER NOT NULL,
    profile_digest TEXT NOT NULL,
    scoring_version INTEGER NOT NULL,
    seeds INTEGER NOT NULL,
    candidates INTEGER NOT NULL,
    finished_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS candidate (
    rank INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    id INTEGER NOT NULL,
    representative INTEGER,
    score REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS reason (
    rank INTEGER NOT NULL,
    position INTEGER NOT NULL,
    weight REAL NOT NULL,
    kind TEXT NOT NULL,
    id INTEGER,
    name TEXT,
    role TEXT,
    other_id INTEGER,
    other_name TEXT,
    relation TEXT,
    albums INTEGER,
    PRIMARY KEY (rank, position)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS reason_item (
    rank INTEGER NOT NULL,
    position INTEGER NOT NULL,
    item INTEGER NOT NULL,
    id INTEGER,
    name TEXT,
    PRIMARY KEY (rank, position, item)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS artist (
    rank INTEGER PRIMARY KEY,
    id INTEGER NOT NULL,
    name TEXT,
    score REAL NOT NULL,
    albums INTEGER NOT NULL,
    best_rank INTEGER NOT NULL
);
";

named_enum! {
    /// What a reason connects by.
    ReasonKind {
        /// The candidate is by an artist of the seeds.
        Artist = "artist",
        /// A person on the seeds is credited on the candidate, or is its artist.
        Credit = "credit",
        /// The candidate is on a label of the seeds' families.
        Label = "label",
        /// The candidate is by an alias, group or member of a seed's artist.
        Related = "related",
        /// The candidate has styles the seeds have.
        Styles = "styles",
    }
}

named_enum! {
    /// How a related artist is related to a seed's artist.
    Relation {
        /// An alias of the seed's artist.
        Alias = "alias",
        /// A member of the seed's artist, a group.
        Member = "member",
        /// A group the seed's artist is in.
        Group = "group",
    }
}

/// What a list was computed from: a rebuild that finds the same skips its work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
    /// The set the catalog was built from, `YYYYMMDD`.
    pub catalog_set: String,
    /// When the catalog was swapped in, in seconds since the Unix epoch.
    pub catalog_swapped_at: i64,
    /// [`digest`] of what the score reads from the profile, as the caller writes it.
    pub profile_digest: String,
    /// The version of the scoring code, which the caller bumps when a weight or a rule
    /// changes, so a list the old code made is rebuilt.
    pub scoring_version: u32,
}

/// A fixed-size digest of `text`: two 64-bit hashes, FNV-1a and FNV-1, as 32 hex digits.
/// Written out here because `std`'s hasher doesn't keep its output across releases, and a
/// stored digest has to match the one a later build computes. Two texts that differ
/// collide with a chance of about 2^-64 at best: the two hashes use the same offset and
/// prime and differ only in the order of the multiply and the xor, so they are correlated
/// and the 128 bits are not independent. That is far less than the chance of a disk error,
/// but this is not a cryptographic hash: it protects against accident, not against someone
/// crafting a profile.
#[must_use]
pub fn digest(text: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let (mut a, mut b) = (OFFSET, OFFSET);
    for &byte in text.as_bytes() {
        a = (a ^ u64::from(byte)).wrapping_mul(PRIME);
        b = b.wrapping_mul(PRIME) ^ u64::from(byte);
    }
    format!("{a:016x}{b:016x}")
}

/// The `computed` row: the inputs and what came of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Computed {
    /// What the list was computed from.
    pub inputs: Inputs,
    /// The seeds it was scored from.
    pub seeds: u64,
    /// The candidates in the list.
    pub candidates: u64,
    /// When the rebuild finished, in seconds since the Unix epoch.
    pub finished_at: i64,
}

/// One candidate of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// A master, or a release without one.
    pub kind: Kind,
    /// Its Discogs id.
    pub id: u64,
    /// The release whose community data stands for it, `None` when the catalog has no row
    /// for its master.
    pub representative: Option<u64>,
    /// Its score, from 0 to 1 for the best.
    pub score: f64,
    /// Its strongest connections, strongest first.
    pub reasons: Vec<Reason>,
}

/// One artist of the list's roll-up: its best three candidates' shares of their scores,
/// scaled so the best artist's is 1.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRow {
    /// Its Discogs id.
    pub id: u64,
    /// Its name, `None` when the catalog has no row for it.
    pub name: Option<String>,
    /// Its score, from 0 to 1 for the best artist.
    pub score: f64,
    /// The candidates its score counts, at most 3.
    pub albums: u64,
    /// The rank of its best candidate.
    pub best_rank: u64,
}

/// An artist, label or style, by id, with its name when the catalog has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// Its Discogs id, or the catalog's id for a style.
    pub id: u64,
    /// Its name.
    pub name: Option<String>,
}

/// One of a reason's list: a credit's role on the seeds, by name only, or a style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The style's id; `None` for a role.
    pub id: Option<u64>,
    /// The role's or the style's name.
    pub name: Option<String>,
}

/// One connection of a candidate. `named` is the artist (artist, credit, related) or the
/// label (label); `role` the credit's role on the candidate; `other` the seeds' label a
/// label reason comes through, or the seed's artist a related one is related to;
/// `relation` how; `albums` the seeds that share it (artist, credit, label); `items` the
/// credit's roles on the seeds, or the styles.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    missing_docs,
    reason = "the struct's comment says what each field holds"
)]
pub struct Reason {
    pub weight: f64,
    pub kind: ReasonKind,
    pub named: Option<Named>,
    pub role: Option<String>,
    pub other: Option<Named>,
    pub relation: Option<Relation>,
    pub albums: Option<u64>,
    pub items: Vec<Item>,
}

/// Takes the lock a rebuild holds, waiting while another process holds it; `waiting` is
/// called once before that wait. While it holds that it holds `scores.running`, which
/// [`running`] probes.
///
/// # Errors
///
/// When a lock file can't be opened or locked.
pub fn lock(data_dir: &Path, waiting: impl FnOnce()) -> Result<SyncLock, LockError> {
    lock_running_waiting(
        data_dir.join(LOCK_FILE),
        data_dir.join(RUNNING_FILE),
        waiting,
    )
}

/// Whether a rebuild is running in `data_dir`, found without waiting and without creating
/// the lock file.
///
/// # Errors
///
/// When the lock file exists but can't be opened or locked.
pub fn running(data_dir: &Path) -> Result<bool, LockError> {
    crate::community::running(data_dir.join(RUNNING_FILE))
}

/// A list being written to `scores.next.sqlite`, in one transaction.
pub struct Writer {
    data_dir: PathBuf,
    path: PathBuf,
    connection: Connection,
    /// The rank the next row takes.
    rank: u64,
    /// The rank the next artist takes.
    artist_rank: u64,
}

impl Writer {
    /// Deletes a `scores.next.sqlite` an earlier rebuild left, and begins it again empty.
    /// It is written without a journal or syncs, since a write that fails is thrown away;
    /// [`Writer::finish`] syncs it before the rename.
    ///
    /// # Errors
    ///
    /// When the old file can't be deleted, or the new one can't be created.
    pub fn create(data_dir: &Path) -> Result<Self, DatabaseError> {
        let path = data_dir.join(NEXT_FILE_NAME);
        let fail = |error| DatabaseError::new(NAME, &path, error);
        sqlite::remove(&path).map_err(|error| DatabaseError::new(NAME, &path, error))?;
        let connection = Connection::open(&path).map_err(fail)?;
        connection
            .execute_batch(&format!(
                "PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF; {SCHEMA} BEGIN;"
            ))
            .map_err(fail)?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            path,
            connection,
            rank: 1,
            artist_rank: 1,
        })
    }

    /// Adds `row` at the next rank: the caller adds the list best first.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn add(&mut self, row: &Row) -> Result<(), DatabaseError> {
        self.add_rows(row)
            .map_err(|error| DatabaseError::new(NAME, &self.path, error))?;
        self.rank += 1;
        Ok(())
    }

    /// Adds `artist` at the next rank: the caller adds the artists best first.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn add_artist(&mut self, artist: &ArtistRow) -> Result<(), DatabaseError> {
        self.connection
            .prepare_cached(
                "INSERT INTO artist (rank, id, name, score, albums, best_rank)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .and_then(|mut insert| {
                insert.execute(params![
                    self.artist_rank,
                    artist.id,
                    artist.name,
                    artist.score,
                    artist.albums,
                    artist.best_rank
                ])
            })
            .map_err(|error| DatabaseError::new(NAME, &self.path, error))?;
        self.artist_rank += 1;
        Ok(())
    }

    fn add_rows(&self, row: &Row) -> rusqlite::Result<()> {
        let c = &self.connection;
        c.prepare_cached(
            "INSERT INTO candidate (rank, kind, id, representative, score)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![
            self.rank,
            row.kind.name(),
            row.id,
            row.representative,
            row.score
        ])?;
        let mut reason = c.prepare_cached(
            "INSERT INTO reason (rank, position, weight, kind, id, name, role, other_id,
             other_name, relation, albums) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        let mut item = c.prepare_cached(
            "INSERT INTO reason_item (rank, position, item, id, name) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (position, r) in row.reasons.iter().enumerate() {
            reason.execute(params![
                self.rank,
                position,
                r.weight,
                r.kind.name(),
                r.named.as_ref().map(|n| n.id),
                r.named.as_ref().and_then(|n| n.name.as_deref()),
                r.role,
                r.other.as_ref().map(|n| n.id),
                r.other.as_ref().and_then(|n| n.name.as_deref()),
                r.relation.map(Relation::name),
                r.albums,
            ])?;
            for (n, entry) in r.items.iter().enumerate() {
                item.execute(params![self.rank, position, n, entry.id, entry.name])?;
            }
        }
        Ok(())
    }

    /// Records what the list was computed from, `seeds` seeds and `now`, commits, and swaps
    /// the file in over `scores.sqlite` ([`swap_in`]), so the list is on the disk when this
    /// returns. Returns the `computed` row it wrote.
    ///
    /// # Errors
    ///
    /// When a step before the rename fails, or the rename does; the old list stays in use.
    /// When only the sync of the directory after it fails, the new list is in use already
    /// and the error says so ([`SwapError::is_swapped`]).
    pub fn finish(self, inputs: &Inputs, seeds: u64, now: i64) -> Result<Computed, FinishError> {
        let Self {
            data_dir,
            path,
            connection,
            rank,
            ..
        } = self;
        let fail = |error| FinishError::Failed(DatabaseError::new(NAME, &path, error));
        let computed = Computed {
            inputs: inputs.clone(),
            seeds,
            candidates: rank - 1,
            finished_at: now,
        };
        connection
            .execute(
                "INSERT INTO computed (catalog_set, catalog_swapped_at, profile_digest,
                 scoring_version, seeds, candidates, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    computed.inputs.catalog_set,
                    computed.inputs.catalog_swapped_at,
                    computed.inputs.profile_digest,
                    computed.inputs.scoring_version,
                    computed.seeds,
                    computed.candidates,
                    computed.finished_at
                ],
            )
            .and_then(|_| connection.execute_batch("COMMIT"))
            .map_err(fail)?;
        connection.close().map_err(|(_, error)| fail(error))?;
        swap_in(&path, &data_dir.join(FILE_NAME)).map_err(FinishError::Swap)?;
        Ok(computed)
    }
}

/// Why [`Writer::finish`] returned an error.
#[derive(Debug)]
pub enum FinishError {
    /// A step failed before the swap; the old list stays.
    Failed(DatabaseError),
    /// The swap failed, before the new list was in use or after it.
    Swap(SwapError),
}

impl std::fmt::Display for FinishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(error) => error.fmt(f),
            Self::Swap(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FinishError {}

/// The list in use, opened to read.
pub struct Scores {
    path: PathBuf,
    connection: Connection,
}

impl Scores {
    /// Opens `scores.sqlite` in `data_dir` with SQLite's read-only flag. A missing file is
    /// `Reading::Missing`, before the first rebuild; one of another shape is
    /// `Reading::OtherShape`, which the next rebuild replaces.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory or the file can't be opened.
    pub fn open_read_only(data_dir: &Path) -> Result<Reading<Self>, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let reading = sqlite::open_read_only(data_dir, &SPEC, sqlite::BUSY_TIMEOUT)
            .map_err(|error| DatabaseError::new(NAME, &path, error.boxed()))?;
        Ok(reading.map(|connection| Self { path, connection }))
    }

    /// What the list was computed from and what came of it.
    ///
    /// # Errors
    ///
    /// When the read fails, or the file has no `computed` row, which a rebuild always
    /// writes.
    pub fn computed(&self) -> Result<Computed, DatabaseError> {
        self.connection
            .query_row(
                "SELECT catalog_set, catalog_swapped_at, profile_digest, scoring_version, seeds,
                 candidates, finished_at
                 FROM computed",
                [],
                |row| {
                    Ok(Computed {
                        inputs: Inputs {
                            catalog_set: row.get(0)?,
                            catalog_swapped_at: row.get(1)?,
                            profile_digest: row.get(2)?,
                            scoring_version: row.get(3)?,
                        },
                        seeds: row.get(4)?,
                        candidates: row.get(5)?,
                        finished_at: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|error| self.fail(error))?
            .ok_or_else(|| {
                DatabaseError::new(
                    NAME,
                    &self.path,
                    std::io::Error::other("no computed row, which every rebuild writes"),
                )
            })
    }

    /// The first `limit` candidates of the list, best first, with their reasons.
    ///
    /// # Errors
    ///
    /// When the read fails, a row holds a kind or relation this build doesn't know, or a
    /// reason or item has no candidate or reason to belong to.
    pub fn top(&self, limit: u64) -> Result<Vec<Row>, DatabaseError> {
        self.page(0, limit)
    }

    /// Up to `limit` candidates of the list after the first `after`, best first, with their
    /// reasons: the ranks `after` + 1 on.
    ///
    /// # Errors
    ///
    /// When the read fails, a row holds a kind or relation this build doesn't know, or a
    /// reason or item has no candidate or reason to belong to.
    pub fn page(&self, after: u64, limit: u64) -> Result<Vec<Row>, DatabaseError> {
        self.read_ranks(after.saturating_add(1), after.saturating_add(limit))
            .map_err(|error| self.fail(error))
    }

    /// The candidate at `rank`, with its reasons, or `None` when the list has none.
    ///
    /// # Errors
    ///
    /// When the read fails, the row holds a kind or relation this build doesn't know, or a
    /// reason or item has no reason to belong to.
    pub fn candidate(&self, rank: u64) -> Result<Option<Row>, DatabaseError> {
        let mut rows = self
            .read_ranks(rank, rank)
            .map_err(|error| self.fail(error))?;
        Ok(rows.pop())
    }

    /// The first `limit` artists of the roll-up, best first.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn artists(&self, limit: u64) -> Result<Vec<ArtistRow>, DatabaseError> {
        // Ranks are SQLite integers, so a limit past the largest is every row.
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.connection
            .prepare(
                "SELECT id, name, score, albums, best_rank FROM artist WHERE rank <= ?1
                 ORDER BY rank",
            )
            .and_then(|mut select| {
                select
                    .query_map([limit], |row| {
                        Ok(ArtistRow {
                            id: row.get(0)?,
                            name: row.get(1)?,
                            score: row.get(2)?,
                            albums: row.get(3)?,
                            best_rank: row.get(4)?,
                        })
                    })?
                    .collect()
            })
            .map_err(|error| self.fail(error))
    }

    /// Reads the three tables for ranks `first` to `last`, each in one pass, and puts each
    /// reason with its candidate and each item with its reason.
    fn read_ranks(&self, first: u64, last: u64) -> rusqlite::Result<Vec<Row>> {
        // Ranks are SQLite integers, so a bound past the largest is every row.
        let (lo, hi) = (
            i64::try_from(first).unwrap_or(i64::MAX),
            i64::try_from(last).unwrap_or(i64::MAX),
        );
        // A row's place among the ones read: the ranks are 1-based and the first is `first`.
        // A rank outside the candidates read is a reason or item with no candidate.
        let place = |rank: usize, what: &str, len: usize| {
            usize::try_from(first)
                .ok()
                .and_then(|first| rank.checked_sub(first))
                .filter(|&place| place < len)
                .ok_or_else(|| {
                    rusqlite::Error::InvalidColumnName(format!(
                        "{what} at rank {rank} has no candidate"
                    ))
                })
        };
        let c = &self.connection;
        let mut rows: Vec<Row> = c
            .prepare(
                "SELECT kind, id, representative, score FROM candidate
                 WHERE rank BETWEEN ?1 AND ?2 ORDER BY rank",
            )?
            .query_map([lo, hi], |row| {
                Ok(Row {
                    kind: named(&row.get::<_, String>(0)?, Kind::parse)?,
                    id: row.get(1)?,
                    representative: row.get(2)?,
                    score: row.get(3)?,
                    reasons: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut reasons = c.prepare(
            "SELECT rank, weight, kind, id, name, role, other_id, other_name, relation, albums
             FROM reason WHERE rank BETWEEN ?1 AND ?2 ORDER BY rank, position",
        )?;
        let mut reasons = reasons.query([lo, hi])?;
        while let Some(row) = reasons.next()? {
            let rank = place(row.get(0)?, "a reason", rows.len())?;
            let relation: Option<String> = row.get(8)?;
            let reason = Reason {
                weight: row.get(1)?,
                kind: named(&row.get::<_, String>(2)?, ReasonKind::parse)?,
                named: optional_named(row.get(3)?, row.get(4)?),
                role: row.get(5)?,
                other: optional_named(row.get(6)?, row.get(7)?),
                relation: relation
                    .map(|name| named(&name, Relation::parse))
                    .transpose()?,
                albums: row.get(9)?,
                items: Vec::new(),
            };
            rows[rank].reasons.push(reason);
        }
        let mut items = c.prepare(
            "SELECT rank, position, id, name FROM reason_item
             WHERE rank BETWEEN ?1 AND ?2 ORDER BY rank, position, item",
        )?;
        let mut items = items.query([lo, hi])?;
        while let Some(row) = items.next()? {
            let (rank, position): (usize, usize) =
                (place(row.get(0)?, "an item", rows.len())?, row.get(1)?);
            let reason = rows[rank].reasons.get_mut(position).ok_or_else(|| {
                rusqlite::Error::InvalidColumnName(format!(
                    "an item at position {position} has no reason"
                ))
            })?;
            reason.items.push(Item {
                id: row.get(2)?,
                name: row.get(3)?,
            });
        }
        Ok(rows)
    }

    fn fail(&self, error: rusqlite::Error) -> DatabaseError {
        DatabaseError::new(NAME, &self.path, error)
    }
}

/// An id and name read from a reason's columns, `None` when it has no id.
fn optional_named(id: Option<u64>, name: Option<String>) -> Option<Named> {
    id.map(|id| Named { id, name })
}

/// Where the list in use is and what it holds.
static SPEC: sqlite::Spec<'static> = sqlite::Spec {
    file_name: FILE_NAME,
    lock_file: OPEN_LOCK_FILE,
    schema: SCHEMA,
    later: "",
    expected: &EXPECTED,
};

/// The objects `SCHEMA` creates, computed the first time a file is opened.
static EXPECTED: sqlite::Expected = sqlite::Expected::new();

#[cfg(test)]
mod tests {
    use super::{FILE_NAME, Inputs, Row, Scores, Writer, digest};
    use crate::build::tests::TempDir;
    use crate::feedback::Kind;
    use crate::sqlite::Reading;

    fn inputs() -> Inputs {
        Inputs {
            catalog_set: "20000101".to_string(),
            catalog_swapped_at: 100,
            profile_digest: digest("[]"),
            scoring_version: 1,
        }
    }

    /// Finishes a list of one candidate after `damage` runs raw SQL on it, and reads page 1.
    fn read_damaged(name: &str, damage: &str) -> String {
        let dir = TempDir::new(name);
        let mut writer = Writer::create(&dir.0).unwrap();
        writer
            .add(&Row {
                kind: Kind::Master,
                id: 7,
                representative: None,
                score: 1.0,
                reasons: Vec::new(),
            })
            .unwrap();
        writer.connection.execute_batch(damage).unwrap();
        writer.finish(&inputs(), 1, 200).unwrap();
        let Reading::Open(scores) = Scores::open_read_only(&dir.0).unwrap() else {
            panic!("no list in {FILE_NAME}");
        };
        scores.page(0, 10).unwrap_err().to_string()
    }

    #[test]
    fn a_reason_with_no_candidate_is_an_error_not_a_panic() {
        let error = read_damaged(
            "orphan-reason",
            "INSERT INTO reason (rank, position, weight, kind) VALUES (5, 0, 1.0, 'artist')",
        );
        assert!(error.contains("reason"), "{error}");
    }

    #[test]
    fn an_item_with_no_reason_is_an_error_not_a_panic() {
        let error = read_damaged(
            "orphan-item",
            "INSERT INTO reason_item (rank, position, item, id) VALUES (1, 3, 0, 9)",
        );
        assert!(error.contains("item"), "{error}");
    }
}
