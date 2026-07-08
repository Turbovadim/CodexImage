# Agent notes

## Design Context

Read `PRODUCT.md` before any UI work. Short version: CodexImage is a **product-register** desktop tool (Electron + Svelte 5 + Tailwind 4) — a fast, dense, keyboard-first node-canvas image-generation studio in the Raycast/Linear spirit. Generated images are the heroes; chrome stays quiet. One indigo accent (`--color-accent`) for actions/selection, dark neutrals for everything else (tokens in `src/index.css`). No SaaS/marketing styling, no "AI magic" gradients, no bouncy consumer motion, no terminal brutalism. Every frequent action gets a hotkey; motion only conveys state (150–250 ms, ease-out); zero layout shift is a hard rule.

`DESIGN.md` doesn't exist yet — generate it with `$impeccable document` once wanted.
