# CodexImage GPUI

A native, keyboard-first image-generation studio built directly on
[Zed's GPUI](https://github.com/zed-industries/zed) and the Codex CLI.

## Requirements

- macOS (the current product target)
- Rust stable
- A logged-in Codex CLI (`codex login`)

## Run

```bash
cargo run --release
```

To create a self-contained macOS application bundle:

```bash
./scripts/package-macos.sh
open "dist/CodexImage.app"
```

Use `--install` to copy the bundle to `/Applications`, and `--open` to
launch it after packaging:

```bash
./scripts/package-macos.sh --install --open
```

On first launch, the app opens the Electron data at
`~/Library/Application Support/codeximage/data` when it contains existing
boards. New native-only installs use
`~/Library/Application Support/CodexImage/data`. Set `CODEXIMAGE_DATA` or
`CODEX_BIN` to override either location.

Generated images are adaptively conditioned before display, export, or reuse as
edit inputs. The untouched generation is kept under `generated-originals`,
while the board uses the corrected 16-bit copy. Existing generated board images
are upgraded once in the background. Set
`CODEXIMAGE_REINGEST_CONDITIONING=0` to disable conditioning, or use a value
between `0` and `1` to reduce its strength. The generation agent also uses the
same conditioner synchronously between dependent image calls made within one
run, preventing the artifact from accumulating through a sequence.

The renderer virtualizes offscreen graph cards, uses thumbnails until zoomed
in, and performs persistence, image decoding, and Codex sessions away from the
GPUI render path. Cards are cached across board updates and only rebuilt when
their node, layout, or images actually change, so one finished generation does
not re-encode the whole board. The dot grid coarsens by powers of two as you
zoom out, keeping both its density and its tile count constant. Each generation
runs in its own process group so stop, replacement, deletion, timeout, and app
quit terminate the entire job tree.

## Keyboard shortcuts

| Key | Action |
| --- | --- |
| `/` | Focus prompt |
| `Enter` / `Shift+Enter` | Generate / newline |
| `⌘K` | Board switcher |
| `F` | Fit canvas |
| `⌘=` / `⌘-` / `⌘0` | Zoom in / out / actual size |
| `G` | Gallery |
| `Esc` | Close or cancel |
| `B`, `R`, `E`, `D`, `Delete` | Branch, regenerate, edit, duplicate, delete hovered node |
