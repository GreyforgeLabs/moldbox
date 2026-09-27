"""Command-line interface for sqlite-checkpoint."""

from __future__ import annotations

import argparse
import json
import sys

from sqlite_checkpoint import __version__
from sqlite_checkpoint.core import (
    BackupResult,
    CheckpointMode,
    CheckpointResult,
    IncompleteCheckpointError,
    SnapshotResult,
    backup,
    checkpoint,
    db_info,
    snapshot,
)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sqlite-checkpoint",
        description="Atomic SQLite backup & checkpoint tool.",
    )
    parser.add_argument("--version", action="version", version=f"%(prog)s {__version__}")
    parser.add_argument("--json", action="store_true", help="Output results as JSON")

    sub = parser.add_subparsers(dest="command", required=True)

    # checkpoint
    cp = sub.add_parser("checkpoint", help="Run a WAL checkpoint", aliases=["cp"])
    cp.add_argument("database", help="Path to the SQLite database")
    cp.add_argument(
        "-m",
        "--mode",
        choices=["passive", "full", "restart", "truncate"],
        default="passive",
        help="Checkpoint mode (default: passive)",
    )
    cp.add_argument(
        "--require-complete",
        action="store_true",
        help="Exit 3 when the checkpoint is busy or partial",
    )

    # backup
    bk = sub.add_parser("backup", help="Create an atomic backup", aliases=["bk"])
    bk.add_argument("database", help="Path to the SQLite database")
    bk.add_argument("destination", help="Path for the backup file")
    bk.add_argument(
        "--pages-per-step",
        type=int,
        default=1000,
        help="Positive pages to copy per backup step (default: 1000)",
    )
    bk.add_argument(
        "--pause-ms",
        type=float,
        default=0,
        help="Milliseconds to pause between backup steps (default: 0)",
    )

    # snapshot (checkpoint + backup)
    sn = sub.add_parser("snapshot", help="Checkpoint then backup (recommended)", aliases=["snap"])
    sn.add_argument("database", help="Path to the SQLite database")
    sn.add_argument("destination", help="Path for the backup file")
    sn.add_argument(
        "-m",
        "--mode",
        choices=["passive", "full", "restart", "truncate"],
        default="passive",
        help="Checkpoint mode (default: passive)",
    )
    sn.add_argument(
        "--pages-per-step",
        type=int,
        default=1000,
        help="Positive pages to copy per backup step (default: 1000)",
    )
    sn.add_argument(
        "--pause-ms",
        type=float,
        default=0,
        help="Milliseconds to pause between backup steps (default: 0)",
    )
    sn.add_argument(
        "--require-complete",
        action="store_true",
        help="Exit 3 when the checkpoint is busy or partial",
    )

    # info
    inf = sub.add_parser("info", help="Show database info")
    inf.add_argument("database", help="Path to the SQLite database")

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    use_json = args.json

    try:
        if args.command in ("checkpoint", "cp"):
            result = checkpoint(args.database, mode=CheckpointMode[args.mode.upper()])
            _print_checkpoint(result, use_json)
            if args.require_complete and not result.fully_checkpointed:
                return 3

        elif args.command in ("backup", "bk"):
            result = backup(
                args.database,
                args.destination,
                pages_per_step=args.pages_per_step,
                pause_ms=args.pause_ms,
            )
            _print_backup(result, use_json)

        elif args.command in ("snapshot", "snap"):
            try:
                result = snapshot(
                    args.database,
                    args.destination,
                    checkpoint_mode=CheckpointMode[args.mode.upper()],
                    pages_per_step=args.pages_per_step,
                    pause_ms=args.pause_ms,
                    require_complete=args.require_complete,
                )
            except IncompleteCheckpointError as error:
                _print_incomplete_snapshot(error.result, use_json)
                return 3
            _print_snapshot(result, use_json)

        elif args.command == "info":
            info = db_info(args.database)
            _print_info(info, use_json)

    except FileNotFoundError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except FileExistsError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    return 0


def _print_checkpoint(r: CheckpointResult, as_json: bool) -> None:
    if as_json:
        print(
            json.dumps(
                {
                    "command": "checkpoint",
                    "mode": r.mode.value,
                    "status": r.status,
                    "busy": r.busy,
                    "wal_pages": r.wal_pages,
                    "checkpointed_pages": r.checkpointed_pages,
                    "fully_checkpointed": r.fully_checkpointed,
                    "database": r.database,
                    "elapsed_ms": r.elapsed_ms,
                }
            )
        )
    else:
        print(f"checkpoint {r.mode.value} [{r.status}]")
        print(f"  busy:               {'yes' if r.busy else 'no'}")
        print(f"  wal pages:          {r.wal_pages}")
        print(f"  checkpointed:       {r.checkpointed_pages}")
        print(f"  elapsed:            {r.elapsed_ms} ms")


def _print_backup(r: BackupResult, as_json: bool) -> None:
    if as_json:
        print(
            json.dumps(
                {
                    "command": "backup",
                    "source": r.source,
                    "destination": r.destination,
                    "size_bytes": r.size_bytes,
                    "sha256": r.sha256,
                    "elapsed_ms": r.elapsed_ms,
                }
            )
        )
    else:
        print("backup complete")
        print(f"  source:             {r.source}")
        print(f"  destination:        {r.destination}")
        print(f"  size:               {r.size_bytes:,} bytes")
        print(f"  sha256:             {r.sha256}")
        print(f"  elapsed:            {r.elapsed_ms} ms")


def _print_incomplete_snapshot(r: CheckpointResult, as_json: bool) -> None:
    if as_json:
        print(json.dumps({
            "command": "snapshot",
            "checkpoint": {
                "mode": r.mode.value,
                "status": r.status,
                "busy": r.busy,
                "wal_pages": r.wal_pages,
                "checkpointed_pages": r.checkpointed_pages,
                "fully_checkpointed": False,
            },
            "backup": None,
            "published": False,
        }))
    else:
        print(f"snapshot not created: checkpoint {r.status}")


def _print_snapshot(r: SnapshotResult, as_json: bool) -> None:
    if as_json:
        print(
            json.dumps(
                {
                    "command": "snapshot",
                    "checkpoint": {
                        "mode": r.checkpoint.mode.value,
                        "status": r.checkpoint.status,
                        "busy": r.checkpoint.busy,
                        "wal_pages": r.checkpoint.wal_pages,
                        "checkpointed_pages": r.checkpoint.checkpointed_pages,
                        "fully_checkpointed": r.checkpoint.fully_checkpointed,
                    },
                    "backup": {
                        "source": r.backup.source,
                        "destination": r.backup.destination,
                        "size_bytes": r.backup.size_bytes,
                        "sha256": r.backup.sha256,
                    },
                    "elapsed_ms": round(r.checkpoint.elapsed_ms + r.backup.elapsed_ms, 2),
                }
            )
        )
    else:
        print(f"snapshot complete [checkpoint {r.checkpoint.status}]")
        print(f"  checkpoint busy:    {'yes' if r.checkpoint.busy else 'no'}")
        print(f"  wal pages:          {r.checkpoint.wal_pages}")
        print(f"  checkpointed:       {r.checkpoint.checkpointed_pages}")
        print(f"  destination:        {r.backup.destination}")
        print(f"  size:               {r.backup.size_bytes:,} bytes")
        print(f"  sha256:             {r.backup.sha256}")
        total = round(r.checkpoint.elapsed_ms + r.backup.elapsed_ms, 2)
        print(f"  elapsed:            {total} ms")


def _print_info(info: dict, as_json: bool) -> None:
    if as_json:
        print(json.dumps({"command": "info", **info}))
    else:
        print("database info")
        print(f"  path:               {info['path']}")
        print(f"  size:               {info['size_bytes']:,} bytes")
        print(f"  journal mode:       {info['journal_mode']}")
        print(f"  WAL file:           {'yes' if info['wal_file_exists'] else 'no'}")
        if info["wal_file_exists"]:
            print(f"  WAL size:           {info['wal_size_bytes']:,} bytes")
        print(f"  page size:          {info['page_size']:,}")
        print(f"  page count:         {info['page_count']:,}")
        print(f"  freelist pages:     {info['freelist_count']:,}")


if __name__ == "__main__":
    raise SystemExit(main())
