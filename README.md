# CodexImage

CodexImage is an infinite canvas for image generation. Each prompt sits in a
card. Branching a result adds a child card, so the parent prompt and every edit
stay visible.

It is a native macOS and Windows app built with
[Zed's GPUI](https://github.com/zed-industries/zed). It calls the Codex CLI
through your logged-in ChatGPT account. You do not need a separate API key.

## Install on macOS

You need an Apple Silicon Mac running macOS 13 or newer. Install the Codex CLI
and run `codex login` before opening CodexImage.

[Download the latest DMG](https://github.com/Turbovadim/CodexImage/releases/latest),
open it, then drag CodexImage into Applications.

The current test build is ad hoc signed and not yet notarized. On first launch,
right-click CodexImage and choose Open. If macOS still blocks it, open System
Settings > Privacy & Security and choose Open Anyway.

## Install on Windows

You need 64-bit Windows 10 or Windows 11. Install the Codex CLI, sign in, and
confirm that `codex` works in PowerShell before opening CodexImage:

```powershell
npm install -g @openai/codex
codex login
```

Download the latest `windows-x86_64.zip`, extract it to a permanent directory,
and run `CodexImage.exe`. Release builds are currently unsigned, so Microsoft
Defender SmartScreen may require **More info > Run anyway** on first launch.

## What it does

- Generates parallel takes from one prompt.
- Keeps branches visible as a graph instead of flattening them into chat history.
- Lets you branch, regenerate, edit, duplicate, or delete any result.
- Opens images in a lightbox and collects completed work in a gallery.
- Saves boards locally and marks interrupted jobs after a restart instead of
  leaving them stuck as running.

Large boards only decode visible cards. CodexImage uses thumbnails while zoomed
out and moves file writes, image decoding, and Codex sessions off the render
thread. Each generation runs in its own process group, so stopping a job closes
its full process tree.

## Run from source

Clone the repository, log in to the Codex CLI, then run on either platform:

```bash
cargo run --release
```

Build the app bundle with:

```bash
./scripts/package-macos.sh
open "dist/CodexImage.app"
```

Use `--install` to copy the app into Applications and `--open` to launch it:

```bash
./scripts/package-macos.sh --install --open
```

Create a versioned Apple Silicon DMG and checksum with:

```bash
./scripts/package-macos.sh --dmg
```

On Windows, install the Rust MSVC toolchain and Visual Studio 2022 Build Tools
with **Desktop development with C++**, then create a portable ZIP and checksum:

```powershell
.\scripts\package-windows.ps1 -Archive
```

Use `-Install` for a per-user install with a Start menu shortcut, and `-Open`
to launch the packaged application:

```powershell
.\scripts\package-windows.ps1 -Install -Open
```

## Data and generated images

CodexImage keeps boards under
`~/Library/Application Support/CodexImage/data` on macOS and
`%LOCALAPPDATA%\CodexImage\data` on Windows. If it finds data from the old
Electron app in the platform's roaming application-data directory, it opens
that instead. Set `CODEXIMAGE_DATA` to choose another data directory or
`CODEX_BIN` to use a specific Codex executable. Windows discovery includes the
standard npm, pnpm, Bun, Volta, Cargo, and WinGet locations.

The app keeps untouched generations under `generated-originals`. It creates
conditioned 16-bit copies for the canvas, exports, and later image edits. This
reduces repeating artifacts that can build up across a chain of edits. Existing
board images are conditioned once in the background.

Set `CODEXIMAGE_REINGEST_CONDITIONING=0` to turn conditioning off. Use a value
between `0` and `1` to reduce its strength.

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| `/` | Focus the prompt |
| `Enter` | Generate |
| `Shift+Enter` | Insert a newline |
| `⌘K` / `Ctrl+K` | Open the board switcher |
| `F` | Fit the canvas |
| `⌘=` / `Ctrl+=` | Zoom in |
| `⌘-` / `Ctrl+-` | Zoom out |
| `⌘0` / `Ctrl+0` | Reset zoom |
| `G` | Open the gallery |
| `Esc` | Close or cancel |
| `B` | Branch from the hovered node |
| `R` | Regenerate the hovered node |
| `E` | Edit the hovered node |
| `D` | Duplicate the hovered node |
| `Delete` | Delete the hovered node |
