//! Core library tests, ported one-for-one from the original `tests/test_core.py`
//! (fault injection uses [`Hooks`] where the Python suite monkeypatched).

use super::*;
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

/// The `wal_db` fixture: a WAL-mode database with two rows.
fn wal_db() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let conn = Connection::open(&path).expect("open");
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .expect("wal");
    conn.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO items (name) VALUES ('alpha');
         INSERT INTO items (name) VALUES ('bravo');",
    )
    .expect("seed");
    conn.close().expect("close");
    (dir, path)
}

fn names(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).expect("open backup");
    let mut stmt = conn
        .prepare("SELECT name FROM items ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |row| row.get(0))
        .expect("query")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("rows")
}

fn temp_leftovers(dir: &Path, prefix: &str) -> Vec<String> {
    fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix))
        .collect()
}

fn result(mode: CheckpointMode, busy: bool, wal: i64, ckpt: i64) -> CheckpointResult {
    CheckpointResult {
        mode,
        busy,
        wal_pages: wal,
        checkpointed_pages: ckpt,
        database: PathBuf::from("db"),
        elapsed_ms: 1.0,
    }
}

// --- TestCheckpoint -------------------------------------------------------

#[test]
fn test_passive_checkpoint() {
    let (_dir, db) = wal_db();
    let result = checkpoint(&db, CheckpointMode::Passive).unwrap();
    assert_eq!(result.mode, CheckpointMode::Passive);
    assert!(!result.busy);
    assert!(result.elapsed_ms >= 0.0);
    assert_eq!(result.database, fs::canonicalize(&db).unwrap());
}

#[test]
fn test_truncate_checkpoint() {
    let (_dir, db) = wal_db();
    let result = checkpoint(&db, CheckpointMode::Truncate).unwrap();
    assert!(result.fully_checkpointed());
    // After TRUNCATE the WAL file should be empty or removed.
    let wal = db.with_extension("db-wal");
    assert!(!wal.exists() || fs::metadata(&wal).unwrap().len() == 0);
}

#[test]
fn test_checkpoint_missing_database() {
    let dir = TempDir::new().unwrap();
    let err = checkpoint(dir.path().join("nope.db"), CheckpointMode::Passive).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert_eq!(err.exit_code(), 1);
    assert!(!dir.path().join("nope.db").exists());
}

#[test]
fn test_status_distinguishes_complete_busy_and_partial() {
    let complete = result(CheckpointMode::Passive, false, 2, 2);
    let busy = result(CheckpointMode::Passive, true, 2, 1);
    let partial = result(CheckpointMode::Passive, false, 2, 1);

    assert_eq!(complete.status(), CheckpointStatus::Complete);
    assert_eq!(busy.status(), CheckpointStatus::Busy);
    assert_eq!(partial.status(), CheckpointStatus::Partial);
    assert_eq!(complete.status().as_str(), "complete");
    assert_eq!(busy.status().as_str(), "busy");
    assert_eq!(partial.status().as_str(), "partial");
}

// --- TestBackup -----------------------------------------------------------

#[test]
fn test_basic_backup() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("backup.db");
    let result = backup(&db, &dest, &BackupOptions::default()).unwrap();
    assert!(dest.exists());
    assert!(result.size_bytes > 0);
    assert_eq!(result.sha256.len(), 64);
    assert_eq!(
        fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(result.destination, dest);
    assert_eq!(result.sha256, sha256_file(&dest).unwrap());
}

#[test]
fn test_backup_is_valid_database() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("backup.db");
    backup(&db, &dest, &BackupOptions::default()).unwrap();
    assert_eq!(names(&dest), ["alpha", "bravo"]);
}

#[test]
fn test_backup_can_be_restored_and_passes_integrity_check() {
    let (dir, db) = wal_db();
    let backup_path = dir.path().join("backup.db");
    let restored_path = dir.path().join("restored.db");
    backup(&db, &backup_path, &BackupOptions::default()).unwrap();
    let source = Connection::open(&backup_path).unwrap();
    let mut restored = Connection::open(&restored_path).unwrap();
    Backup::new(&source, &mut restored)
        .unwrap()
        .run_to_completion(100, Duration::ZERO, None)
        .unwrap();
    let integrity: String = restored
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(restored);
    assert_eq!(names(&restored_path), ["alpha", "bravo"]);
}

#[test]
fn test_sync_failure_before_publish_leaves_no_destination() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("backup.db");
    let failing = |_: &Path| Err(io::Error::other("sync failed"));
    let hooks = Hooks {
        fsync_file: Some(&failing),
        ..Hooks::default()
    };
    let err = backup_with(&db, &destination, &BackupOptions::default(), &hooks).unwrap_err();
    assert_eq!(err.to_string(), "sync failed");
    assert_eq!(err.exit_code(), 2);
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".backup.db.tmp-").is_empty());
}

#[test]
fn test_directory_sync_failure_rolls_back_published_name() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("backup.db");
    let failing = |_: &Path| Err(io::Error::other("sync failed"));
    let hooks = Hooks {
        fsync_directory: Some(&failing),
        ..Hooks::default()
    };
    let err = backup_with(&db, &destination, &BackupOptions::default(), &hooks).unwrap_err();
    assert_eq!(err.to_string(), "sync failed");
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".backup.db.tmp-").is_empty());
}

#[test]
fn test_backup_refuses_overwrite() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("backup.db");
    fs::write(&dest, b"existing").unwrap();
    let err = backup(&db, &dest, &BackupOptions::default()).unwrap_err();
    assert!(matches!(err, Error::AlreadyExists(_)), "{err:?}");
    assert_eq!(err.exit_code(), 1);
    assert_eq!(fs::read(&dest).unwrap(), b"existing");
}

#[test]
fn test_backup_leaves_no_temp_file() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("backup.db");
    backup(&db, &dest, &BackupOptions::default()).unwrap();
    assert!(dest.exists());
    assert!(temp_leftovers(dir.path(), ".backup.db.tmp-").is_empty());
}

#[test]
fn test_missing_source() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("nope.db");
    let destination = dir.path().join("out.db");
    let err = backup(&source, &destination, &BackupOptions::default()).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(!source.exists());
    assert!(!destination.exists());
}

fn rejects_non_regular_source(kind: &str) {
    let (dir, db) = wal_db();
    let source = dir.path().join("source");
    if kind == "directory" {
        fs::create_dir(&source).unwrap();
    } else {
        std::os::unix::fs::symlink(&db, &source).unwrap();
    }
    let destination = dir.path().join("out.db");
    let err = backup(&source, &destination, &BackupOptions::default()).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(
        err.to_string()
            .starts_with("Database is not a regular file: ")
    );
    assert!(!destination.exists());
}

#[test]
fn test_backup_rejects_non_regular_source_directory() {
    rejects_non_regular_source("directory");
}

#[test]
fn test_backup_rejects_non_regular_source_symlink() {
    rejects_non_regular_source("symlink");
}

#[test]
fn test_backup_accepts_read_only_source() {
    let (dir, db) = wal_db();
    fs::set_permissions(&db, fs::Permissions::from_mode(0o444)).unwrap();
    let destination = dir.path().join("readonly-backup.db");
    backup(&db, &destination, &BackupOptions::default()).unwrap();
    assert!(destination.exists());
}

#[test]
fn test_source_replacement_between_validation_and_connect_fails_closed() {
    let (dir, db) = wal_db();
    let replacement = dir.path().join("replacement.db");
    {
        let conn = Connection::open(&replacement).unwrap();
        conn.execute_batch("CREATE TABLE replacement(value TEXT)")
            .unwrap();
    }
    let destination = dir.path().join("out.db");
    let replaced = Cell::new(false);
    let replacing = |uri: &str| {
        if !replaced.get() && uri.contains("mode=ro") {
            replaced.set(true);
            fs::rename(&replacement, &db).unwrap();
        }
    };
    let hooks = Hooks {
        before_connect: Some(&replacing),
        ..Hooks::default()
    };
    let err = backup_with(&db, &destination, &BackupOptions::default(), &hooks).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(err.to_string().contains("changed"), "{err}");
    assert!(replaced.get());
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".out.db.tmp-").is_empty());
}

#[test]
fn test_source_deletion_between_validation_and_connect_never_recreates_it() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("out.db");
    let deleted = Cell::new(false);
    let deleting = |uri: &str| {
        if !deleted.get() && uri.contains("mode=ro") {
            deleted.set(true);
            fs::remove_file(&db).unwrap();
        }
    };
    let hooks = Hooks {
        before_connect: Some(&deleting),
        ..Hooks::default()
    };
    let err = backup_with(&db, &destination, &BackupOptions::default(), &hooks).unwrap_err();
    assert!(matches!(err, Error::Sqlite(_)), "{err:?}");
    assert_eq!(err.to_string(), "unable to open database file");
    assert!(!db.exists());
    assert!(!destination.exists());
}

fn rejects_invalid_page_steps(pages_per_step: i64) {
    let (dir, db) = wal_db();
    let destination = dir.path().join("out.db");
    let options = BackupOptions {
        pages_per_step,
        ..BackupOptions::default()
    };
    let err = backup(&db, &destination, &options).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert!(err.to_string().contains("pages_per_step"));
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".out.db.tmp-").is_empty());
}

#[test]
fn test_backup_rejects_invalid_page_steps_before_output_0() {
    rejects_invalid_page_steps(0);
}

#[test]
fn test_backup_rejects_invalid_page_steps_before_output_neg1() {
    rejects_invalid_page_steps(-1);
}

// Python's `True` and `1.5` cases are type errors in Rust; they are ported
// as CLI-level tests in `cli::tests` (`--pages-per-step True` / `1.5`).

#[test]
fn backup_rejects_most_negative_page_steps() {
    rejects_invalid_page_steps(i64::MIN);
}

/// Page counts above the C `int` range are refused before any destination
/// is published.
#[test]
fn backup_rejects_page_steps_above_c_int() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("out.db");
    let options = BackupOptions {
        pages_per_step: i64::from(i32::MAX) + 1,
        ..BackupOptions::default()
    };
    let err = backup(&db, &destination, &options).unwrap_err();
    assert!(err.to_string().contains("pages_per_step"), "{err}");
    assert_eq!(err.exit_code(), 2);
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".out.db.tmp-").is_empty());
}

fn rejects_invalid_pause(pause_ms: f64) {
    let (dir, db) = wal_db();
    let destination = dir.path().join("out.db");
    let options = BackupOptions {
        pause_ms,
        ..BackupOptions::default()
    };
    let err = backup(&db, &destination, &options).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert!(err.to_string().contains("pause_ms"));
    assert!(!destination.exists());
}

#[test]
fn test_backup_rejects_invalid_pause_before_output_neg1() {
    rejects_invalid_pause(-1.0);
}

// Python's `True` pause case is a type error in Rust; it is ported as a
// CLI-level test in `cli::tests` (`--pause-ms True`).

#[test]
fn backup_rejects_negative_infinite_pause() {
    rejects_invalid_pause(f64::NEG_INFINITY);
}

#[test]
fn test_backup_rejects_invalid_pause_before_output_inf() {
    rejects_invalid_pause(f64::INFINITY);
}

#[test]
fn test_backup_rejects_invalid_pause_before_output_nan() {
    rejects_invalid_pause(f64::NAN);
}

#[test]
fn test_failed_validation_never_publishes_destination() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("out.db");
    let failing = |_: &Path| Err(Error::Sqlite("injected".to_string()));
    let hooks = Hooks {
        quick_check: Some(&failing),
        ..Hooks::default()
    };
    let err = backup_with(&db, &destination, &BackupOptions::default(), &hooks).unwrap_err();
    assert_eq!(err, Error::Sqlite("injected".to_string()));
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".out.db.tmp-").is_empty());
}

// --- TestSnapshot ---------------------------------------------------------

#[test]
fn test_snapshot_creates_valid_backup() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("snap.db");
    let result = snapshot(&db, &dest, &SnapshotOptions::default()).unwrap();
    assert!(dest.exists());
    assert_eq!(result.backup.destination, dest);
    let conn = Connection::open(&dest).unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn test_invalid_controls_fail_before_checkpoint() {
    let (dir, db) = wal_db();
    let must_not_run = |_: &Path, _: CheckpointMode| -> Result<CheckpointResult> {
        panic!("checkpoint must not run")
    };
    let hooks = Hooks {
        checkpoint: Some(&must_not_run),
        ..Hooks::default()
    };
    let options = SnapshotOptions {
        pages_per_step: 0,
        ..SnapshotOptions::default()
    };
    let err = snapshot_with(&db, &dir.path().join("snap.db"), &options, &hooks).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert!(err.to_string().contains("pages_per_step"));
}

#[test]
fn test_required_checkpoint_failure_does_not_create_backup() {
    let (dir, db) = wal_db();
    let incomplete = CheckpointResult {
        database: db.clone(),
        ..result(CheckpointMode::Passive, false, 10, 4)
    };
    let fake = |_: &Path, _: CheckpointMode| Ok(incomplete.clone());
    let hooks = Hooks {
        checkpoint: Some(&fake),
        ..Hooks::default()
    };
    let destination = dir.path().join("snap.db");
    let options = SnapshotOptions {
        require_complete: true,
        ..SnapshotOptions::default()
    };
    let err = snapshot_with(&db, &destination, &options, &hooks).unwrap_err();
    match &err {
        Error::IncompleteCheckpoint(result) => assert_eq!(**result, incomplete),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(err.exit_code(), 3);
    assert_eq!(
        err.to_string(),
        "checkpoint partial; backup was not created"
    );
    assert!(!destination.exists());
}

// --- TestDbInfo -----------------------------------------------------------

#[test]
fn test_info_returns_expected_keys() {
    let (_dir, db) = wal_db();
    let info = db_info(&db).unwrap();
    assert_eq!(info.journal_mode, "wal");
    assert!(info.page_count >= 1);
    assert!(info.size_bytes > 0);
    assert_eq!(info.path, fs::canonicalize(&db).unwrap());
}

#[test]
fn test_info_missing_database() {
    let dir = TempDir::new().unwrap();
    let err = db_info(dir.path().join("nope.db")).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
}

#[test]
fn test_wal_disappearance_during_context_read_is_tolerated() {
    let (_dir, db) = wal_db();
    let disappearing = |path: &Path| {
        assert!(path.as_os_str().as_bytes().ends_with(b"-wal"));
        Err(io::Error::from_raw_os_error(libc::ENOENT))
    };
    let hooks = Hooks {
        wal_lstat: Some(&disappearing),
        ..Hooks::default()
    };
    let info = db_info_with(&db, &hooks).unwrap();
    assert!(!info.wal_file_exists);
    assert_eq!(info.wal_size_bytes, 0);
}

// --- Additional Rust-port coverage ----------------------------------------

#[test]
fn info_on_extensionless_database_reads_its_wal() {
    // The Python implementation raised "Invalid suffix '-wal'" here.
    let (dir, db) = wal_db();
    let plain = dir.path().join("appdata");
    fs::rename(&db, &plain).unwrap();
    let _ = fs::rename(db.with_extension("db-wal"), dir.path().join("appdata-wal"));
    let _ = fs::rename(db.with_extension("db-shm"), dir.path().join("appdata-shm"));
    let info = db_info(&plain).unwrap();
    assert_eq!(info.journal_mode, "wal");
    assert_eq!(
        info.wal_file_exists,
        dir.path().join("appdata-wal").exists()
    );
}

#[test]
fn backup_of_non_database_fails_without_output() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("garbage.db");
    fs::write(&source, b"definitely not sqlite ".repeat(64)).unwrap();
    let destination = dir.path().join("out.db");
    let err = backup(&source, &destination, &BackupOptions::default()).unwrap_err();
    assert_eq!(err, Error::Sqlite("file is not a database".to_string()));
    assert!(!destination.exists());
    assert!(temp_leftovers(dir.path(), ".out.db.tmp-").is_empty());
}

#[test]
fn backup_into_missing_directory_reports_temp_path() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("missing").join("out.db");
    let err = backup(&db, &destination, &BackupOptions::default()).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    let text = err.to_string();
    assert!(
        text.starts_with("[Errno 2] No such file or directory: '"),
        "{text}"
    );
    assert!(text.contains("/missing/.out.db.tmp-"), "{text}");
}

#[test]
fn quick_check_reports_python_tuple_repr() {
    let (dir, _db) = wal_db();
    let corrupt = dir.path().join("corrupt.db");
    {
        let conn = Connection::open(&corrupt).unwrap();
        conn.execute_batch(
            "CREATE TABLE t(a); CREATE INDEX ta ON t(a);
             WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<2000)
             INSERT INTO t SELECT randomblob(50) FROM c;",
        )
        .unwrap();
    }
    // Damage an interior b-tree page while keeping the header valid.
    let mut bytes = fs::read(&corrupt).unwrap();
    let page = 4096;
    for b in &mut bytes[page * 3 + 8..page * 3 + 200] {
        *b = 0xFF;
    }
    fs::write(&corrupt, &bytes).unwrap();
    match quick_check(&corrupt, &Hooks::default()) {
        Err(Error::Sqlite(msg)) => {
            assert!(
                msg.starts_with("SQLite quick_check failed for temporary backup: ('")
                    || msg == "database disk image is malformed",
                "{msg}"
            );
        }
        other => panic!("expected a quick_check failure, got {other:?}"),
    }
}

#[test]
fn partial_checkpoint_with_open_reader_is_reported() {
    let (dir, db) = wal_db();
    let reader = Connection::open(&db).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
        .unwrap();
    let writer = Connection::open(&db).unwrap();
    writer
        .execute_batch("INSERT INTO items (name) VALUES ('charlie')")
        .unwrap();
    let result = checkpoint(&db, CheckpointMode::Passive).unwrap();
    assert_eq!(result.status(), CheckpointStatus::Partial, "{result:?}");
    let options = SnapshotOptions {
        require_complete: true,
        ..SnapshotOptions::default()
    };
    let destination = dir.path().join("snap.db");
    let err = snapshot(&db, &destination, &options).unwrap_err();
    assert_eq!(err.exit_code(), 3);
    assert!(!destination.exists());
    drop(reader);
}

#[test]
fn checkpoint_mode_names() {
    for mode in CheckpointMode::ALL {
        assert_eq!(
            CheckpointMode::from_name(&mode.as_str().to_lowercase()),
            Some(mode)
        );
        assert_eq!(mode.as_str().parse::<CheckpointMode>().unwrap(), mode);
    }
    assert!("bogus".parse::<CheckpointMode>().is_err());
}

#[test]
fn temp_file_is_private_and_unique() {
    let dir = TempDir::new().unwrap();
    let a = make_temp_file(dir.path().as_os_str(), b".x.tmp-").unwrap();
    let b = make_temp_file(dir.path().as_os_str(), b".x.tmp-").unwrap();
    assert_ne!(a, b);
    for path in [&a, &b] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let name = path.file_name().unwrap().to_str().unwrap();
        let suffix = name.strip_prefix(".x.tmp-").unwrap();
        assert_eq!(suffix.len(), 8);
        assert!(
            suffix
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        );
    }
}

#[test]
fn interrupted_error_reports_signal_and_exit_code() {
    assert_eq!(Error::Interrupted(2).to_string(), "interrupted by SIGINT");
    assert_eq!(Error::Interrupted(15).to_string(), "interrupted by SIGTERM");
    assert_eq!(Error::Interrupted(1).to_string(), "interrupted by SIGHUP");
    assert_eq!(
        Error::Interrupted(10).to_string(),
        "interrupted by signal 10"
    );
    assert_eq!(Error::Interrupted(2).exit_code(), 130);
}

#[test]
fn interruptible_sleep_completes_without_a_signal() {
    let start = Instant::now();
    interruptible_sleep(Duration::from_millis(30)).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(30));
}
