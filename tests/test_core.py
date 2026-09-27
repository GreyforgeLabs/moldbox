"""Tests for the core checkpoint module."""

import math
import os
import sqlite3

import pytest

from sqlite_checkpoint import core
from sqlite_checkpoint.core import (
    BackupResult,
    CheckpointMode,
    CheckpointResult,
    SnapshotResult,
    backup,
    checkpoint,
    db_info,
    snapshot,
)


class TestCheckpoint:
    def test_passive_checkpoint(self, wal_db):
        result = checkpoint(wal_db)
        assert isinstance(result, CheckpointResult)
        assert result.mode == CheckpointMode.PASSIVE
        assert result.busy is False
        assert result.elapsed_ms >= 0

    def test_truncate_checkpoint(self, wal_db):
        result = checkpoint(wal_db, mode=CheckpointMode.TRUNCATE)
        assert result.fully_checkpointed
        # After TRUNCATE the WAL file should be empty or removed.
        wal_file = wal_db.with_suffix(".db-wal")
        assert not wal_file.exists() or wal_file.stat().st_size == 0

    def test_missing_database(self, tmp_path):
        with pytest.raises(FileNotFoundError):
            checkpoint(tmp_path / "nope.db")

    def test_status_distinguishes_complete_busy_and_partial(self):
        complete = CheckpointResult(CheckpointMode.PASSIVE, False, 2, 2, "db", 0)
        busy = CheckpointResult(CheckpointMode.PASSIVE, True, 2, 1, "db", 0)
        partial = CheckpointResult(CheckpointMode.PASSIVE, False, 2, 1, "db", 0)

        assert complete.status == "complete"
        assert busy.status == "busy"
        assert partial.status == "partial"


class TestBackup:
    def test_basic_backup(self, wal_db, tmp_path):
        dest = tmp_path / "backup.db"
        result = backup(wal_db, dest)
        assert isinstance(result, BackupResult)
        assert dest.exists()
        assert result.size_bytes > 0
        assert len(result.sha256) == 64
        assert dest.stat().st_mode & 0o777 == 0o600

    def test_backup_is_valid_database(self, wal_db, tmp_path):
        dest = tmp_path / "backup.db"
        backup(wal_db, dest)
        conn = sqlite3.connect(str(dest))
        rows = conn.execute("SELECT name FROM items ORDER BY id").fetchall()
        conn.close()
        assert rows == [("alpha",), ("bravo",)]

    def test_backup_can_be_restored_and_passes_integrity_check(self, wal_db, tmp_path):
        backup_path = tmp_path / "backup.db"
        restored_path = tmp_path / "restored.db"
        backup(wal_db, backup_path)
        with sqlite3.connect(backup_path) as source, sqlite3.connect(restored_path) as restored:
            source.backup(restored)
            assert restored.execute("PRAGMA integrity_check").fetchone() == ("ok",)
            assert restored.execute("SELECT name FROM items ORDER BY id").fetchall() == [
                ("alpha",), ("bravo",)
            ]

    def test_sync_failure_before_publish_leaves_no_destination(
        self, wal_db, tmp_path, monkeypatch
    ):
        destination = tmp_path / "backup.db"
        monkeypatch.setattr(
            core, "_fsync_file", lambda _path: (_ for _ in ()).throw(OSError("sync failed"))
        )
        with pytest.raises(OSError, match="sync failed"):
            backup(wal_db, destination)
        assert not destination.exists()

    def test_directory_sync_failure_rolls_back_published_name(
        self, wal_db, tmp_path, monkeypatch
    ):
        destination = tmp_path / "backup.db"
        monkeypatch.setattr(
            core, "_fsync_directory", lambda _path: (_ for _ in ()).throw(OSError("sync failed"))
        )
        with pytest.raises(OSError, match="sync failed"):
            backup(wal_db, destination)
        assert not destination.exists()

    def test_backup_refuses_overwrite(self, wal_db, tmp_path):
        dest = tmp_path / "backup.db"
        dest.write_bytes(b"existing")
        with pytest.raises(FileExistsError):
            backup(wal_db, dest)

    def test_backup_leaves_no_temp_file(self, wal_db, tmp_path):
        dest = tmp_path / "backup.db"
        backup(wal_db, dest)
        assert dest.exists()
        assert not list(tmp_path.glob(".backup.db.tmp-*"))

    def test_missing_source(self, tmp_path):
        source = tmp_path / "nope.db"
        destination = tmp_path / "out.db"
        with pytest.raises(FileNotFoundError):
            backup(source, destination)
        assert not source.exists()
        assert not destination.exists()

    @pytest.mark.parametrize("kind", ["directory", "symlink"])
    def test_backup_rejects_non_regular_source(self, wal_db, tmp_path, kind):
        source = tmp_path / "source"
        if kind == "directory":
            source.mkdir()
        else:
            source.symlink_to(wal_db)
        destination = tmp_path / "out.db"

        with pytest.raises(FileNotFoundError):
            backup(source, destination)

        assert not destination.exists()

    def test_backup_accepts_read_only_source(self, wal_db, tmp_path):
        wal_db.chmod(0o444)
        destination = tmp_path / "readonly-backup.db"

        backup(wal_db, destination)

        assert destination.exists()

    def test_source_replacement_between_validation_and_connect_fails_closed(
        self, wal_db, tmp_path, monkeypatch
    ):
        replacement = tmp_path / "replacement.db"
        with sqlite3.connect(replacement) as connection:
            connection.execute("CREATE TABLE replacement(value TEXT)")
        destination = tmp_path / "out.db"
        real_connect = core.sqlite3.connect
        replaced = False

        def replacing_connect(database, *args, **kwargs):
            nonlocal replaced
            if not replaced and isinstance(database, str) and "mode=ro" in database:
                replaced = True
                os.replace(replacement, wal_db)
            return real_connect(database, *args, **kwargs)

        monkeypatch.setattr(core.sqlite3, "connect", replacing_connect)

        with pytest.raises(FileNotFoundError, match="changed"):
            backup(wal_db, destination)

        assert not destination.exists()

    def test_source_deletion_between_validation_and_connect_never_recreates_it(
        self, wal_db, tmp_path, monkeypatch
    ):
        destination = tmp_path / "out.db"
        real_connect = core.sqlite3.connect
        deleted = False

        def deleting_connect(database, *args, **kwargs):
            nonlocal deleted
            if not deleted and isinstance(database, str) and "mode=ro" in database:
                deleted = True
                wal_db.unlink()
            return real_connect(database, *args, **kwargs)

        monkeypatch.setattr(core.sqlite3, "connect", deleting_connect)

        with pytest.raises(sqlite3.OperationalError):
            backup(wal_db, destination)

        assert not wal_db.exists()
        assert not destination.exists()

    @pytest.mark.parametrize("pages_per_step", [0, -1, True, 1.5])
    def test_backup_rejects_invalid_page_steps_before_output(
        self, wal_db, tmp_path, pages_per_step
    ):
        destination = tmp_path / "out.db"

        with pytest.raises(ValueError, match="pages_per_step"):
            backup(wal_db, destination, pages_per_step=pages_per_step)

        assert not destination.exists()

    @pytest.mark.parametrize("pause_ms", [-1, True, math.inf, math.nan])
    def test_backup_rejects_invalid_pause_before_output(self, wal_db, tmp_path, pause_ms):
        destination = tmp_path / "out.db"

        with pytest.raises(ValueError, match="pause_ms"):
            backup(wal_db, destination, pause_ms=pause_ms)

        assert not destination.exists()

    def test_failed_validation_never_publishes_destination(self, wal_db, tmp_path, monkeypatch):
        destination = tmp_path / "out.db"
        monkeypatch.setattr(
            core,
            "_quick_check",
            lambda _path: (_ for _ in ()).throw(sqlite3.DatabaseError("injected")),
        )

        with pytest.raises(sqlite3.DatabaseError):
            backup(wal_db, destination)

        assert not destination.exists()
        assert not list(tmp_path.glob(".out.db.tmp-*"))


class TestSnapshot:
    def test_snapshot_creates_valid_backup(self, wal_db, tmp_path):
        dest = tmp_path / "snap.db"
        result = snapshot(wal_db, dest)
        assert isinstance(result, SnapshotResult)
        assert dest.exists()
        # Verify data integrity.
        conn = sqlite3.connect(str(dest))
        rows = conn.execute("SELECT count(*) FROM items").fetchone()
        conn.close()
        assert rows[0] == 2

    def test_invalid_controls_fail_before_checkpoint(self, wal_db, tmp_path, monkeypatch):
        monkeypatch.setattr(
            core,
            "checkpoint",
            lambda *_args, **_kwargs: pytest.fail("checkpoint must not run"),
        )

        with pytest.raises(ValueError, match="pages_per_step"):
            snapshot(wal_db, tmp_path / "snap.db", pages_per_step=0)

    def test_required_checkpoint_failure_does_not_create_backup(
        self, wal_db, tmp_path, monkeypatch
    ):
        incomplete = CheckpointResult(CheckpointMode.PASSIVE, False, 10, 4, str(wal_db), 1.0)
        monkeypatch.setattr(core, "checkpoint", lambda *_args, **_kwargs: incomplete)
        destination = tmp_path / "snap.db"
        with pytest.raises(core.IncompleteCheckpointError) as error:
            snapshot(wal_db, destination, require_complete=True)
        assert error.value.result is incomplete
        assert not destination.exists()


class TestDbInfo:
    def test_info_returns_expected_keys(self, wal_db):
        info = db_info(wal_db)
        assert info["journal_mode"] == "wal"
        assert info["page_count"] >= 1
        assert info["size_bytes"] > 0

    def test_missing_database(self, tmp_path):
        with pytest.raises(FileNotFoundError):
            db_info(tmp_path / "nope.db")

    def test_wal_disappearance_during_context_read_is_tolerated(self, wal_db, monkeypatch):
        real_lstat = core.Path.lstat

        def disappearing_wal(path):
            if str(path).endswith("-wal"):
                raise FileNotFoundError(2, "gone", str(path))
            return real_lstat(path)

        monkeypatch.setattr(core.Path, "lstat", disappearing_wal)

        info = db_info(wal_db)

        assert info["wal_file_exists"] is False
        assert info["wal_size_bytes"] == 0
