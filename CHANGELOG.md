# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

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
