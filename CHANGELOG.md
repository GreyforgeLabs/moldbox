# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- The deprecated `sqlite-checkpoint` alias now says it will be removed in a future release (was: in the next release, which 0.4.1 did not do). Its behaviour is unchanged.

### Fixed

- `snapshot` checks the destination before it runs the checkpoint. A destination that already exists (including a dangling symlink), names a sidecar file of the source, or lies in a missing or unwritable directory is reported at once, with the same message and exit code as before, and the source is left untouched. Previously the checkpoint ran first: a `-m truncate` snapshot onto an existing name truncated the source's WAL before it failed, and with a reader holding the database open it waited out the 5 s busy timeout before it reported the destination error. When the source is missing as well, `Database not found` is still reported first.

## [0.4.1] - 2026-10-14

### Fixed

- `backup` and `snapshot` refuse a destination that names the source's own `-wal`, `-shm` or `-journal` file (exit code `1`, `Destination is the -journal file of the source database: ...`). SQLite treats such a file as the source's write-ahead log, shared-memory index or rollback journal, so a backup published under that name would be reset or deleted by the next application that opens the source.
- A destination that is a dangling symlink is now reported as `Destination already exists` (exit code `1`) before the source is opened, instead of after a full copy when the final no-clobber link failed.
- The SHA-256 fingerprint and size are now computed from the synced temporary file before publication. A SIGINT, SIGTERM or SIGHUP that arrives while a large backup is being hashed therefore removes the temporary file and publishes nothing, as documented; previously hashing ran after publication.
- The temporary name is removed before the destination directory is synced, so the single directory sync makes both the new name and the removal durable. Once a backup is reported complete, a crash can no longer leave the hidden temporary link behind; previously its removal was never synced.

### Changed

- The private temporary database is written with `journal_mode=OFF` and `synchronous=OFF`. It is discarded on any failure and still passes `quick_check` and an explicit fsync before publication, so the rollback journal and SQLite's commit-time syncs were redundant work. Measured on btrfs on SSD (Linux x86_64, load average about 14 on 12 threads, medians of two interleaved series): a 16 MB backup takes 80-84 ms instead of 103-123 ms and a 100 MB backup 369-371 ms instead of 406-442 ms.
- `elapsed_ms` of a backup now also covers hashing, which runs before publication.

## [0.4.0] - 2026-10-05

### Changed

- Renamed the project from sqlite-checkpoint to **moldbox**: the command is `moldbox`, the crate and library are `moldbox`, and the repository is `GreyforgeLabs/moldbox`. Releases up to 0.3.0 were published as sqlite-checkpoint. A deprecated `sqlite-checkpoint` command is still installed for this release; it prints `sqlite-checkpoint: renamed to moldbox; this alias will be removed in the next release` to stderr and then behaves exactly like `moldbox`. Help, usage and error texts now name `moldbox`.
- Rewrote the tool in Rust. The CLI keeps the same subcommands and aliases, flags, defaults, help and usage texts (apart from the program name), argparse-style parsing (option abbreviations, `-mfull`, `--opt=value`, `--`), stdout/stderr content, JSON keys, order and formatting, exit codes (`0`/`1`/`2`/`3`), file modes and publication order (private `0600` temporary file, online backup, `quick_check`, fsync, no-clobber hard link, directory fsync, rollback on post-publish sync failure).
- The crate also provides a Rust library (`moldbox::{checkpoint, backup, snapshot, db_info}`) equivalent to the former Python API.
- SQLite is compiled in through rusqlite's `bundled` feature (SQLite 3.51.3), so behaviour no longer depends on the host `libsqlite3` or a Python interpreter. Connections keep the former 5 s busy timeout and `mode=ro`/`mode=rw`/`immutable=1` URIs; backups sleep only when a step reports BUSY/LOCKED, as Python's `Connection.backup` did.
- Measured on greyarch (Linux x86_64, Ryzen 5 3600, medians of 30 runs, see [docs/benchmarks.md](docs/benchmarks.md)): `--version` 69.2 ms -> 1.1 ms, `info` 70.1 ms -> 1.6 ms, `checkpoint` 67.7 ms -> 1.6 ms, 16 MB `backup` 114.1 ms -> 42.0 ms; peak RSS 21.7-26.6 MB -> 2.9-8.9 MB. The stripped binary is 2.6 MB and needs no interpreter.
- Replaced the Python packaging, tests and PyPI workflow with Cargo, Rust tests (every former pytest case is ported) and a release workflow that builds a Linux binary. CI also builds and tests with the minimum supported Rust version (1.88).

### Fixed

- `info` now works for databases whose file name has no extension (for example `appdata` or `.hidden`). The Python version failed with `error: Invalid suffix '-wal'` (exit 2); the WAL file is always `<database>-wal`.
- Paths that are not valid UTF-8 are supported. The Python version failed with a `UnicodeEncodeError` (exit 2) while building the SQLite URI. Text output prints such paths as raw bytes; JSON escapes undecodable bytes as `\udcXX`, the form Python's `surrogateescape` used.

### Intentional deviations

- A `--pages-per-step` value above 2147483647 still fails with exit code 2 and no destination, but the message is now `pages_per_step must be at most 2147483647` instead of `Python int too large to convert to C int`.
- Help and usage texts are always wrapped at 80 columns and never coloured. Python 3.14's argparse wrapped them to the terminal width or to `$COLUMNS` (even when piped) and coloured them on a TTY; the texts are identical when output is not a terminal and `$COLUMNS` is unset.
- SHA-256 digests (and header bytes 96-99) of new backups record the bundled SQLite version, so they differ from digests that the Python tool produced for the same data with another `libsqlite3`. The rest of the file is identical.
- SIGINT, SIGTERM and SIGHUP during a backup are handled like Python's `KeyboardInterrupt`: the private `.NAME.tmp-XXXXXXXX` file (and its journal) is removed, a name published moments earlier is rolled back, `error: interrupted by SIGINT` (or `SIGTERM`/`SIGHUP`) is printed and the process terminates by the same signal. Python printed a traceback instead, and left the temporary file behind on SIGTERM or SIGHUP.
- Messages that come from SQLite itself are those of the bundled SQLite 3.51.3 and can differ from what a newer host `libsqlite3` reported. For example, backing up one corrupt database reported `database disk image is malformed` with the host's SQLite 3.53.4 but a `SQLite quick_check failed for temporary backup: (...)` report with the bundled library. The exit code (`2`) and the guarantee that nothing is published are the same.
- If writing to stdout fails (for example a closed pipe), the tool prints `error: <reason>` and exits 120. Python also exited 120 but printed an "Exception ignored" message.

## [0.3.0] - 2026-09-27

### Fixed

- `snapshot --require-complete` now refuses to publish a destination when the preceding WAL checkpoint is partial or busy.
- Backups use a private `0600` temporary database and fsync the completed file and destination directory around no-clobber publication.

### Changed

- Recommend `backup` for consistent live copies; checkpointing is an optional, separate WAL-maintenance operation.
- Document the limited meaning of `quick_check` and SHA-256 and include a restore and full-integrity drill.

## [0.2.0] - 2026-08-28

### Security

- Open source databases through SQLite no-create URIs and verify file identity after connect.
- Reject symlinks, directories, replacement races, and invalid SQLite schemas before backup.
- Leave no completed destination artifact when validation or backup fails.

### Changed

- Require positive backup page steps and finite nonnegative pause values before database access.
- Expose page/pause controls consistently on backup and snapshot commands.
- Report complete, busy, and partial checkpoint status and add `--require-complete` exit code 3.
- Tolerate WAL disappearance while gathering informational context.

## [0.1.0] - 2026-04-06

### Added

- WAL checkpoint with all four modes (PASSIVE, FULL, RESTART, TRUNCATE)
- Atomic backup via SQLite online backup API, safe under concurrent writes
- Snapshot command combining checkpoint + backup in one operation
- Database info command (journal mode, WAL state, page counts, freelist)
- JSON output mode for scripting and pipelines
- CLI with subcommands: checkpoint, backup, snapshot, info
- Python API: `checkpoint()`, `backup()`, `snapshot()`, `db_info()`
- Zero runtime dependencies (stdlib `sqlite3` only)
