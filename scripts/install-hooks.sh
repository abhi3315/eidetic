#!/usr/bin/env bash
# Install local git hooks for this repo.
#
# Idempotent. Safe to re-run.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOK_DIR="$REPO_ROOT/.git/hooks"
HOOK_PATH="$HOOK_DIR/pre-commit"
SOURCE_PATH="$REPO_ROOT/scripts/pre-commit.sh"

if [[ ! -f "$SOURCE_PATH" ]]; then
    echo "error: $SOURCE_PATH not found" >&2
    exit 1
fi

chmod +x "$SOURCE_PATH"

if [[ -e "$HOOK_PATH" && ! -L "$HOOK_PATH" ]]; then
    echo "warning: $HOOK_PATH exists and is not a symlink — backing up to $HOOK_PATH.bak"
    mv "$HOOK_PATH" "$HOOK_PATH.bak"
fi

ln -sf "$SOURCE_PATH" "$HOOK_PATH"
echo "pre-commit hook installed: $HOOK_PATH -> $SOURCE_PATH"
