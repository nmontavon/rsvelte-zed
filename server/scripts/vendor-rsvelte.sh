#!/usr/bin/env bash
# Clone the rsvelte source tree (at a pinned rev, WITHOUT submodules) into
# server/.rsvelte so the path dependencies in Cargo.toml resolve. We skip
# submodules deliberately: rsvelte has SSH/private submodules (vize, corsa-bind)
# that can't be cloned anonymously, and none of the crates we build need them.
set -euo pipefail

# Keep this rev in sync with the comment in Cargo.toml and the CI workflow.
REV="${RSVELTE_REV:-3876a0106c8e134f67632fd89dd3001fde62a401}"
REPO="https://github.com/baseballyama/rsvelte.git"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$(cd "$SCRIPT_DIR/.." && pwd)/.rsvelte"

if [ -d "$DEST/.git" ]; then
  current="$(git -C "$DEST" rev-parse HEAD 2>/dev/null || echo none)"
  if [ "$current" = "$REV" ]; then
    echo "rsvelte already at $REV"
    exit 0
  fi
fi

rm -rf "$DEST"
# Blobless partial clone keeps it fast; no --recurse-submodules on purpose.
git clone --filter=blob:none --no-checkout "$REPO" "$DEST"
git -C "$DEST" checkout --detach "$REV"
echo "rsvelte checked out at $REV (no submodules)"
