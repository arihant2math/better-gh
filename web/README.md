# Better GitHub — web client

Local-first React client: reads from an in-memory normalized store mirrored
to IndexedDB, writes optimistically, syncs over a WebSocket. Cookbook for
feature work: [`docs/FRONTEND.md`](../docs/FRONTEND.md). Wire protocol:
[`docs/SYNC_PROTOCOL.md`](../docs/SYNC_PROTOCOL.md).

## Scripts

| Script | |
|--------|---|
| `npm run dev` | Vite dev server on :5173, proxying `/api`, `/_bgh` (incl. WebSocket) and git paths to `BGH_BACKEND` (default `http://localhost:3000`) |
| `npm run dev:mock` | same, with the in-browser mock backend (or add `?mock` to any URL) |
| `npm run build` | production build to `dist/` (+ `.br`/`.gz` siblings, `sw.js`) **and the bundle budget check** |
| `npm run size` / `npm run analyze` | budget check / per-chunk breakdown of an existing build |
| `npm run typecheck` | `tsc` for app, build config and service worker |
| `npm run lint` | ESLint (typescript-eslint, react-hooks) |
| `npm test` | vitest (store, reconciliation, sync client vs. mock, query language, diff parser) |
| `node scripts/screenshots.mjs [url] [dir]` | Playwright screenshots of key pages (mock mode) |
| `node scripts/smoke.mjs [url]` | Playwright interaction smoke test (optimistic writes, rollback, reload, keyboard) |
| `node scripts/admin-smoke.mjs [url] [dir]` | Playwright smoke test of site admin + org settings against a real backend (no mock) |
| `node scripts/pat-smoke.mjs [url] [dir]` | Playwright smoke test of fine-grained PATs + org token policy/approval against a real backend (signs up a user, creates an org); `BGH_MOCK=1` runs it against the mock |

## Bundle budget

Enforced by `scripts/size-check.mjs`, run as part of `npm run build`:

| What | Budget |
|------|--------|
| Initial JS (everything `index.html` loads before first render), gzip | **≤ 150 KB** |
| Initial CSS, gzip | ≤ 30 KB |
| Any lazily loaded chunk, gzip | ≤ 60 KB |
| On-demand diagram chunks (only reachable through the Mermaid entry), gzip | ≤ 150 KB each |

Current: ~122 KB gzip initial JS (React DOM ≈ 58 KB, MobX ≈ 14 KB, app shell
+ sync engine ≈ 35 KB). Route pages, markdown (marked + DOMPurify), the
virtualizer and the mock backend are separate lazy chunks. The gemoji
table (`ui/markdown/emoji.json`, shared with the server, regenerate with
`node scripts/gen-emoji.mjs`) is its own chunk; math (temml, MathML output)
and Mermaid load only when a rendered body contains math or a ```mermaid
block. Mermaid and its d3/cytoscape dependencies exceed the 60 KB lazy cap,
so `size-check.mjs` budgets chunks reachable *only* through the Mermaid
entry separately; they are never prefetched.

## Serving (for bgh-server)

* `dist/assets/*` are content-hashed: serve with
  `Cache-Control: public, max-age=31536000, immutable`, preferring the
  precompressed `.br` / `.gz` file when `Accept-Encoding` allows.
* `dist/sw.js`: `Cache-Control: no-cache`, served from `/sw.js`.
* Every SPA route returns `dist/index.html` (`no-cache`) with the
  `<!--BGH_BOOT-->` comment replaced by
  `<script>window.__BGH_BOOT__={…}</script>` (shape: SYNC_PROTOCOL.md §9).
