//! Opening a database file in the data directory, shared by every database the service keeps:
//! the open lock, WAL and the busy timeout, and the check that the file's tables are the
//! schema's.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};

/// How long a write waits for another connection to finish.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How often `open` tries the lock file again while another process holds it.
const LOCK_RETRY_EVERY: Duration = Duration::from_millis(20);

/// A database file to open: where it is and what it should hold.
pub(crate) struct Spec<'a> {
    /// The file's name inside the data directory.
    pub(crate) file_name: &'a str,
    /// The lock file `open` holds while it checks, deletes and creates the file.
    pub(crate) lock_file: &'a str,
    /// The tables and indexes, as `CREATE ... IF NOT EXISTS` statements.
    pub(crate) schema: &'a str,
    /// The objects `schema` creates, read the first time they are needed. A `static` beside
    /// the schema, so every call with this spec shares one.
    pub(crate) expected: &'a Expected,
}

/// The tables and indexes a schema creates, computed once.
pub(crate) type Expected = OnceLock<Vec<Object>>;

/// Why a file couldn't be opened.
pub(crate) enum OpenError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl OpenError {
    /// The error inside, for a database's own error type to hold.
    pub(crate) fn boxed(self) -> Box<dyn std::error::Error + Send + Sync> {
        match self {
            Self::Sqlite(error) => Box::new(error),
            Self::Io(error) => Box::new(error),
        }
    }
}

/// An opened file, and whether it was deleted and started again because its tables were
/// another shape.
pub(crate) struct Opened {
    pub(crate) connection: Connection,
    pub(crate) reset: bool,
}

/// Tries `try_lock` on `file` until it takes it or `wait` has passed, so a lock another
/// process holds for a moment doesn't fail the caller. Always tries once.
fn lock_within(
    file: &File,
    try_lock: fn(&File) -> Result<(), TryLockError>,
    wait: Duration,
) -> Result<(), TryLockError> {
    let deadline = Instant::now() + wait;
    loop {
        match try_lock(file) {
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY_EVERY);
            }
            result => return result,
        }
    }
}

/// Opens `spec`'s file in `data_dir`, creating it and its tables if they don't exist. The
/// directory itself must exist, so a misspelled `data_dir` fails here instead of starting an
/// empty database somewhere else.
///
/// A file whose tables and indexes aren't the schema's (an older build wrote it) is deleted,
/// with its `-wal` and `-shm`, and created again empty; `Opened::reset` says so. The check,
/// the delete and the create run under an exclusive lock on the lock file, so two processes
/// opening at once end with one fresh file. Waiting for that lock gives up after
/// `lock_timeout`.
pub(crate) fn open(
    data_dir: &Path,
    spec: &Spec<'_>,
    lock_timeout: Duration,
) -> Result<Opened, OpenError> {
    let path = data_dir.join(spec.file_name);
    require_dir(data_dir)?;
    let lock_path = data_dir.join(spec.lock_file);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(OpenError::Io)?;
    wait_for(&lock, File::try_lock, &lock_path, lock_timeout)?;
    let mut connection = connect(&path).map_err(OpenError::Sqlite)?;
    let mut reset = false;
    // Pre-release only (AGENTS.md): delete this check with the pre-release line there.
    let found = objects(&connection).map_err(OpenError::Sqlite)?;
    if has_other_shape(&found, spec).map_err(OpenError::Sqlite)? {
        drop(connection);
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.clone().into_os_string();
            file.push(suffix);
            match std::fs::remove_file(&file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(OpenError::Io(error));
                }
                _ => {}
            }
        }
        reset = true;
        connection = connect(&path).map_err(OpenError::Sqlite)?;
    }
    connection
        .execute_batch(spec.schema)
        .map_err(OpenError::Sqlite)?;
    drop(lock);
    Ok(Opened { connection, reset })
}

/// Takes the lock on `file` with `try_lock`, waiting at most `timeout`; `lock_path` names the
/// file in the error.
fn wait_for(
    file: &File,
    try_lock: fn(&File) -> Result<(), TryLockError>,
    lock_path: &Path,
    timeout: Duration,
) -> Result<(), OpenError> {
    match lock_within(file, try_lock, timeout) {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(OpenError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "{} is held by another process; waited {}",
                lock_path.display(),
                if timeout < Duration::from_secs(1) {
                    format!("{} ms", timeout.as_millis())
                } else {
                    format!("{} s", timeout.as_secs())
                }
            ),
        ))),
        Err(TryLockError::Error(error)) => Err(OpenError::Io(error)),
    }
}

/// What `open_read_only` found; `T` is what an open file becomes.
pub enum Reading<T> {
    /// No file, or one with no tables: nothing has been written here yet.
    Missing,
    /// A file whose tables aren't the schema's. `open` would delete it; the read left it alone.
    OtherShape,
    /// A file of the schema's shape, opened for reading only, as the `T` the caller made of it.
    Open(T),
}

impl<T> Reading<T> {
    /// The same finding with the open file passed through `f`.
    pub(crate) fn map<U>(self, f: impl FnOnce(T) -> U) -> Reading<U> {
        match self {
            Self::Missing => Reading::Missing,
            Self::OtherShape => Reading::OtherShape,
            Self::Open(opened) => Reading::Open(f(opened)),
        }
    }
}

/// Fails unless `data_dir` is a directory, so a misspelled one doesn't start an empty
/// database somewhere else.
fn require_dir(data_dir: &Path) -> Result<(), OpenError> {
    if data_dir.is_dir() {
        Ok(())
    } else {
        Err(OpenError::Sqlite(rusqlite::Error::InvalidPath(
            data_dir.to_path_buf(),
        )))
    }
}

/// Opens `spec`'s file in `data_dir` to read, and only read: it creates no file, deletes
/// none and repairs no shape, so a status check can't change what a sync finds. A missing
/// file, and a file with no tables (0 bytes, or a database that has none), is
/// `Reading::Missing`: `open` would create the tables, and until it does there is nothing to
/// read. One of another shape is `Reading::OtherShape`. While it
/// opens and checks the shape it holds a shared lock on the lock file, if there is one, so it
/// can't see the file between `open`'s delete and create; waiting for that lock gives up
/// after `lock_timeout`. A WAL file is read through its `-shm`, which SQLite creates if it
/// must.
pub(crate) fn open_read_only(
    data_dir: &Path,
    spec: &Spec<'_>,
    lock_timeout: Duration,
) -> Result<Reading<Connection>, OpenError> {
    require_dir(data_dir)?;
    let lock_path = data_dir.join(spec.lock_file);
    // No lock file means `open` has never run here, so no reset can be under way.
    let lock = match File::open(&lock_path) {
        Ok(lock) => Some(lock),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(OpenError::Io(error)),
    };
    if let Some(lock) = &lock {
        wait_for(lock, File::try_lock_shared, &lock_path, lock_timeout)?;
    }
    let path = data_dir.join(spec.file_name);
    if !path.try_exists().map_err(OpenError::Io)? {
        return Ok(Reading::Missing);
    }
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(OpenError::Sqlite)?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(OpenError::Sqlite)?;
    let found = objects(&connection).map_err(OpenError::Sqlite)?;
    if found.is_empty() {
        return Ok(Reading::Missing);
    }
    if has_other_shape(&found, spec).map_err(OpenError::Sqlite)? {
        return Ok(Reading::OtherShape);
    }
    Ok(Reading::Open(connection))
}

/// Opens the file and sets the connection up.
fn connect(path: &PathBuf) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // WAL lets a reader (a status command, an HTTP thread) read while another connection
    // writes.
    connection.pragma_update(None, "journal_mode", "wal")?;
    Ok(connection)
}

/// Whether `found`, the objects a file has, has tables or indexes and they aren't the
/// spec's schema's. An empty file is new, not another shape.
fn has_other_shape(found: &[Object], spec: &Spec<'_>) -> rusqlite::Result<bool> {
    Ok(!found.is_empty() && found != expected_objects(spec)?.as_slice())
}

/// The objects a file with the spec's schema has, read from an in-memory database. The
/// schema is a constant, so they are computed once instead of on every status page load. Two
/// threads arriving together may both compute them; the first result is kept.
fn expected_objects<'a>(spec: &Spec<'a>) -> rusqlite::Result<&'a Vec<Object>> {
    if let Some(objects) = spec.expected.get() {
        return Ok(objects);
    }
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(spec.schema)?;
    let objects = objects(&connection)?;
    Ok(spec.expected.get_or_init(|| objects))
}

/// A table or index as (type, name, sql).
pub(crate) type Object = (String, String, String);

/// The tables and indexes in the database as (type, name, sql), with the sql's whitespace
/// normalized, sorted. SQLite's own objects (`sqlite_*`) are left out.
fn objects(connection: &Connection) -> rusqlite::Result<Vec<Object>> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
         ORDER BY type, name",
    )?;
    statement
        .query_map([], |row| {
            let sql: String = row.get(2)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            ))
        })?
        .collect()
}
