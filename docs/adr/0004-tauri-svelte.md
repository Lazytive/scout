# 0004: Tauri 2 + Svelte 5 + TypeScript for the desktop app

Status: accepted (M0); the app itself starts in M6.

## Context
The brief fixes Tauri 2 and lets us pick Svelte or Solid.

## Decision
Svelte 5 (runes) + TypeScript + Vite. Reasons: a larger ecosystem with more Tauri examples,
compiled output with no virtual DOM (small and fast for the library grid), and scoped CSS
that maps cleanly onto theme tokens (CSS custom properties). MIT licence. Solid would also
work; the deciding factor is maintainability by future contributors.

## Consequences
Front-end packages go through the npm licence audit in `cargo xtask audit`; lint/tests
(eslint, svelte-check, vitest) join `cargo xtask ci` when `apps/klip-app` appears.
