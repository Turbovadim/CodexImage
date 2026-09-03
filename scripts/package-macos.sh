#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/dist/CodexImage.app"
INSTALLED="/Applications/CodexImage.app"

INSTALL=0
OPEN=0
DMG=0
for argument in "$@"; do
  case "$argument" in
    --install) INSTALL=1 ;;
    --open) OPEN=1 ;;
    --dmg) DMG=1 ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--dmg] [--install] [--open]"
      echo "  --dmg      Create a versioned disk image in dist"
      echo "  --install  Copy the packaged app to /Applications"
      echo "  --open     Open the app after packaging"
      exit 0
      ;;
    *)
      echo "Unknown option: $argument" >&2
      echo "Usage: $(basename "$0") [--dmg] [--install] [--open]" >&2
      exit 1
      ;;
  esac
done

cargo build --manifest-path "$ROOT/Cargo.toml" --release --locked
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/codex-image" "$APP/Contents/MacOS/codex-image"
cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/resources/icon.icns" "$APP/Contents/Resources/icon.icns"
chmod 755 "$APP/Contents/MacOS/codex-image"

if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$APP" >/dev/null
fi

if [ "$DMG" -eq 1 ]; then
  VERSION="$(sed -nE 's/^version = "([^"]+)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
  ARCH="$(uname -m)"
  DMG_FILE="CodexImage-$VERSION-$ARCH.dmg"
  DMG_PATH="$ROOT/dist/$DMG_FILE"
  DMG_ROOT="$(mktemp -d)"
  trap 'rm -rf "$DMG_ROOT"' EXIT

  cp -R "$APP" "$DMG_ROOT/CodexImage.app"
  ln -s /Applications "$DMG_ROOT/Applications"
  hdiutil create \
    -volname "CodexImage" \
    -srcfolder "$DMG_ROOT" \
    -ov \
    -format UDZO \
    "$DMG_PATH" >/dev/null
  (cd "$ROOT/dist" && shasum -a 256 "$DMG_FILE" > "$DMG_FILE.sha256")

  printf '%s\n' "$DMG_PATH"
  printf '%s\n' "$DMG_PATH.sha256"
fi

TARGET="$APP"
if [ "$INSTALL" -eq 1 ]; then
  rm -rf "$INSTALLED"
  cp -R "$APP" "$INSTALLED"
  TARGET="$INSTALLED"
fi

printf '%s\n' "$TARGET"

if [ "$OPEN" -eq 1 ]; then
  open "$TARGET"
fi
