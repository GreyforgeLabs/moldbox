//! Consistent SQLite online backups and explicit WAL checkpoint control.
//!
//! This crate is the library half of the `moldbox` command-line
//! tool. It exposes the same operations as the CLI:
//!
//! * [`checkpoint`] runs `PRAGMA wal_checkpoint(<mode>)` and reports whether
//!   the write-ahead log was fully checkpointed, busy, or only partially
//!   checkpointed.
//! * [`backup`] copies a live database with SQLite's online backup API into a
//!   private (`0600`) temporary file, validates it with `PRAGMA quick_check`,
//!   fsyncs it, publishes it under the destination name with a no-clobber hard
//!   link and finally fsyncs the destination directory. Errors before
//!   publication leave no destination behind.
//! * [`snapshot`] optionally performs WAL maintenance first and, with
//!   `require_complete`, refuses to publish a backup after a busy or partial
//!   checkpoint.
//! * [`db_info`] reports journal mode, WAL state and page statistics.
//!
//! Source databases are never created: they are validated as regular,
//! non-symlink files, opened through SQLite `mode=ro`/`mode=rw` URIs, and
//! their device/inode identity is re-checked after the connection is opened.
//!
//! ```no_run
//! use moldbox::{backup, checkpoint, BackupOptions, CheckpointMode};
//!
//! let result = checkpoint("myapp.db", CheckpointMode::Truncate)?;
//! println!("{}", result.fully_checkpointed());
//! let copy = backup("myapp.db", "backup.db", &BackupOptions::default())?;
//! println!("{}", copy.sha256);
//! # Ok::<(), moldbox::Error>(())
//! ```

#![forbid(unsafe_code)]

pub(crate) mod argparse;
#[doc(hidden)]
pub mod cli;
pub(crate) mod help;
pub(crate) mod printable;
pub(crate) mod pyfmt;

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

/// Crate version, as printed by `moldbox --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// SQLite busy timeout applied to every connection (Python `sqlite3` default).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Number of candidate names tried for a temporary file (`tempfile.TMP_MAX`).
const TMP_MAX: usize = 10_000;

/// Longest single sleep between interrupt checks while throttling a backup.
const SLEEP_SLICE: Duration = Duration::from_millis(100);

/// Number of the termination signal the CLI's handlers recorded (0 = none).
/// Library callers that never install the handlers are unaffected.
pub(crate) static PENDING_SIGNAL: OnceLock<Arc<AtomicUsize>> = OnceLock::new();

/// The recorded termination signal, if any.
pub(crate) fn pending_signal() -> Option<i32> {
    PENDING_SIGNAL
        .get()
        .map(|flag| flag.load(Ordering::SeqCst))
        .filter(|&sig| sig != 0)
        .and_then(|sig| i32::try_from(sig).ok())
}

/// Fail with [`Error::Interrupted`] once a termination signal was recorded,
/// so the caller's cleanup runs (as Python's `except BaseException` did on
/// `KeyboardInterrupt`).
fn check_interrupt() -> Result<()> {
    match pending_signal() {
        Some(sig) => Err(Error::Interrupted(sig)),
        None => Ok(()),
    }
}

/// SQLite WAL checkpoint modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointMode {
    /// Checkpoint as many frames as possible without waiting.
    Passive,
    /// Wait for writers, then checkpoint every frame.
    Full,
    /// Like `Full`, then wait for readers so the next writer restarts the log.
    Restart,
    /// Like `Restart`, then truncate the WAL file to zero bytes.
    Truncate,
}

impl CheckpointMode {
    /// All modes, in declaration order.
    pub const ALL: [CheckpointMode; 4] = [
        CheckpointMode::Passive,
        CheckpointMode::Full,
        CheckpointMode::Restart,
        CheckpointMode::Truncate,
    ];

    /// The SQLite spelling (`PASSIVE`, `FULL`, `RESTART`, `TRUNCATE`).
    pub fn as_str(self) -> &'static str {
        match self {
            CheckpointMode::Passive => "PASSIVE",
            CheckpointMode::Full => "FULL",
            CheckpointMode::Restart => "RESTART",
            CheckpointMode::Truncate => "TRUNCATE",
        }
    }

    /// Parse a mode name case-insensitively.
    pub fn from_name(name: &str) -> Option<CheckpointMode> {
        CheckpointMode::ALL
            .into_iter()
            .find(|mode| mode.as_str().eq_ignore_ascii_case(name))
    }
}

impl fmt::Display for CheckpointMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for CheckpointMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        CheckpointMode::from_name(s)
            .ok_or_else(|| Error::InvalidArgument(format!("invalid checkpoint mode: {s}")))
    }
}

/// Outcome class of a checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointStatus {
    /// Every WAL frame was checkpointed and nothing was busy.
    Complete,
    /// SQLite reported that the checkpoint was blocked.
    Busy,
    /// Not busy, but fewer frames were checkpointed than the WAL holds.
    Partial,
}

impl CheckpointStatus {
    /// `complete`, `busy` or `partial`.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckpointStatus::Complete => "complete",
            CheckpointStatus::Busy => "busy",
            CheckpointStatus::Partial => "partial",
        }
    }
}

impl fmt::Display for CheckpointStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of a WAL checkpoint operation.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointResult {
    /// Mode that was requested.
    pub mode: CheckpointMode,
    /// SQLite reported the checkpoint as blocked.
    pub busy: bool,
    /// Frames in the WAL (0 when the database is not in WAL mode).
    pub wal_pages: i64,
    /// Frames checkpointed (0 when the database is not in WAL mode).
    pub checkpointed_pages: i64,
    /// Resolved path of the database.
    pub database: PathBuf,
    /// Wall time in milliseconds, rounded to two decimals.
    pub elapsed_ms: f64,
}

impl CheckpointResult {
    /// True when not busy and every WAL frame was checkpointed.
    pub fn fully_checkpointed(&self) -> bool {
        !self.busy && self.wal_pages == self.checkpointed_pages
    }

    /// Complete, busy or partial.
    pub fn status(&self) -> CheckpointStatus {
        if self.busy {
            CheckpointStatus::Busy
        } else if self.fully_checkpointed() {
            CheckpointStatus::Complete
        } else {
            CheckpointStatus::Partial
        }
    }
}

/// Result of an atomic backup operation.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupResult {
    /// Resolved path of the source database.
    pub source: PathBuf,
    /// Destination path as given (lexically normalised).
    pub destination: PathBuf,
    /// Size of the published backup in bytes.
    pub size_bytes: u64,
    /// Lower-case hexadecimal SHA-256 of the published backup.
    pub sha256: String,
    /// Wall time in milliseconds, rounded to two decimals.
    pub elapsed_ms: f64,
}

/// Result of a checkpoint-then-backup snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotResult {
    /// The WAL checkpoint that ran first.
    pub checkpoint: CheckpointResult,
    /// The published backup.
    pub backup: BackupResult,
}

/// Basic information about a SQLite database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbInfo {
    /// Resolved path of the database.
    pub path: PathBuf,
    /// Size of the main database file in bytes.
    pub size_bytes: u64,
    /// Result of `PRAGMA journal_mode`.
    pub journal_mode: String,
    /// Whether `<path>-wal` exists as a regular file.
    pub wal_file_exists: bool,
    /// Size of the WAL file (0 when absent).
    pub wal_size_bytes: u64,
    /// `PRAGMA page_size`.
    pub page_size: i64,
    /// `PRAGMA page_count`.
    pub page_count: i64,
    /// `PRAGMA freelist_count`.
    pub freelist_count: i64,
}

/// Throttling controls for the online backup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackupOptions {
    /// Positive number of pages copied per backup step.
    pub pages_per_step: i64,
    /// Milliseconds to sleep when a backup step finds the source busy or locked.
    pub pause_ms: f64,
}

impl Default for BackupOptions {
    fn default() -> Self {
        BackupOptions {
            pages_per_step: 1000,
            pause_ms: 0.0,
        }
    }
}

/// Options for [`snapshot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapshotOptions {
    /// WAL checkpoint mode used before the backup.
    pub checkpoint_mode: CheckpointMode,
    /// Positive number of pages copied per backup step.
    pub pages_per_step: i64,
    /// Milliseconds to sleep when a backup step finds the source busy or locked.
    pub pause_ms: f64,
    /// Refuse to publish a backup after a busy or partial checkpoint.
    pub require_complete: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        SnapshotOptions {
            checkpoint_mode: CheckpointMode::Passive,
            pages_per_step: 1000,
            pause_ms: 0.0,
            require_complete: false,
        }
    }
}

/// Errors reported by the library. The variants mirror the exception classes
/// the original implementation raised, which determine the CLI exit code.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// A path did not exist or failed source validation (exit code 1).
    NotFound(String),
    /// The destination (or a temporary name) already exists, or the
    /// destination names a `-wal`/`-shm`/`-journal` file of the source
    /// (exit code 1).
    AlreadyExists(String),
    /// Invalid control values (exit code 2).
    InvalidArgument(String),
    /// An SQLite error, including a failed integrity check (exit code 2).
    Sqlite(String),
    /// Any other operating-system error (exit code 2).
    Os(String),
    /// A required complete checkpoint was busy or partial; no backup was
    /// published (exit code 3).
    IncompleteCheckpoint(Box<CheckpointResult>),
    /// The CLI received this termination signal during a backup; the
    /// temporary file was removed and no destination was left published
    /// (exit code 128 + signal if the signal cannot be re-raised).
    Interrupted(i32),
}

impl Error {
    /// The CLI exit code for this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::NotFound(_) | Error::AlreadyExists(_) => 1,
            Error::IncompleteCheckpoint(_) => 3,
            Error::Interrupted(sig) => 128 + sig,
            Error::InvalidArgument(_) | Error::Sqlite(_) | Error::Os(_) => 2,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound(msg)
            | Error::AlreadyExists(msg)
            | Error::InvalidArgument(msg)
            | Error::Sqlite(msg)
            | Error::Os(msg) => f.write_str(msg),
            Error::IncompleteCheckpoint(result) => {
                write!(f, "checkpoint {}; backup was not created", result.status())
            }
            Error::Interrupted(sig) => match *sig {
                libc::SIGHUP => f.write_str("interrupted by SIGHUP"),
                libc::SIGINT => f.write_str("interrupted by SIGINT"),
                libc::SIGTERM => f.write_str("interrupted by SIGTERM"),
                other => write!(f, "interrupted by signal {other}"),
            },
        }
    }
}

impl std::error::Error for Error {}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Run a WAL checkpoint on an existing SQLite database.
pub fn checkpoint(db_path: impl AsRef<Path>, mode: CheckpointMode) -> Result<CheckpointResult> {
    checkpoint_with(db_path.as_ref(), mode, &Hooks::default())
}

/// Create an atomic, consistent backup of a live database with the SQLite
/// online backup API. The destination must not exist; its parent directory
/// must exist.
pub fn backup(
    db_path: impl AsRef<Path>,
    dest_path: impl AsRef<Path>,
    options: &BackupOptions,
) -> Result<BackupResult> {
    backup_with(
        db_path.as_ref(),
        dest_path.as_ref(),
        options,
        &Hooks::default(),
    )
}

/// Optionally maintain the WAL before creating a consistent online backup.
///
/// A checkpoint is not needed for backup consistency; use this only when WAL
/// maintenance is also desired.
pub fn snapshot(
    db_path: impl AsRef<Path>,
    dest_path: impl AsRef<Path>,
    options: &SnapshotOptions,
) -> Result<SnapshotResult> {
    snapshot_with(
        db_path.as_ref(),
        dest_path.as_ref(),
        options,
        &Hooks::default(),
    )
}

/// Return basic information about a SQLite database.
pub fn db_info(db_path: impl AsRef<Path>) -> Result<DbInfo> {
    db_info_with(db_path.as_ref(), &Hooks::default())
}

type PathHook<'a, T> = Option<&'a dyn Fn(&Path) -> T>;
type CheckpointHook<'a> = Option<&'a dyn Fn(&Path, CheckpointMode) -> Result<CheckpointResult>>;

/// Fault-injection points used by the test suite in place of the Python
/// tests' monkeypatching. Production code always uses the defaults.
#[derive(Default)]
pub(crate) struct Hooks<'a> {
    /// Called with the URI right before a source connection is opened.
    pub before_connect: Option<&'a dyn Fn(&str)>,
    pub fsync_file: PathHook<'a, io::Result<()>>,
    pub fsync_directory: PathHook<'a, io::Result<()>>,
    pub quick_check: PathHook<'a, Result<()>>,
    pub checkpoint: CheckpointHook<'a>,
    /// Replaces `lstat` of the WAL file: returns (is_regular_file, size).
    pub wal_lstat: PathHook<'a, io::Result<(bool, u64)>>,
}

#[derive(Debug, Clone)]
struct DatabaseIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
}

fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(pyfmt::pathlib_str(path.as_os_str()))
}

fn shown(path: &Path) -> String {
    pyfmt::display_bytes(path.as_os_str().as_bytes())
}

fn os_error(err: &io::Error, filename: Option<&Path>, filename2: Option<&Path>) -> Error {
    let msg = pyfmt::os_error_message(
        err,
        filename.map(Path::as_os_str),
        filename2.map(Path::as_os_str),
    );
    match err.raw_os_error() {
        Some(libc::ENOENT) => Error::NotFound(msg),
        Some(libc::EEXIST) => Error::AlreadyExists(msg),
        _ => Error::Os(msg),
    }
}

fn sqlite_error(err: rusqlite::Error) -> Error {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => Error::Sqlite(msg),
        rusqlite::Error::SqliteFailure(code, None) => {
            Error::Sqlite(rusqlite::ffi::code_to_str(code.extended_code).to_string())
        }
        other => Error::Sqlite(other.to_string()),
    }
}

/// Error from `sqlite3_backup_step`: Python reports it after
/// `sqlite3_backup_finish`, whose message is `sqlite3_errstr(rc)`.
fn backup_step_error(err: rusqlite::Error) -> Error {
    match err {
        rusqlite::Error::SqliteFailure(code, _) => {
            Error::Sqlite(rusqlite::ffi::code_to_str(code.extended_code).to_string())
        }
        other => sqlite_error(other),
    }
}

fn open_connection(target: &OsStr, flags: OpenFlags) -> Result<Connection> {
    match Connection::open_with_flags(target, flags) {
        Ok(conn) => {
            conn.busy_timeout(BUSY_TIMEOUT).map_err(sqlite_error)?;
            Ok(conn)
        }
        Err(rusqlite::Error::SqliteFailure(code, Some(msg)))
            if code.code == rusqlite::ErrorCode::CannotOpen =>
        {
            // rusqlite appends ": <path>" to open failures; SQLite (and the
            // original tool) report only the bare message.
            let suffix = format!(": {}", target.to_string_lossy());
            let msg = msg.strip_suffix(&suffix).unwrap_or(&msg).to_string();
            Err(Error::Sqlite(msg))
        }
        Err(err) => Err(sqlite_error(err)),
    }
}

fn close_connection(conn: Connection) -> Result<()> {
    conn.close().map_err(|(_, err)| sqlite_error(err))
}

fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    (left.dev(), left.ino(), left.mode()) == (right.dev(), right.ino(), right.mode())
}

fn validated_existing_database_path(path: &Path) -> Result<DatabaseIdentity> {
    let initial = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => {
            return Err(Error::NotFound(format!(
                "Database not found: {}",
                shown(path)
            )));
        }
        Err(err) => return Err(os_error(&err, Some(path), None)),
    };
    if initial.file_type().is_symlink() || !initial.file_type().is_file() {
        return Err(Error::NotFound(format!(
            "Database is not a regular file: {}",
            shown(path)
        )));
    }
    let changed = || {
        Error::NotFound(format!(
            "Database path changed during validation: {}",
            shown(path)
        ))
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| {
            Error::NotFound(format!("Database cannot be opened safely: {}", shown(path)))
        })?;
    let opened = file.metadata().map_err(|err| os_error(&err, None, None))?;
    if !opened.file_type().is_file() || !same_identity(&initial, &opened) {
        return Err(changed());
    }
    let real_path = fs::canonicalize(path).map_err(|err| os_error(&err, Some(path), None))?;
    let resolved =
        fs::metadata(&real_path).map_err(|err| os_error(&err, Some(&real_path), None))?;
    if !same_identity(&opened, &resolved) {
        return Err(changed());
    }
    Ok(DatabaseIdentity {
        path: real_path,
        device: opened.dev(),
        inode: opened.ino(),
    })
}

fn connect_existing(
    identity: &DatabaseIdentity,
    readonly: bool,
    immutable: bool,
    hooks: &Hooks<'_>,
) -> Result<Connection> {
    let mut uri = String::from("file:");
    uri.push_str(&pyfmt::quote_path(identity.path.as_os_str().as_bytes()));
    uri.push_str(if readonly { "?mode=ro" } else { "?mode=rw" });
    if immutable {
        uri.push_str("&immutable=1");
    }
    if let Some(hook) = hooks.before_connect {
        hook(&uri);
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = open_connection(OsStr::new(&uri), flags)?;
    let verified = (|| {
        conn.query_row("PRAGMA schema_version", [], |_| Ok(()))
            .optional()
            .map_err(sqlite_error)?;
        let current = fs::metadata(&identity.path)
            .map_err(|err| os_error(&err, Some(&identity.path), None))?;
        if (current.dev(), current.ino()) != (identity.device, identity.inode) {
            return Err(Error::NotFound(format!(
                "Database path changed while opening: {}",
                shown(&identity.path)
            )));
        }
        Ok(())
    })();
    match verified {
        Ok(()) => Ok(conn),
        Err(err) => {
            drop(conn);
            Err(err)
        }
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    pyfmt::round2(start.elapsed().as_secs_f64() * 1000.0)
}

pub(crate) fn checkpoint_with(
    db_path: &Path,
    mode: CheckpointMode,
    hooks: &Hooks<'_>,
) -> Result<CheckpointResult> {
    let identity = validated_existing_database_path(&normalized(db_path))?;
    let start = Instant::now();
    let conn = connect_existing(&identity, false, false, hooks)?;
    // PRAGMA wal_checkpoint returns (busy, wal_pages, checkpointed_pages);
    // busy is 1 when a PASSIVE checkpoint was blocked by another connection.
    let row = conn
        .query_row(
            &format!("PRAGMA wal_checkpoint({})", mode.as_str()),
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(sqlite_error);
    let closed = close_connection(conn);
    let (busy, wal_pages, checkpointed) = row?;
    closed?;
    let elapsed = elapsed_ms(start);
    Ok(CheckpointResult {
        mode,
        busy: busy != 0,
        wal_pages: wal_pages.max(0),
        checkpointed_pages: checkpointed.max(0),
        database: identity.path,
        elapsed_ms: elapsed,
    })
}

pub(crate) fn validate_backup_controls(pages_per_step: i64, pause_ms: f64) -> Result<()> {
    if pages_per_step <= 0 {
        return Err(Error::InvalidArgument(
            "pages_per_step must be greater than zero".to_string(),
        ));
    }
    if !pause_ms.is_finite() || pause_ms < 0.0 {
        return Err(Error::InvalidArgument(
            "pause_ms must be a finite nonnegative number".to_string(),
        ));
    }
    Ok(())
}

/// Create a unique private file like `tempfile.mkstemp(prefix=..., dir=...)`
/// and return its absolute path. The descriptor is closed immediately.
fn make_temp_file(dir: &OsStr, prefix: &[u8]) -> Result<PathBuf> {
    const CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut base = pyfmt::abspath(dir.as_bytes()).map_err(|err| os_error(&err, None, None))?;
    if !base.ends_with(b"/") {
        base.push(b'/');
    }
    let state = RandomState::new();
    let mut counter: u64 = 0;
    for _ in 0..TMP_MAX {
        let mut name = base.clone();
        name.extend_from_slice(prefix);
        for _ in 0..8 {
            let mut hasher = state.build_hasher();
            hasher.write_u64(counter);
            counter += 1;
            name.push(CHARACTERS[(hasher.finish() % CHARACTERS.len() as u64) as usize]);
        }
        let candidate = PathBuf::from(OsString::from_vec(name));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                return Ok(candidate);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(os_error(&err, Some(&candidate), None)),
        }
    }
    Err(Error::AlreadyExists(format!(
        "[Errno {}] No usable temporary file name found",
        libc::EEXIST
    )))
}

fn fsync_file(path: &Path, hooks: &Hooks<'_>) -> Result<()> {
    if let Some(hook) = hooks.fsync_file {
        return hook(path).map_err(|err| os_error(&err, None, None));
    }
    let file = File::open(path).map_err(|err| os_error(&err, Some(path), None))?;
    file.sync_all().map_err(|err| os_error(&err, None, None))
}

fn fsync_directory(path: &Path, hooks: &Hooks<'_>) -> Result<()> {
    if let Some(hook) = hooks.fsync_directory {
        return hook(path).map_err(|err| os_error(&err, None, None));
    }
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
        .map_err(|err| os_error(&err, Some(path), None))?;
    dir.sync_all().map_err(|err| os_error(&err, None, None))
}

fn quick_check(path: &Path, hooks: &Hooks<'_>) -> Result<()> {
    if let Some(hook) = hooks.quick_check {
        return hook(path);
    }
    let identity = validated_existing_database_path(path)?;
    let conn = connect_existing(&identity, true, true, hooks)?;
    let result = conn
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .optional()
        .map_err(sqlite_error);
    let closed = close_connection(conn);
    let result = result?;
    closed?;
    match result.as_deref() {
        Some("ok") => Ok(()),
        Some(text) => Err(Error::Sqlite(format!(
            "SQLite quick_check failed for temporary backup: ({},)",
            pyfmt::py_repr(text.as_bytes())
        ))),
        None => Err(Error::Sqlite(
            "SQLite quick_check failed for temporary backup: None".to_string(),
        )),
    }
}

/// Size in bytes and lower-case hexadecimal SHA-256 of `path`, checking for
/// a pending termination signal between chunks.
fn sha256_file(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).map_err(|err| os_error(&err, Some(path), None))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size: u64 = 0;
    loop {
        check_interrupt()?;
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(os_error(&err, None, None)),
        };
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte & 0xf)]));
    }
    Ok((size, hex))
}

/// Copy `source` into `dest` with `sqlite3_backup_step`, sleeping only when a
/// step reports BUSY or LOCKED (the semantics of Python's
/// `Connection.backup(pages=..., sleep=...)`).
fn run_online_backup(
    source: &Connection,
    dest: &mut Connection,
    pages_per_step: i64,
    pause_ms: f64,
) -> Result<()> {
    let pages = i32::try_from(pages_per_step).map_err(|_| {
        Error::InvalidArgument(format!("pages_per_step must be at most {}", i32::MAX))
    })?;
    // Python converts the pause to seconds and SQLite back to whole
    // milliseconds with truncation.
    let sleep_ms = ((pause_ms / 1000.0) * 1000.0) as i32;
    let step = Backup::new(source, dest).map_err(sqlite_error)?;
    loop {
        let state = step.step(pages).map_err(backup_step_error)?;
        check_interrupt()?;
        match state {
            StepResult::Done => return Ok(()),
            StepResult::Busy | StepResult::Locked if sleep_ms > 0 => {
                interruptible_sleep(Duration::from_millis(sleep_ms as u64))?;
            }
            _ => {}
        }
    }
}

/// Sleep in short slices so a termination signal is noticed promptly even
/// with a very large `pause_ms`.
fn interruptible_sleep(total: Duration) -> Result<()> {
    let deadline = Instant::now() + total;
    loop {
        check_interrupt()?;
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(SLEEP_SLICE));
    }
}

fn copy_validate_publish(
    identity: &DatabaseIdentity,
    tmp_path: &Path,
    dest_path: &Path,
    dest_parent: &Path,
    options: &BackupOptions,
    hooks: &Hooks<'_>,
    published: &mut bool,
) -> Result<(u64, String)> {
    let source = connect_existing(identity, true, false, hooks)?;
    let mut dest = open_connection(
        tmp_path.as_os_str(),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // The temporary file is private, discarded on any failure, and checked
    // and fsynced explicitly before publication. SQLite's rollback journal
    // and commit-time syncs would only add a second write of every page and
    // extra fsyncs, so the copy runs without them.
    dest.query_row("PRAGMA journal_mode=OFF", [], |_| Ok(()))
        .map_err(sqlite_error)?;
    dest.execute_batch("PRAGMA synchronous=OFF")
        .map_err(sqlite_error)?;
    run_online_backup(&source, &mut dest, options.pages_per_step, options.pause_ms)?;
    close_connection(dest)?;
    close_connection(source)?;
    quick_check(tmp_path, hooks)?;
    check_interrupt()?;
    fsync_file(tmp_path, hooks)?;
    // Fingerprint the synced temporary file, so that nothing slow runs after
    // publication and a signal during hashing still publishes nothing. The
    // destination is a hard link to these exact bytes.
    let fingerprint = sha256_file(tmp_path)?;
    check_interrupt()?;
    // Atomic no-clobber publish: link succeeds only if dest_path does not exist.
    fs::hard_link(tmp_path, dest_path)
        .map_err(|err| os_error(&err, Some(tmp_path), Some(dest_path)))?;
    *published = true;
    // Drop the temporary name first: the one directory sync below then makes
    // both the new name and the removal durable, so a crash after success is
    // reported cannot leave a second link to the backup behind. A crash before
    // that sync may leave either name (or both), but success is not reported.
    let _ = fs::remove_file(tmp_path);
    fsync_directory(dest_parent, hooks)?;
    // A signal that arrived while publishing rolls the new name back, as the
    // original rolled back on KeyboardInterrupt inside its try block.
    check_interrupt()?;
    Ok(fingerprint)
}

/// Refuse a destination that names the source's own `-wal`, `-shm` or
/// `-journal` file. A backup published there would later be taken for (and
/// reset or deleted as) the source's write-ahead log, shared-memory index or
/// rollback journal.
fn reject_sidecar_destination(
    source: &Path,
    dest_path: &Path,
    name: &OsStr,
    parent: &Path,
) -> Result<()> {
    let Ok(parent) = fs::canonicalize(parent) else {
        // A missing or unreadable directory is reported by the temp-file step.
        return Ok(());
    };
    let candidate = parent.join(name);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = source.as_os_str().to_os_string();
        sidecar.push(suffix);
        if candidate.as_os_str() == sidecar {
            return Err(Error::AlreadyExists(format!(
                "Destination is the {suffix} file of the source database: {}",
                shown(dest_path)
            )));
        }
    }
    Ok(())
}

/// A destination that passed the pre-copy checks, with its private temporary
/// file already created.
struct PreparedDestination {
    dest_path: PathBuf,
    parent: PathBuf,
    tmp_path: PathBuf,
}

impl PreparedDestination {
    fn discard(self) {
        let _ = fs::remove_file(&self.tmp_path);
    }
}

/// Checks that `dest_path` is free and is not one of the source's sidecar
/// names, then creates the private temporary file in its directory (which
/// also reports a missing or unwritable directory).
fn prepare_destination(source: &Path, dest_path: &Path) -> Result<PreparedDestination> {
    let dest_path = normalized(dest_path);

    // lstat: a dangling symlink also occupies the name and would make the
    // final no-clobber link fail only after the whole copy.
    if fs::symlink_metadata(&dest_path).is_ok() {
        return Err(Error::AlreadyExists(format!(
            "Destination already exists: {}",
            shown(&dest_path)
        )));
    }

    let (name, parent) = pyfmt::pathlib_split(dest_path.as_os_str());
    let parent = PathBuf::from(parent);
    reject_sidecar_destination(source, &dest_path, &name, &parent)?;
    let mut prefix = b".".to_vec();
    prefix.extend_from_slice(name.as_bytes());
    prefix.extend_from_slice(b".tmp-");
    let tmp_path = make_temp_file(parent.as_os_str(), &prefix)?;
    Ok(PreparedDestination {
        dest_path,
        parent,
        tmp_path,
    })
}

pub(crate) fn backup_with(
    db_path: &Path,
    dest_path: &Path,
    options: &BackupOptions,
    hooks: &Hooks<'_>,
) -> Result<BackupResult> {
    validate_backup_controls(options.pages_per_step, options.pause_ms)?;
    let identity = validated_existing_database_path(&normalized(db_path))?;
    let prepared = prepare_destination(&identity.path, dest_path)?;
    backup_into(identity, prepared, options, hooks)
}

/// Copies the source into the prepared temporary file and publishes it. The
/// temporary file is removed on every error.
fn backup_into(
    identity: DatabaseIdentity,
    prepared: PreparedDestination,
    options: &BackupOptions,
    hooks: &Hooks<'_>,
) -> Result<BackupResult> {
    let start = Instant::now();
    let mut published = false;
    let outcome = copy_validate_publish(
        &identity,
        &prepared.tmp_path,
        &prepared.dest_path,
        &prepared.parent,
        options,
        hooks,
        &mut published,
    );
    let (size_bytes, sha256) = match outcome {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            if published {
                // Best-effort rollback after a post-publish sync failure. The
                // caller must inspect the destination if this cleanup also fails.
                let _ = fs::remove_file(&prepared.dest_path);
                let _ = fsync_directory(&prepared.parent, hooks);
            }
            prepared.discard();
            return Err(err);
        }
    };
    let elapsed = elapsed_ms(start);
    Ok(BackupResult {
        source: identity.path,
        destination: prepared.dest_path,
        size_bytes,
        sha256,
        elapsed_ms: elapsed,
    })
}

pub(crate) fn snapshot_with(
    db_path: &Path,
    dest_path: &Path,
    options: &SnapshotOptions,
    hooks: &Hooks<'_>,
) -> Result<SnapshotResult> {
    validate_backup_controls(options.pages_per_step, options.pause_ms)?;
    // Check the destination before the checkpoint, which writes to the source
    // and can wait out the busy timeout: a destination that cannot be used is
    // reported at once and leaves the source untouched.
    let identity = validated_existing_database_path(&normalized(db_path))?;
    let prepared = prepare_destination(&identity.path, dest_path)?;
    let checkpoint = match hooks.checkpoint {
        Some(hook) => hook(db_path, options.checkpoint_mode),
        None => checkpoint_with(db_path, options.checkpoint_mode, hooks),
    };
    let checkpoint = match checkpoint {
        Ok(checkpoint) if !options.require_complete || checkpoint.fully_checkpointed() => {
            checkpoint
        }
        Ok(checkpoint) => {
            prepared.discard();
            return Err(Error::IncompleteCheckpoint(Box::new(checkpoint)));
        }
        Err(err) => {
            prepared.discard();
            return Err(err);
        }
    };
    // Validate the source again: the backup must copy the file that is at
    // the path now.
    let identity = match validated_existing_database_path(&normalized(db_path)) {
        Ok(identity) => identity,
        Err(err) => {
            prepared.discard();
            return Err(err);
        }
    };
    let backup = backup_into(
        identity,
        prepared,
        &BackupOptions {
            pages_per_step: options.pages_per_step,
            pause_ms: options.pause_ms,
        },
        hooks,
    )?;
    Ok(SnapshotResult { checkpoint, backup })
}

/// Regular-file presence and size of `path`, tolerating races where the file
/// disappears (any lstat error means "absent").
fn file_context(path: &Path, hooks: &Hooks<'_>) -> (bool, u64) {
    let probe = match hooks.wal_lstat {
        Some(hook) => hook(path),
        None => fs::symlink_metadata(path).map(|meta| (meta.file_type().is_file(), meta.len())),
    };
    match probe {
        Ok((true, size)) => (true, size),
        _ => (false, 0),
    }
}

pub(crate) fn db_info_with(db_path: &Path, hooks: &Hooks<'_>) -> Result<DbInfo> {
    let identity = validated_existing_database_path(&normalized(db_path))?;
    let mut wal = identity.path.clone().into_os_string();
    wal.push("-wal");
    let wal_path = PathBuf::from(wal);

    let conn = connect_existing(&identity, true, false, hooks)?;
    let pragmas = (|| -> rusqlite::Result<(String, i64, i64, i64)> {
        Ok((
            conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?,
            conn.query_row("PRAGMA page_size", [], |row| row.get(0))?,
            conn.query_row("PRAGMA page_count", [], |row| row.get(0))?,
            conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?,
        ))
    })()
    .map_err(sqlite_error);
    let closed = close_connection(conn);
    let (journal_mode, page_size, page_count, freelist_count) = pragmas?;
    closed?;

    let (wal_file_exists, wal_size_bytes) = file_context(&wal_path, hooks);
    let size_bytes = fs::metadata(&identity.path)
        .map_err(|err| os_error(&err, Some(&identity.path), None))?
        .len();
    Ok(DbInfo {
        path: identity.path,
        size_bytes,
        journal_mode,
        wal_file_exists,
        wal_size_bytes,
        page_size,
        page_count,
        freelist_count,
    })
}

#[cfg(test)]
mod tests;
