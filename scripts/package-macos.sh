#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/dist/CodexImage.app"

cargo build --manifest-path "$ROOT/Cargo.toml" --release
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/codex-image" "$APP/Contents/MacOS/codex-image"
cp "$ROOT/resources/Info.plist" "$APP/Contents/Info.plist"
cp "$ROOT/resources/icon.icns" "$APP/Contents/Resources/icon.icns"
chmod 755 "$APP/Contents/MacOS/codex-image"

if command -v codesign >/dev/null 2>&1; then
  codesign --force --sign - "$APP" >/dev/null
fi

printf '%s\n' "$APP"
