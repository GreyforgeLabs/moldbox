"""Tests for the CLI module."""

import json

from sqlite_checkpoint import cli
from sqlite_checkpoint.cli import main
from sqlite_checkpoint.core import (
    BackupResult,
    CheckpointMode,
    CheckpointResult,
    SnapshotResult,
)


class TestCLI:
    def test_checkpoint_command(self, wal_db, capsys):
        rc = main(["checkpoint", str(wal_db)])
        assert rc == 0
        out = capsys.readouterr().out
        assert "checkpoint PASSIVE" in out

    def test_backup_command(self, wal_db, tmp_path, capsys):
        dest = tmp_path / "cli_backup.db"
        rc = main(["backup", str(wal_db), str(dest)])
        assert rc == 0
        assert dest.exists()
        out = capsys.readouterr().out
        assert "sha256" in out

    def test_snapshot_command(self, wal_db, tmp_path, capsys):
        dest = tmp_path / "cli_snap.db"
        rc = main(["snapshot", str(wal_db), str(dest)])
        assert rc == 0
        out = capsys.readouterr().out
        assert "snapshot complete" in out

    def test_info_command(self, wal_db, capsys):
        rc = main(["info", str(wal_db)])
        assert rc == 0
        out = capsys.readouterr().out
        assert "wal" in out

    def test_json_output(self, wal_db, capsys):
        rc = main(["--json", "info", str(wal_db)])
        assert rc == 0
        data = json.loads(capsys.readouterr().out)
        assert data["command"] == "info"
        assert data["journal_mode"] == "wal"

    def test_missing_database_returns_error(self, tmp_path, capsys):
        rc = main(["info", str(tmp_path / "nope.db")])
        assert rc == 1

    def test_alias_cp(self, wal_db, capsys):
        rc = main(["cp", str(wal_db)])
        assert rc == 0

    def test_checkpoint_require_complete_returns_distinct_exit(self, capsys, monkeypatch):
        result = CheckpointResult(CheckpointMode.PASSIVE, True, 10, 4, "db", 1.0)
        monkeypatch.setattr(cli, "checkpoint", lambda *_args, **_kwargs: result)

        rc = main(["--json", "checkpoint", "db", "--require-complete"])

        assert rc == 3
        output = json.loads(capsys.readouterr().out)
        assert output["status"] == "busy"
        assert output["fully_checkpointed"] is False

    def test_snapshot_option_parity_and_require_complete(self, tmp_path, capsys, monkeypatch):
        checkpoint_result = CheckpointResult(CheckpointMode.PASSIVE, False, 10, 4, "db", 1.0)
        backup_result = BackupResult("db", str(tmp_path / "snap.db"), 1, "a" * 64, 1.0)
        captured = {}

        def fake_snapshot(database, destination, **kwargs):
            captured.update(database=database, destination=destination, **kwargs)
            return SnapshotResult(checkpoint_result, backup_result)

        monkeypatch.setattr(cli, "snapshot", fake_snapshot)

        rc = main(
            [
                "snapshot",
                "db",
                str(tmp_path / "snap.db"),
                "--pages-per-step",
                "17",
                "--pause-ms",
                "2.5",
                "--require-complete",
            ]
        )

        assert rc == 3
        assert captured["pages_per_step"] == 17
        assert captured["pause_ms"] == 2.5
        assert "checkpoint partial" in capsys.readouterr().out

    def test_invalid_backup_controls_return_usage_failure_without_output(
        self, wal_db, tmp_path, capsys
    ):
        destination = tmp_path / "invalid.db"

        rc = main(["backup", str(wal_db), str(destination), "--pages-per-step", "0"])

        assert rc == 2
        assert not destination.exists()
        assert "pages_per_step" in capsys.readouterr().err
