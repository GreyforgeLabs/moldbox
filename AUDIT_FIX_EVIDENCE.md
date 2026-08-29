# Audit remediation evidence

Release: `0.2.0`

Date: 2026-08-28

## GF-AUD-009

- Source paths are opened with no-follow descriptor validation and SQLite `mode=ro`/`mode=rw`
  URIs, never create mode.
- Device/inode/type identity is checked before and after SQLite opens the source, and a schema read
  must succeed before backup begins.
- Symlinks, directories, missing sources, deletion, and replacement races fail without publishing a
  destination or recreating a source.
- Temporary backups receive an immutable no-create quick check before atomic no-clobber publication.
- WAL disappearance during informational inspection is treated as contextual absence, not a reason
  to create or substitute a database.

## GF-AUD-030

- `pages_per_step` must be a positive integer; `pause_ms` must be finite and nonnegative.
- Snapshot exposes the same page and pause controls as backup and validates them before checkpoint.
- Checkpoints report `complete`, `busy`, or `partial` in human and JSON output.
- `checkpoint` and `snapshot` both support `--require-complete`, returning exit code `3` for busy or
  partial results.

## Validation

- Tier 2: `PYTHONPATH=src python -m pytest -q`
- Static: `python -m ruff check src tests`
- Formatting: `python -m ruff format --check src tests`
- Packaging: `python -m build`; `python -m twine check dist/*`
- Patch hygiene: `git diff --check`

Tests cover missing/directory/symlink/read-only sources, replacement/deletion between validation and
connect, invalid controls, failed quick checks, WAL disappearance, status classes, CLI option parity,
and distinct completion-required exits.
