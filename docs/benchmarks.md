# Benchmarks: moldbox 0.4.0 (Rust) vs. sqlite-checkpoint 0.3.0 (Python)

Measured on 2026-10-05 on a desktop workstation (Linux 7.2.5 x86_64, AMD Ryzen 5 3600,
12 threads, 16 GB RAM).

- Rust: `target/release/moldbox` built with `cargo build --release --locked`
  (rustc 1.98.1; `opt-level = 3`, fat LTO, one codegen unit, stripped,
  `panic = "abort"`; bundled SQLite 3.51.3). The measurements were taken
  before the rename, when the same binary was still called `sqlite-checkpoint`;
  only the name has changed since.
- Python: the 0.3.0 sources (`main` at `085189e`) run as
  `PYTHONPATH=<checkout>/src python3 -m sqlite_checkpoint` with CPython 3.14.7
  and the system SQLite 3.53.4.

The working directory was on tmpfs, so `fsync` was cheap. On a disk-backed
filesystem both backup timings would gain the same device sync time. The
benchmark database `app.db` was a 16,429,056-byte WAL-mode database
(16,000 rows of 1,000-byte random blobs).

## Startup, wall time and peak resident memory

`hyperfine` and `/usr/bin/time` are not installed on the host. A small C probe
(`maxrss.c`) forks, `execvp`s the command with stdout/stderr on `/dev/null`,
waits with `wait4`, and prints the elapsed `CLOCK_MONOTONIC` time and
`ru_maxrss` (the value GNU `time -v` reports as "Maximum resident set size").
Each command ran 3 warm-up times and then 30 measured times; the values are
medians of the 30 runs. Backup and snapshot runs wrote a fresh destination
each time. The probe's own floor, measured with `/usr/bin/true`, is about
1.3 ms and 1,528 KiB. Other jobs were running on the host (1-minute load
average about 5 on 12 threads), so absolute times carry some noise.

```bash
cc -O2 -o maxrss maxrss.c     # fork + execvp + wait4 -> elapsed ms, ru_maxrss
./maxrss target/release/moldbox --json backup app.db out/rs0.db
./maxrss python3 -m sqlite_checkpoint --json backup app.db out/py0.db
```

| Command | Python 0.3.0 | Rust 0.4.0 | Speed-up |
|---|---:|---:|---:|
| `--version` (trivial, cold start) | 69.19 ms | 1.05 ms | 66x |
| `--help` | 71.94 ms | 1.17 ms | 61x |
| `--json info app.db` | 70.10 ms | 1.55 ms | 45x |
| `checkpoint app.db` | 67.67 ms | 1.62 ms | 42x |
| `--json backup app.db DEST` (16 MB, representative) | 114.10 ms | 42.03 ms | 2.7x |
| `snapshot app.db DEST` (16 MB) | 112.30 ms | 47.27 ms | 2.4x |

Most of Python's time on small commands is interpreter start-up and imports.
For `backup`, both versions spend most of the remaining time copying pages,
running `quick_check` and hashing the published file.

An earlier measurement with a shell loop timed by `date +%s%N` reported about
2.1 ms for the Rust start-up commands; that method adds roughly 1.9 ms of its
own (the same loop around `/usr/bin/true` measures 1.9 ms), which is why the
probe is used here.

| Peak RSS | Python 0.3.0 | Rust 0.4.0 |
|---|---:|---:|
| `--version` | 21,702 KiB | 2,916 KiB |
| `--help` | 21,708 KiB | 2,920 KiB |
| `--json info app.db` | 22,840 KiB | 4,734 KiB |
| `checkpoint app.db` | 22,674 KiB | 4,756 KiB |
| `--json backup app.db DEST` (16 MB) | 26,542 KiB | 8,596 KiB |
| `snapshot app.db DEST` (16 MB) | 26,640 KiB | 8,924 KiB |

## Installed footprint

| | Python 0.3.0 | Rust 0.4.0 |
|---|---:|---:|
| Tool itself | 22,355 bytes of package source (`src/sqlite_checkpoint`, excluding `__pycache__`) | 2,635,392-byte stripped binary (SQLite linked in statically) |
| Runtime requirements | CPython >= 3.11 (here: 6,360,840-byte `libpython3.14.so` + standard library) plus `_sqlite3` (147,264 bytes) and the system `libsqlite3` (1,673,352 bytes) | only the C runtime (`libc`, `libm`, `libgcc_s`) |

Most of the binary is the bundled SQLite amalgamation, which includes the same
compile-time features (FTS3/FTS5, R*Tree, JSON) that a system SQLite would
provide.

## Output compatibility check

A differential harness ran 559 argument vectors through both implementations
in fresh fixture directories: help, usage errors and argparse edge cases,
every subcommand in text and JSON form, missing, symlinked, non-regular,
unreadable, empty, truncated, corrupt and hot-journal sources, read-only files
and directories, existing, dangling, directory and unwritable destinations,
invalid and unusual numeric controls, and Unicode, control-character and
non-UTF-8 paths. It compared exit codes, stdout, stderr, resulting files and
file modes after masking only elapsed times, random temporary names and
SHA-256 digests. 541 were identical. The other 18 are the deviations listed in
the CHANGELOG: extensionless and non-UTF-8 paths now work, the
`--pages-per-step` overflow message, `$COLUMNS`-dependent help wrapping, and one
corrupt database that SQLite 3.53.4 and the bundled 3.51.3 report with
different messages. A further 500 random argument vectors and about 11,000
code points passed through `repr()` in error messages were identical.
Concurrency checks (a held reader, a held writer, an exclusive lock, and a
writer committing during `backup` and `snapshot`) gave the same results in both
versions, and every published copy passed `PRAGMA integrity_check` after a
restore.

Backup files differ only in header bytes 96-99, where SQLite records the
version of the library that wrote the file (3051003 vs. 3053004). This is why
digests differ between the implementations.

The end-to-end test `differential_against_python_reference` in `tests/cli.rs`
repeats a subset of these comparisons when
`SQLITE_CHECKPOINT_PY_SRC=/path/to/python/checkout/src` is set. It runs the
Python module `sqlite_checkpoint` with its argparse program name set to
`moldbox`, so help and usage layout is still compared byte for byte.
