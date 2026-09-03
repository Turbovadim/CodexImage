# CodexImage

CodexImage is an infinite canvas for image generation. Each prompt sits in a
card. Branching a result adds a child card, so the parent prompt and every edit
stay visible.

It is a native macOS app built with
[Zed's GPUI](https://github.com/zed-industries/zed). It calls the Codex CLI
through your logged-in ChatGPT account. You do not need a separate API key.

## Install

You need an Apple Silicon Mac running macOS 13 or newer. Install the Codex CLI
and run `codex login` before opening CodexImage.

[Download the latest DMG](https://github.com/Turbovadim/CodexImage/releases/latest),
open it, then drag CodexImage into Applications.

The current test build is ad hoc signed and not yet notarized. On first launch,
right-click CodexImage and choose Open. If macOS still blocks it, open System
Settings > Privacy & Security and choose Open Anyway.

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

Clone the repository, log in to the Codex CLI, then run:

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

## Data and generated images

CodexImage keeps boards under
`~/Library/Application Support/CodexImage/data`. If it finds data from the old
Electron app under `~/Library/Application Support/codeximage/data`, it opens
that instead. Set `CODEXIMAGE_DATA` to choose another data directory or
`CODEX_BIN` to use a specific Codex executable.

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
| `⌘K` | Open the board switcher |
| `F` | Fit the canvas |
| `⌘=` | Zoom in |
| `⌘-` | Zoom out |
| `⌘0` | Reset zoom |
| `G` | Open the gallery |
| `Esc` | Close or cancel |
| `B` | Branch from the hovered node |
| `R` | Regenerate the hovered node |
| `E` | Edit the hovered node |
| `D` | Duplicate the hovered node |
| `Delete` | Delete the hovered node |
