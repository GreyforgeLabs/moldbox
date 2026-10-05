//! End-to-end tests of the `moldbox` binary and its deprecated
//! `sqlite-checkpoint` alias.
//!
//! The differential test at the bottom compares this binary with the original
//! Python implementation (released as sqlite-checkpoint 0.3.0, module
//! `sqlite_checkpoint`) when `SQLITE_CHECKPOINT_PY_SRC` points at its `src/`
//! directory (it is skipped otherwise). The reference runs with its argparse
//! program name set to `moldbox`, so help and usage layout is compared byte for
//! byte; `normalise` additionally maps both program names to one token.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use assert_cmd::Command;
use rusqlite::Connection;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("moldbox").expect("binary")
}

fn wal_db(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let conn = Connection::open(&path).expect("open");
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
        .expect("wal");
    conn.execute_batch(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO items (name) VALUES ('alpha');
         INSERT INTO items (name) VALUES ('bravo');",
    )
    .expect("seed");
    path
}

fn stdout_of(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().expect("run");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn version_prints_crate_version() {
    bin()
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("moldbox {}\n", env!("CARGO_PKG_VERSION")))
        .stderr("");
}

#[test]
fn no_arguments_is_a_usage_error() {
    let (code, stdout, stderr) = stdout_of(&mut bin());
    assert_eq!(code, 2);
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        "usage: moldbox [-h] [--version] [--json]\n               \
         {checkpoint,cp,backup,bk,snapshot,snap,info} ...\nmoldbox: error: the \
         following arguments are required: command\n"
    );
}

#[test]
fn backup_publishes_private_copy_and_json_report() {
    let dir = TempDir::new().unwrap();
    let db = wal_db(dir.path(), "app.db");
    let (code, stdout, stderr) =
        stdout_of(
            bin()
                .current_dir(dir.path())
                .args(["--json", "backup", "app.db", "./copy.db/"]),
        );
    assert_eq!(code, 0, "{stderr}");
    let dest = dir.path().join("copy.db");
    assert_eq!(
        fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let source = fs::canonicalize(&db).unwrap();
    assert!(
        stdout.starts_with(&format!(
            "{{\"command\": \"backup\", \"source\": \"{}\", \"destination\": \"copy.db\", \
             \"size_bytes\": {}, \"sha256\": \"",
            source.display(),
            fs::metadata(&dest).unwrap().len()
        )),
        "{stdout}"
    );
    // A second run refuses to overwrite.
    let (code, _, stderr) = stdout_of(
        bin()
            .current_dir(dir.path())
            .args(["backup", "app.db", "copy.db"]),
    );
    assert_eq!(code, 1);
    assert_eq!(stderr, "error: Destination already exists: copy.db\n");
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn require_complete_exits_3_for_partial_checkpoint_and_publishes_nothing() {
    let dir = TempDir::new().unwrap();
    let db = wal_db(dir.path(), "app.db");
    // Hold a read snapshot so later frames cannot be checkpointed.
    let reader = Connection::open(&db).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
        .unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch("INSERT INTO items (name) VALUES ('charlie')")
        .unwrap();

    let (code, stdout, _) = stdout_of(bin().arg("checkpoint").arg(&db).arg("--require-complete"));
    assert_eq!(code, 3);
    assert!(
        stdout.starts_with("checkpoint PASSIVE [partial]\n"),
        "{stdout}"
    );

    let dest = dir.path().join("snap.db");
    let (code, stdout, stderr) = stdout_of(
        bin()
            .args(["--json", "snapshot"])
            .arg(&db)
            .arg(&dest)
            .arg("--require-complete"),
    );
    assert_eq!(code, 3, "{stderr}");
    assert!(stdout.starts_with(
        "{\"command\": \"snapshot\", \"checkpoint\": {\"mode\": \"PASSIVE\", \"status\": \"partial\""
    ));
    assert!(
        stdout.ends_with("\"backup\": null, \"published\": false}\n"),
        "{stdout}"
    );
    assert!(!dest.exists());
    drop(reader);
}

#[test]
fn info_reports_text_and_json() {
    let dir = TempDir::new().unwrap();
    let db = wal_db(dir.path(), "app.db");
    let (code, stdout, _) = stdout_of(bin().arg("info").arg(&db));
    assert_eq!(code, 0);
    assert!(stdout.starts_with("database info\n  path:               "));
    assert!(stdout.contains("  journal mode:       wal\n"));
    assert!(stdout.contains("  page size:          4,096\n"));
    let (code, stdout, _) = stdout_of(bin().args(["--json", "info"]).arg(&db));
    assert_eq!(code, 0);
    assert!(stdout.contains("\"journal_mode\": \"wal\", \"wal_file_exists\": "));
}

#[test]
fn errors_use_documented_exit_codes() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("garbage.db"), b"not sqlite ".repeat(100)).unwrap();
    let (code, _, stderr) = stdout_of(bin().current_dir(dir.path()).args(["info", "missing.db"]));
    assert_eq!(
        (code, stderr.as_str()),
        (1, "error: Database not found: missing.db\n")
    );
    let (code, _, stderr) = stdout_of(bin().current_dir(dir.path()).args(["info", "garbage.db"]));
    assert_eq!(
        (code, stderr.as_str()),
        (2, "error: file is not a database\n")
    );
    let (code, _, stderr) = stdout_of(bin().current_dir(dir.path()).args(["info", "."]));
    assert_eq!(
        (code, stderr.as_str()),
        (1, "error: Database is not a regular file: .\n")
    );
}

#[test]
fn non_utf8_paths_round_trip() {
    let dir = TempDir::new().unwrap();
    let name = OsStr::from_bytes(b"caf\xff.db");
    wal_db(dir.path(), "seed.db");
    fs::rename(dir.path().join("seed.db"), dir.path().join(name)).unwrap();
    let _ = fs::remove_file(dir.path().join("seed.db-wal"));
    let _ = fs::remove_file(dir.path().join("seed.db-shm"));
    let out = bin()
        .current_dir(dir.path())
        .args([
            OsStr::new("--json"),
            OsStr::new("backup"),
            name,
            OsStr::new("out\u{e9}.db"),
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("caf\\udcff.db\""), "{stdout}");
    assert!(
        stdout.contains("\"destination\": \"out\\u00e9.db\""),
        "{stdout}"
    );
}

// --- Differential test against the Python reference ------------------------

/// A termination signal while the backup waits on a locked source removes the
/// private temporary file, publishes nothing, and the process then dies by
/// the same signal (the original removed its temp file on Ctrl-C).
#[test]
fn interrupted_backup_removes_temporary_file() {
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    let dir = TempDir::new().unwrap();
    let db = dir.path().join("locked.db");
    let holder = Connection::open(&db).unwrap();
    holder
        .execute_batch("CREATE TABLE t(v); BEGIN EXCLUSIVE; INSERT INTO t VALUES (1);")
        .unwrap();
    for (signal, number) in [("-INT", 2), ("-TERM", 15)] {
        let dest = dir.path().join("out.db");
        let child = Process::new(assert_cmd::cargo::cargo_bin("moldbox"))
            .arg("backup")
            .arg(&db)
            .arg(&dest)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Wait until the temporary file exists: the backup is now blocked on
        // the exclusive lock (5 s busy timeout).
        let start = Instant::now();
        while !fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(".out.db.tmp-"))
        {
            assert!(start.elapsed() < Duration::from_secs(4), "no temp file");
            std::thread::sleep(Duration::from_millis(5));
        }
        let status = Process::new("kill")
            .arg(signal)
            .arg(child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.signal(), Some(number), "{out:?}");
        assert!(out.stdout.is_empty());
        let mut entries: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries, ["locked.db", "locked.db-journal"], "{out:?}");
    }
    holder.execute_batch("ROLLBACK").unwrap();
}

fn normalise(text: &[u8], root: &Path) -> String {
    let mut s = String::from_utf8_lossy(text).replace(&root.display().to_string(), "<ROOT>");
    for marker in ["\"elapsed_ms\": ", "elapsed:            "] {
        let mut out = String::new();
        let mut rest = s.as_str();
        while let Some(i) = rest.find(marker) {
            out.push_str(&rest[..i + marker.len()]);
            rest = &rest[i + marker.len()..];
            let end = rest
                .find(|c: char| {
                    !(c.is_ascii_digit() || c == '.' || c == 'e' || c == '-' || c == '+')
                })
                .unwrap_or(rest.len());
            out.push('X');
            rest = &rest[end..];
        }
        out.push_str(rest);
        s = out;
    }
    // Temporary names are random; the SQLite library version stamped into the
    // backup header (and so the digest) depends on the linked SQLite.
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find(".tmp-") {
        out.push_str(&rest[..i + 5]);
        out.push_str("XXXXXXXX");
        rest = &rest[(i + 13).min(rest.len())..];
    }
    out.push_str(rest);
    // Replace every 64-character lower-case hex run (SHA-256 digests).
    let mut s = String::with_capacity(out.len());
    let mut run = String::new();
    for c in out.chars().chain(std::iter::once('\0')) {
        if c.is_ascii_digit() || ('a'..='f').contains(&c) {
            run.push(c);
            continue;
        }
        if run.len() == 64 {
            s.push_str(&"H".repeat(64));
        } else {
            s.push_str(&run);
        }
        run.clear();
        if c != '\0' {
            s.push(c);
        }
    }
    // The reference is released 0.3.0 and the port is the crate version; the
    // program name is `moldbox` on both sides (the reference is renamed at run
    // time) but the former name is mapped too, so either spelling compares
    // equal.
    s.replace("sqlite-checkpoint", "PROG")
        .replace("moldbox", "PROG")
        .replace("PROG 0.3.0", "PROG VERSION")
        .replace(
            &format!("PROG {}", env!("CARGO_PKG_VERSION")),
            "PROG VERSION",
        )
}

fn fixture(root: &Path) {
    fs::create_dir_all(root).unwrap();
    wal_db(root, "t.db");
    Connection::open(root.join("plain.db"))
        .unwrap()
        .execute_batch("CREATE TABLE x(a)")
        .unwrap();
    fs::write(root.join("garbage.db"), b"not a database ".repeat(100)).unwrap();
    fs::write(root.join("exists.db"), b"x").unwrap();
    fs::create_dir(root.join("dir")).unwrap();
    std::os::unix::fs::symlink("t.db", root.join("link.db")).unwrap();
}

/// Run the Python reference module `sqlite_checkpoint` exactly as
/// `python3 -m sqlite_checkpoint` would, but with its argparse program name
/// changed from `sqlite-checkpoint` to `moldbox`, so argparse itself lays out
/// help and usage text for the new name.
const PY_REFERENCE_AS_MOLDBOX: &str = "\
import argparse, runpy, sys
_init = argparse.ArgumentParser.__init__
def _renamed(self, *args, **kwargs):
    if kwargs.get('prog') == 'sqlite-checkpoint':
        kwargs['prog'] = 'moldbox'
    _init(self, *args, **kwargs)
argparse.ArgumentParser.__init__ = _renamed
runpy.run_module('sqlite_checkpoint', run_name='__main__', alter_sys=True)
";

#[test]
fn differential_against_python_reference() {
    let Some(py_src) = std::env::var_os("SQLITE_CHECKPOINT_PY_SRC") else {
        eprintln!("SQLITE_CHECKPOINT_PY_SRC not set; skipping differential test");
        return;
    };
    let cases: &[&[&str]] = &[
        &["--help"],
        &["snapshot", "-h"],
        &["--version"],
        &["bogus"],
        &["backup", "t.db"],
        &["backup", "t.db", "b.db", "--p", "3"],
        &["checkpoint", "-mx", "t.db"],
        &["info", "t.db", "--json"],
        &["info", "t.db"],
        &["--json", "info", "plain.db"],
        &["info", "garbage.db"],
        &["info", "link.db"],
        &["info", "dir"],
        &["info", "missing.db"],
        &["info", "t.db/x"],
        &["checkpoint", "t.db"],
        &[
            "--json",
            "cp",
            "t.db",
            "-m",
            "truncate",
            "--require-complete",
        ],
        &["backup", "t.db", "b.db"],
        &[
            "--json",
            "bk",
            "t.db",
            "b.db",
            "--pages-per-step",
            "1",
            "--pause-ms",
            "2.5",
        ],
        &["backup", "t.db", "exists.db"],
        &["backup", "t.db", "nodir/b.db"],
        &["backup", "t.db", "b.db", "--pages-per-step", "0"],
        &["backup", "t.db", "b.db", "--pause-ms", "nan"],
        &["snapshot", "t.db", "s.db"],
        &["--json", "snap", "t.db", "s.db", "--require-complete"],
        &["snapshot", "garbage.db", "s.db"],
    ];
    let work = TempDir::new().unwrap();
    let rust_bin = assert_cmd::cargo::cargo_bin("moldbox");
    for (n, case) in cases.iter().enumerate() {
        let py_root = work.path().join(format!("py{n}"));
        let rs_root = work.path().join(format!("rs{n}"));
        fixture(&py_root);
        fixture(&rs_root);
        let py = Process::new("python3")
            .args(["-c", PY_REFERENCE_AS_MOLDBOX])
            .args(*case)
            .current_dir(&py_root)
            .env("PYTHONPATH", &py_src)
            .output()
            .expect("python3");
        let rs = Process::new(&rust_bin)
            .args(*case)
            .current_dir(&rs_root)
            .output()
            .expect("rust binary");
        assert_eq!(py.status.code(), rs.status.code(), "exit code for {case:?}");
        assert_eq!(
            normalise(&py.stdout, &py_root),
            normalise(&rs.stdout, &rs_root),
            "stdout for {case:?}"
        );
        assert_eq!(
            normalise(&py.stderr, &py_root),
            normalise(&rs.stderr, &rs_root),
            "stderr for {case:?}"
        );
        let listing = |root: &Path| {
            let mut entries: Vec<(String, u32)> = fs::read_dir(root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| {
                    let meta = fs::symlink_metadata(e.path()).unwrap();
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        meta.permissions().mode(),
                    )
                })
                .collect();
            entries.sort();
            entries
        };
        assert_eq!(listing(&py_root), listing(&rs_root), "files for {case:?}");
    }
}

const ALIAS_NOTE: &str =
    "sqlite-checkpoint: renamed to moldbox; this alias will be removed in the next release\n";

#[test]
fn deprecated_alias_prints_note_then_behaves_like_moldbox() {
    let dir = TempDir::new().unwrap();
    wal_db(dir.path(), "t.db");
    let cases: &[&[&str]] = &[
        &["--version"],
        &["--help"],
        &[],
        &["bogus"],
        &["info", "t.db"],
        &["--json", "info", "t.db"],
        &["info", "missing.db"],
        &["checkpoint", "-mx", "t.db"],
        &["backup", "t.db", "t.db"],
    ];
    for case in cases {
        let (alias_code, alias_stdout, alias_stderr) = stdout_of(
            Command::cargo_bin("sqlite-checkpoint")
                .expect("alias binary")
                .args(*case)
                .current_dir(dir.path()),
        );
        let (code, stdout, stderr) = stdout_of(bin().args(*case).current_dir(dir.path()));
        assert_eq!(alias_code, code, "exit code for {case:?}");
        assert_eq!(alias_stdout, stdout, "stdout for {case:?}");
        let rest = alias_stderr
            .strip_prefix(ALIAS_NOTE)
            .unwrap_or_else(|| panic!("no deprecation note for {case:?}: {alias_stderr}"));
        assert_eq!(rest, stderr, "stderr for {case:?}");
        assert!(!stderr.contains("renamed to moldbox"), "{case:?}");
    }
}

#[test]
fn deprecated_alias_backup_publishes_like_moldbox() {
    let dir = TempDir::new().unwrap();
    let db = wal_db(dir.path(), "t.db");
    let dest = dir.path().join("alias.db");
    let out = Command::cargo_bin("sqlite-checkpoint")
        .expect("alias binary")
        .arg("backup")
        .arg(&db)
        .arg(&dest)
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stderr), ALIAS_NOTE);
    let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
