# Product

## Register

product

## Users

Power users generating images through their existing ChatGPT subscription — starting with the author, shared as open-source (non-commercial) for anyone comfortable installing the Codex CLI. They are keyboard-fluent, tool-literate people (the Raycast/Linear/Figma crowd) working in long iterative sessions: prompt, compare takes, branch the best one, refine. Context is a desktop app (Electron/macOS-first) on a large screen, usually in low ambient light, with dozens of generations in flight across an infinite canvas.

## Product Purpose

CodexImage is a local image-generation studio: a node-canvas UI on top of the Codex CLI, so generation runs through the user's ChatGPT subscription with no extra API cost. Every prompt is a node on an infinite canvas; any node can be branched, continued, or regenerated, and ×N fans out parallel takes side by side. Success = the fastest possible loop from idea to compared variations — the canvas holds the whole exploration tree so nothing is ever lost, and the chrome never gets between the user and the images.

## Brand Personality

Fast, dense, pro-tool. The energy of Raycast and Linear applied to a creative canvas: keyboard-first, zero ceremony, information-dense chrome that disappears into the task. Confident and quiet — the generated images are the only heroes on screen. Delight lives in responsiveness (instant layout, no flicker, hotkeys everywhere), not decoration.

## Anti-references

- **Web-app SaaS chrome**: marketing gradients, oversized rounded cards, hero metrics, onboarding tours — anything that reads "website" instead of "native tool".
- **AI-tool aesthetic**: sparkle icons, purple-to-pink gradients, "magic" theming that most AI image tools ship with.
- **Consumer-toy softness**: oversized touch targets, bouncy motion, emoji-heavy empty states.
- **Terminal brutalism**: monospace-everything, 1-bit borders, deliberately raw hacker styling.

## Design Principles

1. **Images are the interface.** The canvas and its cards get the space and contrast budget; chrome stays near-invisible until needed (hover, focus, selection).
2. **Never make the user wait or wonder.** Every generation shows live progress; layout reserves exact space before images load (zero layout shift); state (running, failed, selected, target) is always legible at a glance.
3. **Keyboard-first, mouse-optional.** Every frequent action has a hotkey; hover + key is a first-class interaction. New features ship with their shortcut, not as an afterthought.
4. **Dense but calm.** Pro-tool density (small type, tight controls, rich metadata) without noise — one accent color for actions and selection, neutrals for everything else.
5. **Native tool, not a website.** Desktop-app conventions: real context menus, no marketing flourishes, motion only to convey state (150–250 ms, ease-out).

## Accessibility & Inclusion

Sensible defaults, no formal WCAG target: readable contrast for text and controls on the dark theme, full keyboard operability for core flows (already a product pillar), visible focus/selection states, and `prefers-reduced-motion` fallbacks for any animation.
