#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

ARCHIVE=0
for argument in "$@"; do
  case "$argument" in
    --archive) ARCHIVE=1 ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--archive]"
      echo "  --archive  Create a versioned tarball and checksum in dist"
      exit 0
      ;;
    *)
      echo "Unknown option: $argument" >&2
      echo "Usage: $(basename "$0") [--archive]" >&2
      exit 1
      ;;
  esac
done

cargo build --manifest-path "$ROOT/Cargo.toml" --release --locked

VERSION="$(sed -nE 's/^version = "([^"]+)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
TARGET="linux-$(uname -m)"
NAME="CodexImage-$VERSION-$TARGET"
STAGE="$ROOT/dist/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"
# The app binary doubles as its own image-conditioning helper on Linux.
install -m 755 "$ROOT/target/release/codex-image" "$STAGE/codex-image"
cp "$ROOT/README.md" "$STAGE/"

if [ "$ARCHIVE" -eq 1 ]; then
  ARCHIVE_FILE="$NAME.tar.gz"
  tar -C "$ROOT/dist" -czf "$ROOT/dist/$ARCHIVE_FILE" "$NAME"
  (cd "$ROOT/dist" && sha256sum "$ARCHIVE_FILE" > "$ARCHIVE_FILE.sha256")
  echo "$ROOT/dist/$ARCHIVE_FILE"
  echo "$ROOT/dist/$ARCHIVE_FILE.sha256"
else
  echo "$STAGE"
fi
