# CodexImage

Local image-generation studio: a flowith-style **node canvas** on top of the Codex CLI,
so image generation runs through **your ChatGPT subscription** (gpt-5.5 + its image
generation tool) with no extra API costs. Every prompt becomes a node on an infinite
canvas; branch, continue, and regenerate any node — ×4 gives you four takes side by side,
each its own branchable node.

## How it works

- A board is a tree of nodes. Each node = one prompt + its generated image(s), linked
  to the node it branched from.
- **Every generation is a fresh, stateless `codex exec` session** — no `resume`, no
  session files to fork or race on. A node's context is rebuilt from its ancestor
  prompt chain (as text) plus the parent's output image (attached by file path as the
  edit target). That's what makes regenerate-anywhere and N-way branching trivially safe.
- Children snapshot the source image path they were generated from, so regenerating a
  parent never corrupts existing branches (image files are never overwritten).
- Generated images are picked up from `~/.codex/generated_images/<thread-id>/` and
  copied into `data/images/`, so they survive codex session cleanup.
- ×N spawns N parallel codex processes, one per sibling node, each nudged toward its
  own distinct interpretation. ×4 takes roughly the same wall time as ×1.
- Reference images (attach or paste) are saved into `data/images/` and codex views
  them before generating.
- Old linear chats (`data/chats.json`) are auto-migrated into node chains in
  `data/boards.json` on first launch.

## Stack

- `server.ts` — zero-dependency Node HTTP server (Node ≥ 22.7 runs TypeScript natively).
  API + SSE progress events + codex process management.
- `src/` — Svelte 5 (runes) + TypeScript + Tailwind 4 on @xyflow/svelte, built with Vite.

## Usage

```bash
bun install        # or npm install
npm run build      # typecheck + build frontend to dist/
npm start          # serve app + API on http://localhost:4750
npm run app        # desktop app: builds everything and opens an Electron window
npm run app:dist   # package a standalone CodexImage.app into release/mac-arm64/
```

The Electron app runs the same API server inside its main process (bundled to
`dist-electron/server.mjs` via esbuild). In dev mode it shares the project's
`data/`; the packaged app keeps its data in
`~/Library/Application Support/CodexImage/data`. If a server is already running
on port 4750 (e.g. `npm run dev`), the app reuses it instead of starting its
own — handy for developing the UI with HMR (note that it then also uses that
server's data directory).

Development (Vite dev server on :5173 with HMR, proxying to the API):

```bash
npm run dev
```

## Requirements

- Codex CLI logged in to a ChatGPT account (`codex login`).
- The `image_generation` feature enabled (stable/default in recent codex versions —
  check with `codex features list`).

## Notes

- Simple object/illustration images take ~20–40 s; photorealistic people are the slow
  path (~3–4 min per image on OpenAI's side).
- Deleting a node moves its subtree to an in-memory trash for 5 minutes — the
  toast's Undo restores it. Deleting a board is permanent.
- Card thumbnails are generated with `sips`, which ships with macOS only; on other
  platforms the app still works but loads full-size images everywhere.
- Per-board codex event logs live in `data/logs/<board-id>.jsonl` for debugging.
- `boards.json` is written atomically (temp file + rename); if it's ever unreadable
  at startup, a copy is preserved as `boards.json.corrupt-<ts>` instead of being
  overwritten.
- `PORT` and `CODEX_BIN` env vars override the defaults (4750, `codex`).
