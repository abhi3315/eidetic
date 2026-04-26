#!/usr/bin/env bash
# Pre-commit hook: format + lint check.
#
# Tests are intentionally NOT run here. They live in CI. Slow hooks
# get bypassed with --no-verify, which defeats the purpose.
#
# Install via: ./scripts/install-hooks.sh

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

echo "[pre-commit] cargo fmt --check"
cargo fmt --all -- --check

echo "[pre-commit] cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "[pre-commit] ok"
