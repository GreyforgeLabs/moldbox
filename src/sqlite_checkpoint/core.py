"""Core checkpoint and backup logic for SQLite databases."""

from __future__ import annotations

import contextlib
import hashlib
import math
import os
import sqlite3
import stat
import tempfile
import time
from dataclasses import dataclass
from enum import Enum
from numbers import Real
from pathlib import Path
from urllib.parse import quote


class CheckpointMode(Enum):
    """SQLite WAL checkpoint modes."""

    PASSIVE = "PASSIVE"
    FULL = "FULL"
    RESTART = "RESTART"
    TRUNCATE = "TRUNCATE"


@dataclass(frozen=True)
class CheckpointResult:
    """Result of a WAL checkpoint operation."""

    mode: CheckpointMode
    busy: bool
    wal_pages: int
    checkpointed_pages: int
    database: str
    elapsed_ms: float

    @property
    def fully_checkpointed(self) -> bool:
        return not self.busy and self.wal_pages == self.checkpointed_pages

    @property
    def status(self) -> str:
        if self.busy:
            return "busy"
        if self.fully_checkpointed:
            return "complete"
        return "partial"


@dataclass(frozen=True)
class BackupResult:
    """Result of an atomic backup operation."""

    source: str
    destination: str
    size_bytes: int
    sha256: str
    elapsed_ms: float


@dataclass(frozen=True)
class SnapshotResult:
    """Result of a checkpoint-then-backup snapshot."""

    checkpoint: CheckpointResult
    backup: BackupResult


class IncompleteCheckpointError(RuntimeError):
    """A required complete checkpoint was partial; no backup was published."""

    def __init__(self, result: CheckpointResult) -> None:
        self.result = result
        super().__init__(f"checkpoint {result.status}; backup was not created")


@dataclass(frozen=True)
class _DatabaseIdentity:
    path: Path
    device: int
    inode: int


def checkpoint(
    db_path: str | Path,
    mode: CheckpointMode = CheckpointMode.PASSIVE,
) -> CheckpointResult:
    """Run a WAL checkpoint on the given SQLite database.

    Args:
        db_path: Path to the SQLite database file.
        mode: Checkpoint mode (PASSIVE, FULL, RESTART, TRUNCATE).

    Returns:
        CheckpointResult with page counts and timing.

    Raises:
        FileNotFoundError: If the database file does not exist.
        sqlite3.OperationalError: If the checkpoint fails.
    """
    identity = _validated_existing_database_path(Path(db_path))
    db_path = identity.path

    t0 = time.monotonic()
    conn = _connect_existing(identity, readonly=False)
    try:
        # PRAGMA wal_checkpoint returns (result, wal_pages, checkpointed_pages).
        # result: 0 = success, 1 = blocked by concurrent reader (PASSIVE only).
        busy, wal_pages, checkpointed = conn.execute(
            f"PRAGMA wal_checkpoint({mode.value})"
        ).fetchone()
    finally:
        conn.close()
    elapsed = (time.monotonic() - t0) * 1000

    return CheckpointResult(
        mode=mode,
        busy=bool(busy),
        wal_pages=max(wal_pages, 0),
        checkpointed_pages=max(checkpointed, 0),
        database=str(db_path),
        elapsed_ms=round(elapsed, 2),
    )


def backup(
    db_path: str | Path,
    dest_path: str | Path,
    *,
    pages_per_step: int = 1000,
    pause_ms: float = 0,
) -> BackupResult:
    """Create an atomic backup of a SQLite database using the online backup API.

    This is safe to run while the database is being written to. The backup
    will reflect a consistent snapshot even under concurrent writes.

    Args:
        db_path: Path to the source database.
        dest_path: Path for the backup file. Parent directory must exist.
        pages_per_step: Positive number of pages to copy per step.
        pause_ms: Milliseconds to sleep between steps (for throttling).

    Returns:
        BackupResult with size, checksum, and timing.

    Raises:
        FileNotFoundError: If the source database does not exist.
        FileExistsError: If the destination already exists.
    """
    _validate_backup_controls(pages_per_step=pages_per_step, pause_ms=pause_ms)
    identity = _validated_existing_database_path(Path(db_path))
    db_path = identity.path
    dest_path = Path(dest_path)

    if dest_path.exists():
        raise FileExistsError(f"Destination already exists: {dest_path}")

    descriptor, tmp_name = tempfile.mkstemp(prefix=f".{dest_path.name}.tmp-", dir=dest_path.parent)
    os.close(descriptor)
    tmp_path = Path(tmp_name)
    t0 = time.monotonic()
    source: sqlite3.Connection | None = None
    dest: sqlite3.Connection | None = None
    published = False
    try:
        source = _connect_existing(identity, readonly=True)
        dest = sqlite3.connect(str(tmp_path))
        source.backup(dest, pages=pages_per_step, sleep=pause_ms / 1000)
        dest.close()
        source.close()
        _quick_check(tmp_path)
        _fsync_file(tmp_path)
        # Atomic no-clobber publish: link succeeds only if dest_path does not exist.
        os.link(tmp_path, dest_path)
        published = True
        _fsync_directory(dest_path.parent)
    except BaseException:
        if published:
            # Best-effort rollback after a post-publish sync failure. The caller
            # must inspect the destination if this cleanup also fails.
            with contextlib.suppress(OSError):
                dest_path.unlink()
            with contextlib.suppress(OSError):
                _fsync_directory(dest_path.parent)
        with contextlib.suppress(OSError):
            tmp_path.unlink()
        with contextlib.suppress(Exception):
            dest.close()
        with contextlib.suppress(Exception):
            source.close()
        raise
    finally:
        with contextlib.suppress(Exception):
            dest.close()
        with contextlib.suppress(Exception):
            source.close()
        with contextlib.suppress(OSError):
            tmp_path.unlink()

    elapsed = (time.monotonic() - t0) * 1000
    size = dest_path.stat().st_size
    sha = _sha256(dest_path)

    return BackupResult(
        source=str(db_path),
        destination=str(dest_path),
        size_bytes=size,
        sha256=sha,
        elapsed_ms=round(elapsed, 2),
    )


def snapshot(
    db_path: str | Path,
    dest_path: str | Path,
    *,
    checkpoint_mode: CheckpointMode = CheckpointMode.PASSIVE,
    pages_per_step: int = 1000,
    pause_ms: float = 0,
    require_complete: bool = False,
) -> SnapshotResult:
    """Optionally maintain the WAL before creating a consistent online backup.

    A checkpoint is not needed for backup consistency. Use this combined
    operation only when WAL maintenance is also desired.

    Args:
        db_path: Path to the source database.
        dest_path: Path for the backup file.
        checkpoint_mode: WAL checkpoint mode to use before backup.
        pages_per_step: Positive number of pages per backup step.
        pause_ms: Milliseconds to sleep between steps (for throttling).
        require_complete: Refuse backup publication after a partial checkpoint.

    Returns:
        SnapshotResult containing both checkpoint and backup results.
    """
    _validate_backup_controls(pages_per_step=pages_per_step, pause_ms=pause_ms)
    cp = checkpoint(db_path, mode=checkpoint_mode)
    if require_complete and not cp.fully_checkpointed:
        raise IncompleteCheckpointError(cp)
    bk = backup(db_path, dest_path, pages_per_step=pages_per_step, pause_ms=pause_ms)
    return SnapshotResult(checkpoint=cp, backup=bk)


def db_info(db_path: str | Path) -> dict:
    """Return basic info about a SQLite database.

    Returns:
        Dict with keys: path, size_bytes, journal_mode, wal_file_exists,
        wal_size_bytes, page_size, page_count, freelist_count.
    """
    identity = _validated_existing_database_path(Path(db_path))
    db_path = identity.path

    wal_path = db_path.with_suffix(db_path.suffix + "-wal")

    conn = _connect_existing(identity, readonly=True)
    try:
        journal_mode = conn.execute("PRAGMA journal_mode").fetchone()[0]
        page_size = conn.execute("PRAGMA page_size").fetchone()[0]
        page_count = conn.execute("PRAGMA page_count").fetchone()[0]
        freelist = conn.execute("PRAGMA freelist_count").fetchone()[0]
    finally:
        conn.close()

    wal_exists, wal_size = _file_context(wal_path)
    return {
        "path": str(db_path),
        "size_bytes": db_path.stat().st_size,
        "journal_mode": journal_mode,
        "wal_file_exists": wal_exists,
        "wal_size_bytes": wal_size,
        "page_size": page_size,
        "page_count": page_count,
        "freelist_count": freelist,
    }


def _sha256(path: Path) -> str:
    """Compute SHA-256 hex digest of a file."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def _fsync_file(path: Path) -> None:
    with path.open("rb") as handle:
        os.fsync(handle.fileno())


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _quick_check(path: Path) -> None:
    """Validate a SQLite file before publishing it."""
    identity = _validated_existing_database_path(path)
    conn = _connect_existing(identity, readonly=True, immutable=True)
    try:
        result = conn.execute("PRAGMA quick_check").fetchone()
    finally:
        conn.close()
    if not result or result[0] != "ok":
        raise sqlite3.DatabaseError(f"SQLite quick_check failed for temporary backup: {result!r}")


def _validate_backup_controls(*, pages_per_step: int, pause_ms: float) -> None:
    if isinstance(pages_per_step, bool) or not isinstance(pages_per_step, int):
        raise ValueError("pages_per_step must be a positive integer")
    if pages_per_step <= 0:
        raise ValueError("pages_per_step must be greater than zero")
    if isinstance(pause_ms, bool) or not isinstance(pause_ms, Real):
        raise ValueError("pause_ms must be a finite nonnegative number")
    pause_value = float(pause_ms)
    if not math.isfinite(pause_value) or pause_value < 0:
        raise ValueError("pause_ms must be a finite nonnegative number")


def _validated_existing_database_path(path: Path) -> _DatabaseIdentity:
    try:
        initial = path.lstat()
    except FileNotFoundError as exc:
        raise FileNotFoundError(f"Database not found: {path}") from exc
    if stat.S_ISLNK(initial.st_mode) or not stat.S_ISREG(initial.st_mode):
        raise FileNotFoundError(f"Database is not a regular file: {path}")
    try:
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise FileNotFoundError(f"Database cannot be opened safely: {path}") from exc
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode) or not _same_identity(initial, opened):
            raise FileNotFoundError(f"Database path changed during validation: {path}")
        real_path = path.resolve(strict=True)
        resolved = real_path.stat()
        if not _same_identity(opened, resolved):
            raise FileNotFoundError(f"Database path changed during validation: {path}")
        return _DatabaseIdentity(real_path, opened.st_dev, opened.st_ino)
    finally:
        os.close(descriptor)


def _connect_existing(
    identity: _DatabaseIdentity, *, readonly: bool, immutable: bool = False
) -> sqlite3.Connection:
    mode = "ro" if readonly else "rw"
    immutable_option = "&immutable=1" if immutable else ""
    uri = f"file:{quote(str(identity.path), safe='/')}?mode={mode}{immutable_option}"
    connection = sqlite3.connect(uri, uri=True)
    try:
        connection.execute("PRAGMA schema_version").fetchone()
        current = identity.path.stat()
        if (current.st_dev, current.st_ino) != (identity.device, identity.inode):
            raise FileNotFoundError(f"Database path changed while opening: {identity.path}")
    except Exception:
        connection.close()
        raise
    return connection


def _same_identity(left: os.stat_result, right: os.stat_result) -> bool:
    return (left.st_dev, left.st_ino, left.st_mode) == (right.st_dev, right.st_ino, right.st_mode)


def _file_context(path: Path) -> tuple[bool, int]:
    """Return regular-file presence and size while tolerating WAL races."""
    try:
        metadata = path.lstat()
    except OSError:
        return False, 0
    if not stat.S_ISREG(metadata.st_mode):
        return False, 0
    return True, metadata.st_size
