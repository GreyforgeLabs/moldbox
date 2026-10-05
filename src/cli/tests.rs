//! CLI tests ported from the original `tests/test_cli.py` (in-process, like
//! the Python `main([...])` calls), plus argparse-parity checks.

use super::*;
use rusqlite::Connection;
use std::ffi::OsString;
use tempfile::TempDir;

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
    (dir, path)
}

fn args<I, S>(items: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    items.into_iter().map(Into::into).collect()
}

fn main(items: Vec<OsString>) -> Output {
    run(&items)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

fn checkpoint_result(busy: bool, wal: i64, ckpt: i64) -> CheckpointResult {
    CheckpointResult {
        mode: CheckpointMode::Passive,
        busy,
        wal_pages: wal,
        checkpointed_pages: ckpt,
        database: PathBuf::from("db"),
        elapsed_ms: 1.0,
    }
}

// --- TestCLI --------------------------------------------------------------

#[test]
fn test_checkpoint_command() {
    let (_dir, db) = wal_db();
    let out = main(args([OsString::from("checkpoint"), db.into()]));
    assert_eq!(out.code, 0);
    assert!(text(&out.stdout).contains("checkpoint PASSIVE"));
}

#[test]
fn test_backup_command() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("cli_backup.db");
    let out = main(args([
        OsString::from("backup"),
        db.into(),
        dest.clone().into(),
    ]));
    assert_eq!(out.code, 0);
    assert!(dest.exists());
    assert!(text(&out.stdout).contains("sha256"));
}

#[test]
fn test_snapshot_command() {
    let (dir, db) = wal_db();
    let dest = dir.path().join("cli_snap.db");
    let out = main(args([OsString::from("snapshot"), db.into(), dest.into()]));
    assert_eq!(out.code, 0);
    assert!(text(&out.stdout).contains("snapshot complete"));
}

#[test]
fn test_info_command() {
    let (_dir, db) = wal_db();
    let out = main(args([OsString::from("info"), db.into()]));
    assert_eq!(out.code, 0);
    assert!(text(&out.stdout).contains("wal"));
}

#[test]
fn test_json_output() {
    let (_dir, db) = wal_db();
    let out = main(args([OsString::from("--json"), "info".into(), db.into()]));
    assert_eq!(out.code, 0);
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("{\"command\": \"info\", \"path\": "),
        "{stdout}"
    );
    assert!(stdout.contains("\"journal_mode\": \"wal\""), "{stdout}");
    assert!(stdout.ends_with("}\n"));
}

#[test]
fn test_missing_database_returns_error() {
    let dir = TempDir::new().unwrap();
    let out = main(args([
        OsString::from("info"),
        dir.path().join("nope.db").into(),
    ]));
    assert_eq!(out.code, 1);
    assert!(text(&out.stderr).starts_with("error: Database not found: "));
}

#[test]
fn test_alias_cp() {
    let (_dir, db) = wal_db();
    let out = main(args([OsString::from("cp"), db.into()]));
    assert_eq!(out.code, 0);
}

#[test]
fn test_checkpoint_require_complete_returns_distinct_exit() {
    let result = checkpoint_result(true, 10, 4);
    let invocation =
        parse_args(&args(["--json", "checkpoint", "db", "--require-complete"])).unwrap();
    let Command::Checkpoint {
        require_complete, ..
    } = invocation.command
    else {
        panic!("expected checkpoint");
    };

    let out = checkpoint_output(&result, invocation.json, require_complete);

    assert_eq!(out.code, 3);
    let stdout = text(&out.stdout);
    assert!(stdout.contains("\"status\": \"busy\""), "{stdout}");
    assert!(stdout.contains("\"fully_checkpointed\": false"), "{stdout}");
}

#[test]
fn test_snapshot_option_parity_and_require_complete() {
    let dir = TempDir::new().unwrap();
    let snap = dir.path().join("snap.db");
    let invocation = parse_args(&args([
        OsString::from("snapshot"),
        "db".into(),
        snap.clone().into(),
        "--pages-per-step".into(),
        "17".into(),
        "--pause-ms".into(),
        "2.5".into(),
        "--require-complete".into(),
    ]))
    .unwrap();
    assert_eq!(
        invocation.command,
        Command::Snapshot {
            database: PathBuf::from("db"),
            destination: snap.clone(),
            mode: CheckpointMode::Passive,
            pages_per_step: 17,
            pause_ms: 2.5,
            require_complete: true,
        }
    );

    let mut out = Output::default();
    let incomplete = Error::IncompleteCheckpoint(Box::new(checkpoint_result(false, 10, 4)));
    report_error(&incomplete, invocation.json, &mut out);

    assert_eq!(out.code, 3);
    assert!(!snap.exists());
    assert!(text(&out.stdout).contains("snapshot not created: checkpoint partial"));
}

#[test]
fn test_invalid_backup_controls_return_usage_failure_without_output() {
    let (dir, db) = wal_db();
    let destination = dir.path().join("invalid.db");
    let out = main(args([
        OsString::from("backup"),
        db.into(),
        destination.clone().into(),
        "--pages-per-step".into(),
        "0".into(),
    ]));
    assert_eq!(out.code, 2);
    assert!(!destination.exists());
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("pages_per_step"));
}

// --- Ported type-level cases from test_core.py ------------------------------

fn rejected_control(flag: &str, value: &str, expected: &str) {
    let (dir, db) = wal_db();
    let destination = dir.path().join("out.db");
    let out = main(args([
        OsString::from("backup"),
        db.into(),
        destination.clone().into(),
        flag.into(),
        value.into(),
    ]));
    assert_eq!(out.code, 2);
    assert!(out.stdout.is_empty());
    assert!(
        text(&out.stderr).contains(expected),
        "{}",
        text(&out.stderr)
    );
    assert!(!destination.exists());
}

/// `test_backup_rejects_invalid_page_steps_before_output[True]`
#[test]
fn test_backup_rejects_invalid_page_steps_before_output_true() {
    rejected_control(
        "--pages-per-step",
        "True",
        "argument --pages-per-step: invalid int value: 'True'",
    );
}

/// `test_backup_rejects_invalid_page_steps_before_output[1.5]`
#[test]
fn test_backup_rejects_invalid_page_steps_before_output_1_5() {
    rejected_control(
        "--pages-per-step",
        "1.5",
        "argument --pages-per-step: invalid int value: '1.5'",
    );
}

/// `test_backup_rejects_invalid_pause_before_output[True]`
#[test]
fn test_backup_rejects_invalid_pause_before_output_true() {
    rejected_control(
        "--pause-ms",
        "True",
        "argument --pause-ms: invalid float value: 'True'",
    );
}

// --- Output formatting parity ----------------------------------------------

#[test]
fn checkpoint_text_layout() {
    let mut result = checkpoint_result(false, 3, 3);
    result.elapsed_ms = 0.56;
    assert_eq!(
        text(&format_checkpoint(&result, false)),
        "checkpoint PASSIVE [complete]\n  busy:               no\n  wal pages:          3\n  \
         checkpointed:       3\n  elapsed:            0.56 ms\n"
    );
    assert_eq!(
        text(&format_checkpoint(&result, true)),
        "{\"command\": \"checkpoint\", \"mode\": \"PASSIVE\", \"status\": \"complete\", \
         \"busy\": false, \"wal_pages\": 3, \"checkpointed_pages\": 3, \"fully_checkpointed\": \
         true, \"database\": \"db\", \"elapsed_ms\": 0.56}\n"
    );
}

#[test]
fn incomplete_snapshot_json_layout() {
    let result = checkpoint_result(true, 10, 4);
    assert_eq!(
        text(&format_incomplete_snapshot(&result, true)),
        "{\"command\": \"snapshot\", \"checkpoint\": {\"mode\": \"PASSIVE\", \"status\": \
         \"busy\", \"busy\": true, \"wal_pages\": 10, \"checkpointed_pages\": 4, \
         \"fully_checkpointed\": false}, \"backup\": null, \"published\": false}\n"
    );
    assert_eq!(
        text(&format_incomplete_snapshot(&result, false)),
        "snapshot not created: checkpoint busy\n"
    );
}

#[test]
fn snapshot_layout_rounds_total_elapsed() {
    let mut cp = checkpoint_result(false, 0, 0);
    cp.elapsed_ms = 0.1;
    let r = SnapshotResult {
        checkpoint: cp,
        backup: BackupResult {
            source: PathBuf::from("/src.db"),
            destination: PathBuf::from("dst \u{e9}.db"),
            size_bytes: 1_234_567,
            sha256: "ab".repeat(32),
            elapsed_ms: 0.2,
        },
    };
    let t = text(&format_snapshot(&r, false));
    assert!(t.contains("  size:               1,234,567 bytes\n"), "{t}");
    assert!(t.ends_with("  elapsed:            0.3 ms\n"), "{t}");
    let j = text(&format_snapshot(&r, true));
    assert!(j.contains("\"destination\": \"dst \\u00e9.db\""), "{j}");
    assert!(j.ends_with("\"elapsed_ms\": 0.3}\n"), "{j}");
}

#[test]
fn info_layout_hides_wal_size_without_wal() {
    let info = DbInfo {
        path: PathBuf::from("/x.db"),
        size_bytes: 8192,
        journal_mode: "delete".into(),
        wal_file_exists: false,
        wal_size_bytes: 0,
        page_size: 4096,
        page_count: 2,
        freelist_count: 0,
    };
    assert_eq!(
        text(&format_info(&info, false)),
        "database info\n  path:               /x.db\n  size:               8,192 bytes\n  \
         journal mode:       delete\n  WAL file:           no\n  page size:          4,096\n  \
         page count:         2\n  freelist pages:     0\n"
    );
}

// --- argparse parity --------------------------------------------------------

fn usage_error(items: &[&str]) -> (i32, String, String) {
    let out = main(args(items.iter().copied()));
    (out.code, text(&out.stdout), text(&out.stderr))
}

#[test]
fn version_and_help() {
    let (code, stdout, _) = usage_error(&["--version"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, format!("moldbox {}\n", crate::VERSION));
    let (code, stdout, _) = usage_error(&["--help"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, help::TOP_HELP);
    let (code, stdout, _) = usage_error(&["cp", "-h"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, help::CHECKPOINT_HELP);
    // A version flag after the sub-command is not recognised.
    let (code, _, stderr) = usage_error(&["info", "x", "--version"]);
    assert_eq!(code, 2);
    assert!(stderr.ends_with("moldbox: error: unrecognized arguments: --version\n"));
}

#[test]
fn usage_errors_match_argparse() {
    let cases: &[(&[&str], &str)] = &[
        (
            &[],
            "moldbox: error: the following arguments are required: command\n",
        ),
        (
            &["bogus"],
            "moldbox: error: argument command: invalid choice: 'bogus' (choose from \
             'checkpoint', 'cp', 'backup', 'bk', 'snapshot', 'snap', 'info')\n",
        ),
        (
            &["info"],
            "moldbox info: error: the following arguments are required: database\n",
        ),
        (
            &["backup", "a"],
            "moldbox backup: error: the following arguments are required: destination\n",
        ),
        (
            &["checkpoint", "-m", "x", "db"],
            "moldbox checkpoint: error: argument -m/--mode: invalid choice: 'x' \
             (choose from 'passive', 'full', 'restart', 'truncate')\n",
        ),
        (
            &["checkpoint", "--mode"],
            "moldbox checkpoint: error: argument -m/--mode: expected one argument\n",
        ),
        (
            &["backup", "a", "b", "--p", "3"],
            "moldbox backup: error: ambiguous option: --p could match \
             --pages-per-step, --pause-ms\n",
        ),
        (
            &["--json=1", "info", "a"],
            "moldbox: error: argument --json: ignored explicit argument '1'\n",
        ),
        (
            &["--json", "info", "a", "b"],
            "moldbox: error: unrecognized arguments: b\n",
        ),
        (
            &["--bogus", "info", "a", "b"],
            "moldbox: error: unrecognized arguments: --bogus b\n",
        ),
        (
            &["info", "--mode", "full", "x"],
            "moldbox: error: unrecognized arguments: --mode x\n",
        ),
    ];
    for (argv, expected_tail) in cases {
        let (code, stdout, stderr) = usage_error(argv);
        assert_eq!(code, 2, "{argv:?}");
        assert!(stdout.is_empty(), "{argv:?}");
        assert!(stderr.starts_with("usage: moldbox"), "{argv:?}: {stderr}");
        assert!(stderr.ends_with(expected_tail), "{argv:?}: {stderr}");
    }
}

#[test]
fn option_spellings_parse_like_argparse() {
    let parse = |items: &[&str]| parse_args(&args(items.iter().copied())).unwrap();
    for argv in [
        &["checkpoint", "-mfull", "db"][..],
        &["checkpoint", "-m", "full", "db"],
        &["checkpoint", "--mode=full", "db"],
        &["checkpoint", "--mo", "full", "db"],
        &["cp", "db", "--m=full"],
    ] {
        assert_eq!(
            parse(argv).command,
            Command::Checkpoint {
                database: PathBuf::from("db"),
                mode: CheckpointMode::Full,
                require_complete: false,
            },
            "{argv:?}"
        );
    }
    assert!(parse(&["--j", "info", "db"]).json);
    // Negative numbers are positionals/values; '--' ends option parsing.
    assert_eq!(
        parse(&["info", "--", "-h"]).command,
        Command::Info {
            database: PathBuf::from("-h")
        }
    );
    assert_eq!(
        parse(&["info", "-5"]).command,
        Command::Info {
            database: PathBuf::from("-5")
        }
    );
    match parse(&[
        "bk",
        "a",
        "b",
        "--pages-per-step",
        "-3",
        "--pause-ms",
        " 1_0.5 ",
    ])
    .command
    {
        Command::Backup {
            pages_per_step,
            pause_ms,
            ..
        } => {
            assert_eq!(pages_per_step, -3);
            assert_eq!(pause_ms, 10.5);
        }
        other => panic!("{other:?}"),
    }
}
