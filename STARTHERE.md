# STARTHERE.md - AI Bootstrap Guide

> This file is designed for AI coding assistants. If you are a human,
> see [README.md](README.md) for the human-friendly guide.

## Quick Bootstrap

```bash
git clone https://github.com/GreyforgeLabs/moldbox.git && cd moldbox && ./scripts/setup.sh
```

## What This Project Does

moldbox (formerly sqlite-checkpoint) is a SQLite online backup and WAL checkpoint tool, written in Rust. Use `backup` for a consistent live copy; checkpointing is optional WAL maintenance. SQLite is compiled into the binary (rusqlite `bundled`), so there are no runtime dependencies. The crate also exposes a Rust library API. Test a restore before relying on a backup.

## Project Structure

```text
moldbox/
  src/
    lib.rs              # library API: checkpoint, backup, snapshot, db_info
    cli.rs              # command dispatch and text/JSON output
    argparse.rs         # argparse-compatible command-line parsing
    help.rs             # help and usage texts
    pyfmt.rs            # output-compatibility helpers (repr, JSON, floats)
    printable.rs        # Python str.isprintable() table used by repr()
    main.rs             # `moldbox` binary entry point
    bin/sqlite-checkpoint.rs  # deprecated alias binary (to be removed in a future release)
    tests.rs            # library unit tests
    cli/tests.rs        # CLI unit tests
  tests/
    cli.rs              # end-to-end binary tests (+ optional differential test)
  scripts/
    setup.sh            # idempotent build and verification script
  docs/
    benchmarks.md       # Rust vs. Python measurements
  .github/workflows/    # CI and release workflows
  README.md             # human-facing docs
  STARTHERE.md          # this file
```

## Setup Prerequisites

- Rust 1.88+ with `cargo` and the `rustfmt`/`clippy` components
- A C compiler (`cc`) to build the bundled SQLite

## Installation Steps

1. Clone: `git clone https://github.com/GreyforgeLabs/moldbox.git`
2. Enter directory: `cd moldbox`
3. Run setup: `./scripts/setup.sh`
4. Optional install: `cargo install --path .`

## Verification

```bash
cargo run --locked --quiet -- --version
# Expected output: moldbox 0.4.2
```

## Key Entry Points

- `src/main.rs` / `src/cli.rs` — CLI entry point (`moldbox` command)
- `src/bin/sqlite-checkpoint.rs` — deprecated `sqlite-checkpoint` alias: prints a deprecation note to stderr, then runs `moldbox`
- `src/lib.rs` — Rust API (`checkpoint`, `backup`, `snapshot`, `db_info`)

## Configuration

No configuration files or environment variables required. All options are passed via CLI flags or function arguments.

## Common Tasks

```bash
# Run tests
cargo test --locked

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Format code
cargo fmt

# Build an optimized binary
cargo build --release --locked
```
