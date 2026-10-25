//! Deprecated `sqlite-checkpoint` alias for `moldbox`.
//!
//! The tool was renamed from sqlite-checkpoint to moldbox in 0.4.0. This alias
//! prints a one-line deprecation note to stderr and then behaves exactly like
//! `moldbox` (same arguments, output and exit codes). It will be removed in
//! a future release.

use std::process::ExitCode;

/// The deprecation note printed to stderr before every invocation.
const DEPRECATION_NOTE: &str =
    "sqlite-checkpoint: renamed to moldbox; this alias will be removed in a future release";

fn main() -> ExitCode {
    eprintln!("{DEPRECATION_NOTE}");
    moldbox::cli::main()
}
