//! Opening a database file in the data directory, shared by every database the service keeps:
//! the open lock, WAL and the busy timeout, and the check that the file's tables are the
//! schema's.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;

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
}

/// Why a file couldn't be opened.
pub(crate) enum OpenError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

/// An opened file, and whether it was deleted and started again because its tables were
/// another shape.
pub(crate) struct Opened {
    pub(crate) connection: Connection,
    pub(crate) reset: bool,
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
    if !data_dir.is_dir() {
        return Err(OpenError::Sqlite(rusqlite::Error::InvalidPath(
            data_dir.to_path_buf(),
        )));
    }
    let lock_path = data_dir.join(spec.lock_file);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(OpenError::Io)?;
    let deadline = Instant::now() + lock_timeout;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY_EVERY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(OpenError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "{} is held by another process; waited {}",
                        lock_path.display(),
                        if lock_timeout < Duration::from_secs(1) {
                            format!("{} ms", lock_timeout.as_millis())
                        } else {
                            format!("{} s", lock_timeout.as_secs())
                        }
                    ),
                )));
            }
            Err(TryLockError::Error(error)) => return Err(OpenError::Io(error)),
        }
    }
    let mut connection = connect(&path).map_err(OpenError::Sqlite)?;
    let mut reset = false;
    // Pre-release only (AGENTS.md): delete this check with the pre-release line there.
    if has_other_shape(&connection, spec.schema).map_err(OpenError::Sqlite)? {
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

/// Opens the file and sets the connection up.
fn connect(path: &PathBuf) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // WAL lets a reader (a status command, an HTTP thread) read while another connection
    // writes.
    connection.pragma_update(None, "journal_mode", "wal")?;
    Ok(connection)
}

/// Whether the file has tables or indexes and they aren't `schema`'s. An empty file is new,
/// not another shape. Reads only.
fn has_other_shape(connection: &Connection, schema: &str) -> rusqlite::Result<bool> {
    let found = objects(connection)?;
    Ok(!found.is_empty() && found != objects(&expected_connection(schema)?)?)
}

/// An in-memory database with `schema`, to read the objects the file should have from.
fn expected_connection(schema: &str) -> rusqlite::Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(schema)?;
    Ok(connection)
}

/// The tables and indexes in the database as (type, name, sql), with the sql's whitespace
/// normalized, sorted. SQLite's own objects (`sqlite_*`) are left out.
fn objects(connection: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
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
