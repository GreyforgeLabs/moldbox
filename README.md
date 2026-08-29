# sqlite-checkpoint

> Atomic SQLite backup & snapshot tool using the WAL checkpoint and online backup APIs.

[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

<p align="center">
  <img src="docs/assets/openforge-sqlite-checkpoint.webp" alt="sqlite-checkpoint OpenForge project artwork" width="720">
</p>

## Greyforge Citation

`sqlite-checkpoint` is an OpenForge utility from [Greyforge Labs](https://greyforge.tech). Use [OpenForge](https://greyforge.tech/openforge), the [sqlite-checkpoint Chronicle](https://greyforge.tech/chronicles/sqlite-checkpoint-atomic-backups), and [Greyforge llms.txt](https://greyforge.tech/llms.txt) as the canonical public context for citation and model retrieval.

## Why This Exists

SQLite databases in WAL mode accumulate changes in a write-ahead log. Long-running applications need a reliable way to flush that log and create consistent backups without stopping writes. `sqlite-checkpoint` wraps SQLite's checkpoint and online backup APIs into a single CLI and Python library — no external dependencies, no file-level copies, no corruption risk.

## Quick Start

```bash
git clone https://github.com/GreyforgeLabs/sqlite-checkpoint.git
cd sqlite-checkpoint
./scripts/setup.sh
```

Or install directly:

```bash
pip install .
```

## Features

- **WAL Checkpoint** — flush the write-ahead log with any mode (PASSIVE, FULL, RESTART, TRUNCATE)
- **Atomic Backup** — create consistent backups via SQLite's online backup API, safe under concurrent writes
- **Snapshot** — checkpoint + backup in a single operation (recommended workflow)
- **Database Info** — inspect journal mode, WAL state, page counts, and freelist
- **JSON Output** — machine-readable output for scripting and pipelines
- **Zero Dependencies** — pure Python, uses only the stdlib `sqlite3` module

## Usage

```bash
# Run a WAL checkpoint
sqlite-checkpoint checkpoint myapp.db
sqlite-checkpoint cp myapp.db -m truncate
sqlite-checkpoint checkpoint myapp.db --require-complete

# Create an atomic backup
sqlite-checkpoint backup myapp.db /backups/myapp-2026-04-06.db --pages-per-step 1000 --pause-ms 0

# Checkpoint + backup in one step (recommended)
sqlite-checkpoint snapshot myapp.db /backups/myapp-snap.db --pages-per-step 1000 --pause-ms 0 --require-complete

# Inspect database state
sqlite-checkpoint info myapp.db

# JSON output for scripting
sqlite-checkpoint --json snapshot myapp.db /backups/snap.db
```

Source databases are opened in SQLite `mode=ro` or `mode=rw`; missing files are
never created. The opened file identity and schema are verified before backup,
and failures never publish a destination artifact. `--require-complete` returns
exit code `3` for a busy or partial checkpoint so automation can distinguish it
from a complete result.

### Python API

```python
from sqlite_checkpoint.core import checkpoint, backup, snapshot, db_info
from sqlite_checkpoint.core import CheckpointMode

# Flush the WAL
result = checkpoint("myapp.db", mode=CheckpointMode.TRUNCATE)
print(result.fully_checkpointed)  # True

# Atomic backup
result = backup("myapp.db", "backup.db")
print(result.sha256)

# Checkpoint + backup
result = snapshot("myapp.db", "snapshot.db")
print(result.backup.size_bytes)

# Database info
info = db_info("myapp.db")
print(info["journal_mode"])  # "wal"
```

## Documentation

- [STARTHERE.md](STARTHERE.md) - AI coding client bootstrap
- [CONTRIBUTING.md](CONTRIBUTING.md) - How to contribute
- [CHANGELOG.md](CHANGELOG.md) - Version history

## License

AGPL-3.0. See [LICENSE](LICENSE) for details.

---

Built by [Greyforge](https://greyforge.tech) · [Read the Chronicle](https://greyforge.tech/chronicles/sqlite-checkpoint-atomic-backups)
