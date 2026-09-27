# STARTHERE.md - AI Bootstrap Guide

> This file is designed for AI coding assistants. If you are a human,
> see [README.md](README.md) for the human-friendly guide.

## Quick Bootstrap

```bash
git clone https://github.com/GreyforgeLabs/sqlite-checkpoint.git && cd sqlite-checkpoint && ./scripts/setup.sh
```

## What This Project Does

SQLite online backup and WAL checkpoint tool. Use `backup` for a consistent live copy; checkpointing is optional WAL maintenance. The CLI and Python library have no runtime Python dependencies. Test a restore before relying on a backup.

## Project Structure

```text
sqlite-checkpoint/
  src/sqlite_checkpoint/
    __init__.py         # package version
    core.py             # checkpoint, backup, snapshot, db_info logic
    cli.py              # argparse CLI entry point
    __main__.py         # python -m sqlite_checkpoint support
  tests/
    test_core.py        # unit tests for core functions
    test_cli.py         # CLI integration tests
  scripts/
    setup.sh            # idempotent install script
  .github/workflows/    # CI, release, and PyPI publish workflows
  README.md             # human-facing docs
  STARTHERE.md          # this file
```

## Setup Prerequisites

- Python 3.11+
- No system dependencies required (uses stdlib `sqlite3`)

## Installation Steps

1. Clone: `git clone https://github.com/GreyforgeLabs/sqlite-checkpoint.git`
2. Enter directory: `cd sqlite-checkpoint`
3. Run setup: `./scripts/setup.sh`

## Verification

```bash
source .venv/bin/activate
sqlite-checkpoint --version
# Expected output: sqlite-checkpoint 0.3.0
```

## Key Entry Points

- `src/sqlite_checkpoint/cli.py` — CLI entry point (`sqlite-checkpoint` command)
- `src/sqlite_checkpoint/core.py` — Python API (`checkpoint`, `backup`, `snapshot`, `db_info`)

## Configuration

No configuration files or environment variables required. All options are passed via CLI flags or function arguments.

## Common Tasks

```bash
# Run tests
python -m pytest

# Run linter
python -m ruff check .

# Format code
python -m ruff format .

# Build distribution
python -m build
```
