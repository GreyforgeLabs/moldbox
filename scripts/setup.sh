#!/usr/bin/env bash
set -euo pipefail

# moldbox setup script
# Idempotent - safe to run multiple times

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo "=== moldbox Setup ==="

check_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "ERROR: $1 is required but not installed."
        exit 1
    fi
}

check_command cargo
check_command rustc
check_command cc

if ! cargo fmt --version >/dev/null 2>&1; then
    echo "ERROR: rustfmt is required. Install it with: rustup component add rustfmt"
    exit 1
fi

cd "$PROJECT_DIR"

echo "Building..."
cargo build --locked

echo "Running tests..."
cargo test --locked

echo "Verification..."
cargo run --locked --quiet -- --version

echo "=== Setup complete ==="
