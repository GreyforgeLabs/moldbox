//! Command-line interface: argument parsing, dispatch and output formatting.
//!
//! Output is byte-compatible with the original Python CLI: the same text
//! layout, `json.dumps` formatting (`", "`/`": "` separators, ASCII-only
//! escapes), exit codes and `error: ...` messages on stderr.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use crate::argparse::{self, Exit, Kind, Namespace, Opt, Pos, Spec, Ty, Value};
use crate::help;
use crate::pyfmt::{float_repr, json_str, round2, thousands};
use crate::{
    BackupOptions, BackupResult, CheckpointMode, CheckpointResult, DbInfo, Error, SnapshotOptions,
    SnapshotResult,
};

const PROG: &str = "moldbox";
const VERSION_LINE: &str = concat!("moldbox ", env!("CARGO_PKG_VERSION"));
const COMMANDS: &[&str] = &[
    "checkpoint",
    "cp",
    "backup",
    "bk",
    "snapshot",
    "snap",
    "info",
];
const MODES: &[&str] = &["passive", "full", "restart", "truncate"];

const HELP_OPT: Opt = Opt {
    strings: &["-h", "--help"],
    dest: "help",
    kind: Kind::Help,
};
const MODE_OPT: Opt = Opt {
    strings: &["-m", "--mode"],
    dest: "mode",
    kind: Kind::Value {
        ty: Ty::Str,
        choices: Some(MODES),
    },
};
const REQUIRE_COMPLETE_OPT: Opt = Opt {
    strings: &["--require-complete"],
    dest: "require_complete",
    kind: Kind::Flag,
};
const PAGES_OPT: Opt = Opt {
    strings: &["--pages-per-step"],
    dest: "pages_per_step",
    kind: Kind::Value {
        ty: Ty::Int,
        choices: None,
    },
};
const PAUSE_OPT: Opt = Opt {
    strings: &["--pause-ms"],
    dest: "pause_ms",
    kind: Kind::Value {
        ty: Ty::Float,
        choices: None,
    },
};
const DATABASE_POS: Pos = Pos {
    dest: "database",
    parser: None,
};
const DESTINATION_POS: Pos = Pos {
    dest: "destination",
    parser: None,
};

static TOP: Spec = Spec {
    prog: PROG,
    usage: help::TOP_USAGE,
    help: help::TOP_HELP,
    version: VERSION_LINE,
    opts: &[
        HELP_OPT,
        Opt {
            strings: &["--version"],
            dest: "version",
            kind: Kind::Version,
        },
        Opt {
            strings: &["--json"],
            dest: "json",
            kind: Kind::Flag,
        },
    ],
    pos: &[Pos {
        dest: "command",
        parser: Some(COMMANDS),
    }],
};

static CHECKPOINT: Spec = Spec {
    prog: "moldbox checkpoint",
    usage: help::CHECKPOINT_USAGE,
    help: help::CHECKPOINT_HELP,
    version: VERSION_LINE,
    opts: &[HELP_OPT, MODE_OPT, REQUIRE_COMPLETE_OPT],
    pos: &[DATABASE_POS],
};

static BACKUP: Spec = Spec {
    prog: "moldbox backup",
    usage: help::BACKUP_USAGE,
    help: help::BACKUP_HELP,
    version: VERSION_LINE,
    opts: &[HELP_OPT, PAGES_OPT, PAUSE_OPT],
    pos: &[DATABASE_POS, DESTINATION_POS],
};

static SNAPSHOT: Spec = Spec {
    prog: "moldbox snapshot",
    usage: help::SNAPSHOT_USAGE,
    help: help::SNAPSHOT_HELP,
    version: VERSION_LINE,
    opts: &[
        HELP_OPT,
        MODE_OPT,
        PAGES_OPT,
        PAUSE_OPT,
        REQUIRE_COMPLETE_OPT,
    ],
    pos: &[DATABASE_POS, DESTINATION_POS],
};

static INFO: Spec = Spec {
    prog: "moldbox info",
    usage: help::INFO_USAGE,
    help: help::INFO_HELP,
    version: VERSION_LINE,
    opts: &[HELP_OPT],
    pos: &[DATABASE_POS],
};

/// A fully parsed command line.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Checkpoint {
        database: PathBuf,
        mode: CheckpointMode,
        require_complete: bool,
    },
    Backup {
        database: PathBuf,
        destination: PathBuf,
        pages_per_step: i64,
        pause_ms: f64,
    },
    Snapshot {
        database: PathBuf,
        destination: PathBuf,
        mode: CheckpointMode,
        pages_per_step: i64,
        pause_ms: f64,
        require_complete: bool,
    },
    Info {
        database: PathBuf,
    },
}

/// Parsed invocation: global `--json` plus the sub-command.
#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub json: bool,
    pub command: Command,
}

fn sub_spec(name: &[u8]) -> &'static Spec {
    match name {
        b"checkpoint" | b"cp" => &CHECKPOINT,
        b"backup" | b"bk" => &BACKUP,
        b"snapshot" | b"snap" => &SNAPSHOT,
        _ => &INFO,
    }
}

fn path_of(ns: &Namespace, dest: &str) -> PathBuf {
    PathBuf::from(OsString::from_vec(ns.bytes(dest).to_vec()))
}

fn mode_of(ns: &Namespace) -> CheckpointMode {
    let name = String::from_utf8_lossy(ns.bytes("mode")).into_owned();
    CheckpointMode::from_name(&name).unwrap_or(CheckpointMode::Passive)
}

fn pages_of(ns: &Namespace) -> i64 {
    match ns.get("pages_per_step") {
        Some(Value::Int(v)) => {
            i64::try_from(*v).unwrap_or(if *v < 0 { i64::MIN } else { i64::MAX })
        }
        _ => 1000,
    }
}

fn pause_of(ns: &Namespace) -> f64 {
    match ns.get("pause_ms") {
        Some(Value::Float(v)) => *v,
        _ => 0.0,
    }
}

fn sub_defaults() -> Namespace {
    let mut ns = Namespace::default();
    ns.set("mode", Value::Bytes(b"passive".to_vec()));
    ns.set("require_complete", Value::Bool(false));
    ns.set("pages_per_step", Value::Int(1000));
    ns.set("pause_ms", Value::Float(0.0));
    ns
}

/// Parse the command line (without the program name).
pub fn parse_args(args: &[OsString]) -> Result<Invocation, Exit> {
    let raw: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
    let mut sub = |name: &[u8], rest: &[Vec<u8>]| {
        argparse::parse_known(sub_spec(name), rest, sub_defaults(), &mut |_, _| {
            unreachable!("sub-commands have no nested parsers")
        })
    };
    let mut defaults = Namespace::default();
    defaults.set("json", Value::Bool(false));
    let ns = argparse::parse(&TOP, &raw, defaults, &mut sub)?;
    let command = match ns.bytes("command") {
        b"checkpoint" | b"cp" => Command::Checkpoint {
            database: path_of(&ns, "database"),
            mode: mode_of(&ns),
            require_complete: ns.flag("require_complete"),
        },
        b"backup" | b"bk" => Command::Backup {
            database: path_of(&ns, "database"),
            destination: path_of(&ns, "destination"),
            pages_per_step: pages_of(&ns),
            pause_ms: pause_of(&ns),
        },
        b"snapshot" | b"snap" => Command::Snapshot {
            database: path_of(&ns, "database"),
            destination: path_of(&ns, "destination"),
            mode: mode_of(&ns),
            pages_per_step: pages_of(&ns),
            pause_ms: pause_of(&ns),
            require_complete: ns.flag("require_complete"),
        },
        _ => Command::Info {
            database: path_of(&ns, "database"),
        },
    };
    Ok(Invocation {
        json: ns.flag("json"),
        command,
    })
}

/// Record SIGINT, SIGTERM and SIGHUP instead of dying immediately, so a
/// backup in progress can remove its temporary file (and roll back a name
/// it just published) before the process exits. The binary re-raises the
/// signal afterwards; see [`pending_signal`].
pub fn install_signal_handlers() -> std::io::Result<()> {
    let flag = crate::PENDING_SIGNAL.get_or_init(|| Arc::new(AtomicUsize::new(0)));
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let value = usize::try_from(signal).unwrap_or(usize::MAX);
        signal_hook::flag::register_usize(signal, Arc::clone(flag), value)?;
    }
    Ok(())
}

/// The termination signal recorded by [`install_signal_handlers`], if any.
pub fn pending_signal() -> Option<i32> {
    crate::pending_signal()
}

/// Captured process output.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run the CLI with the given arguments (without the program name).
pub fn run(args: &[OsString]) -> Output {
    match parse_args(args) {
        Ok(invocation) => execute(&invocation),
        Err(exit) => Output {
            code: exit.code,
            stdout: exit.stdout.into_bytes(),
            stderr: exit.stderr.into_bytes(),
        },
    }
}

/// The process entry point shared by the `moldbox` binary and its deprecated
/// `sqlite-checkpoint` alias: run the CLI on the process arguments, write the
/// captured output and return the exit status.
pub fn main() -> std::process::ExitCode {
    use std::io::{self, Write};
    use std::process::ExitCode;

    // Without the handlers the signals keep their default action, which is
    // still safe: nothing is ever published before it is complete.
    let _ = install_signal_handlers();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let output = run(&args);
    if let Some(signal) = pending_signal() {
        // Interrupted: cleanup already ran. Report any error, then terminate
        // by the same signal (Python died by SIGINT on KeyboardInterrupt).
        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(&output.stderr);
        let _ = stderr.flush();
        drop(stderr);
        let _ = signal_hook::low_level::emulate_default_handler(signal);
        return ExitCode::from(u8::try_from(128 + signal).unwrap_or(1));
    }
    let mut code = output.code;
    let written = {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(&output.stdout)
            .and_then(|()| stdout.flush())
    };
    if let Err(err) = written {
        // Python exits 120 when flushing stdout fails at interpreter shutdown.
        let _ = writeln!(io::stderr(), "error: {err}");
        code = 120;
    }
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(&output.stderr);
    let _ = stderr.flush();
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Execute a parsed invocation.
pub fn execute(invocation: &Invocation) -> Output {
    let json = invocation.json;
    let mut out = Output::default();
    let result: Result<i32, Error> = match &invocation.command {
        Command::Checkpoint {
            database,
            mode,
            require_complete,
        } => crate::checkpoint(database, *mode).map(|result| {
            let done = checkpoint_output(&result, json, *require_complete);
            out.stdout = done.stdout;
            done.code
        }),
        Command::Backup {
            database,
            destination,
            pages_per_step,
            pause_ms,
        } => crate::backup(
            database,
            destination,
            &BackupOptions {
                pages_per_step: *pages_per_step,
                pause_ms: *pause_ms,
            },
        )
        .map(|result| {
            out.stdout = format_backup(&result, json);
            0
        }),
        Command::Snapshot {
            database,
            destination,
            mode,
            pages_per_step,
            pause_ms,
            require_complete,
        } => crate::snapshot(
            database,
            destination,
            &SnapshotOptions {
                checkpoint_mode: *mode,
                pages_per_step: *pages_per_step,
                pause_ms: *pause_ms,
                require_complete: *require_complete,
            },
        )
        .map(|result| {
            out.stdout = format_snapshot(&result, json);
            0
        }),
        Command::Info { database } => crate::db_info(database).map(|info| {
            out.stdout = format_info(&info, json);
            0
        }),
    };
    match result {
        Ok(code) => out.code = code,
        Err(err) => report_error(&err, json, &mut out),
    }
    out
}

/// Output of a completed checkpoint: exit code 3 when `--require-complete`
/// was given and the checkpoint was busy or partial.
pub fn checkpoint_output(result: &CheckpointResult, json: bool, require_complete: bool) -> Output {
    Output {
        code: if require_complete && !result.fully_checkpointed() {
            3
        } else {
            0
        },
        stdout: format_checkpoint(result, json),
        stderr: Vec::new(),
    }
}

/// Map a library error to CLI output (`error: ...` on stderr, or the
/// incomplete-snapshot report on stdout).
pub fn report_error(err: &Error, json: bool, out: &mut Output) {
    out.code = err.exit_code();
    match err {
        Error::IncompleteCheckpoint(result) => {
            out.stdout = format_incomplete_snapshot(result, json);
        }
        other => {
            out.stderr = format!("error: {other}\n").into_bytes();
        }
    }
}

fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn json_bool(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn line(buf: &mut Vec<u8>, label: &str, value: &[u8]) {
    buf.extend_from_slice(format!("  {label:<20}").as_bytes());
    buf.extend_from_slice(value);
    buf.push(b'\n');
}

/// Output of `checkpoint` (text or JSON).
pub fn format_checkpoint(r: &CheckpointResult, json: bool) -> Vec<u8> {
    if json {
        return format!(
            "{{\"command\": \"checkpoint\", \"mode\": \"{}\", \"status\": \"{}\", \"busy\": {}, \
             \"wal_pages\": {}, \"checkpointed_pages\": {}, \"fully_checkpointed\": {}, \
             \"database\": {}, \"elapsed_ms\": {}}}\n",
            r.mode.as_str(),
            r.status().as_str(),
            json_bool(r.busy),
            r.wal_pages,
            r.checkpointed_pages,
            json_bool(r.fully_checkpointed()),
            json_str(path_bytes(&r.database)),
            float_repr(r.elapsed_ms),
        )
        .into_bytes();
    }
    let mut buf =
        format!("checkpoint {} [{}]\n", r.mode.as_str(), r.status().as_str()).into_bytes();
    line(&mut buf, "busy:", yes_no(r.busy).as_bytes());
    line(&mut buf, "wal pages:", r.wal_pages.to_string().as_bytes());
    line(
        &mut buf,
        "checkpointed:",
        r.checkpointed_pages.to_string().as_bytes(),
    );
    line(
        &mut buf,
        "elapsed:",
        format!("{} ms", float_repr(r.elapsed_ms)).as_bytes(),
    );
    buf
}

/// Output of `backup` (text or JSON).
pub fn format_backup(r: &BackupResult, json: bool) -> Vec<u8> {
    if json {
        return format!(
            "{{\"command\": \"backup\", \"source\": {}, \"destination\": {}, \"size_bytes\": {}, \
             \"sha256\": {}, \"elapsed_ms\": {}}}\n",
            json_str(path_bytes(&r.source)),
            json_str(path_bytes(&r.destination)),
            r.size_bytes,
            json_str(r.sha256.as_bytes()),
            float_repr(r.elapsed_ms),
        )
        .into_bytes();
    }
    let mut buf = b"backup complete\n".to_vec();
    line(&mut buf, "source:", path_bytes(&r.source));
    line(&mut buf, "destination:", path_bytes(&r.destination));
    line(
        &mut buf,
        "size:",
        format!("{} bytes", thousands(i128::from(r.size_bytes))).as_bytes(),
    );
    line(&mut buf, "sha256:", r.sha256.as_bytes());
    line(
        &mut buf,
        "elapsed:",
        format!("{} ms", float_repr(r.elapsed_ms)).as_bytes(),
    );
    buf
}

/// Output of `snapshot --require-complete` when the checkpoint was busy or
/// partial and nothing was published.
pub fn format_incomplete_snapshot(r: &CheckpointResult, json: bool) -> Vec<u8> {
    if json {
        return format!(
            "{{\"command\": \"snapshot\", \"checkpoint\": {{\"mode\": \"{}\", \"status\": \"{}\", \
             \"busy\": {}, \"wal_pages\": {}, \"checkpointed_pages\": {}, \
             \"fully_checkpointed\": false}}, \"backup\": null, \"published\": false}}\n",
            r.mode.as_str(),
            r.status().as_str(),
            json_bool(r.busy),
            r.wal_pages,
            r.checkpointed_pages,
        )
        .into_bytes();
    }
    format!("snapshot not created: checkpoint {}\n", r.status().as_str()).into_bytes()
}

/// Output of a successful `snapshot` (text or JSON).
pub fn format_snapshot(r: &SnapshotResult, json: bool) -> Vec<u8> {
    let cp = &r.checkpoint;
    let bk = &r.backup;
    let total = float_repr(round2(cp.elapsed_ms + bk.elapsed_ms));
    if json {
        return format!(
            "{{\"command\": \"snapshot\", \"checkpoint\": {{\"mode\": \"{}\", \"status\": \"{}\", \
             \"busy\": {}, \"wal_pages\": {}, \"checkpointed_pages\": {}, \
             \"fully_checkpointed\": {}}}, \"backup\": {{\"source\": {}, \"destination\": {}, \
             \"size_bytes\": {}, \"sha256\": {}}}, \"elapsed_ms\": {}}}\n",
            cp.mode.as_str(),
            cp.status().as_str(),
            json_bool(cp.busy),
            cp.wal_pages,
            cp.checkpointed_pages,
            json_bool(cp.fully_checkpointed()),
            json_str(path_bytes(&bk.source)),
            json_str(path_bytes(&bk.destination)),
            bk.size_bytes,
            json_str(bk.sha256.as_bytes()),
            total,
        )
        .into_bytes();
    }
    let mut buf = format!("snapshot complete [checkpoint {}]\n", cp.status().as_str()).into_bytes();
    line(&mut buf, "checkpoint busy:", yes_no(cp.busy).as_bytes());
    line(&mut buf, "wal pages:", cp.wal_pages.to_string().as_bytes());
    line(
        &mut buf,
        "checkpointed:",
        cp.checkpointed_pages.to_string().as_bytes(),
    );
    line(&mut buf, "destination:", path_bytes(&bk.destination));
    line(
        &mut buf,
        "size:",
        format!("{} bytes", thousands(i128::from(bk.size_bytes))).as_bytes(),
    );
    line(&mut buf, "sha256:", bk.sha256.as_bytes());
    line(&mut buf, "elapsed:", format!("{total} ms").as_bytes());
    buf
}

/// Output of `info` (text or JSON).
pub fn format_info(info: &DbInfo, json: bool) -> Vec<u8> {
    if json {
        return format!(
            "{{\"command\": \"info\", \"path\": {}, \"size_bytes\": {}, \"journal_mode\": {}, \
             \"wal_file_exists\": {}, \"wal_size_bytes\": {}, \"page_size\": {}, \
             \"page_count\": {}, \"freelist_count\": {}}}\n",
            json_str(path_bytes(&info.path)),
            info.size_bytes,
            json_str(info.journal_mode.as_bytes()),
            json_bool(info.wal_file_exists),
            info.wal_size_bytes,
            info.page_size,
            info.page_count,
            info.freelist_count,
        )
        .into_bytes();
    }
    let mut buf = b"database info\n".to_vec();
    line(&mut buf, "path:", path_bytes(&info.path));
    line(
        &mut buf,
        "size:",
        format!("{} bytes", thousands(i128::from(info.size_bytes))).as_bytes(),
    );
    line(&mut buf, "journal mode:", info.journal_mode.as_bytes());
    line(
        &mut buf,
        "WAL file:",
        yes_no(info.wal_file_exists).as_bytes(),
    );
    if info.wal_file_exists {
        line(
            &mut buf,
            "WAL size:",
            format!("{} bytes", thousands(i128::from(info.wal_size_bytes))).as_bytes(),
        );
    }
    line(
        &mut buf,
        "page size:",
        thousands(i128::from(info.page_size)).as_bytes(),
    );
    line(
        &mut buf,
        "page count:",
        thousands(i128::from(info.page_count)).as_bytes(),
    );
    line(
        &mut buf,
        "freelist pages:",
        thousands(i128::from(info.freelist_count)).as_bytes(),
    );
    buf
}

#[cfg(test)]
mod tests;
