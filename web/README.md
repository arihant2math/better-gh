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
| `npm run viewports -- [options]` | device-testing matrix: screenshots + layout checks at 9 viewports × 2 themes + live resize, see [below](#device-testing-viewport-matrix) |
| `node scripts/screenshots.mjs [url] [dir]` | Playwright screenshots of key pages (mock mode) |
| `node scripts/smoke.mjs [url]` | Playwright interaction smoke test (optimistic writes, rollback, reload, keyboard) |
| `node scripts/admin-smoke.mjs [url] [dir]` | Playwright smoke test of site admin + org settings against a real backend (no mock) |
| `node scripts/pat-smoke.mjs [url] [dir]` | Playwright smoke test of fine-grained PATs + org token policy/approval against a real backend (signs up a user, creates an org); `BGH_MOCK=1` runs it against the mock |

## Device testing (viewport matrix)

`scripts/viewport-matrix.mjs` (`npm run viewports`) loads routes of a
running server at every viewport in `docs/AGENT_WORKFLOW.md` → "Device
testing" — phones and tablets with touch emulation (`isMobile`, `hasTouch`,
DPR 2–3), laptop to 21:9 ultrawide and a portrait monitor — in light and
dark, plus a live-resize pass per route (opens at 1440 wide and shrinks to
360 in steps, re-checking each step, which catches overflow that only
appears on resize). Every page is checked for:

| Check | Fails when |
|---|---|
| `page-overflow` | `documentElement.scrollWidth > clientWidth` (horizontal page scroll) |
| `offscreen` | an element sticks out of the viewport without a scrolling ancestor (wholly off-screen positioned layers such as skip links are ignored) |
| `unreachable` | a control is clipped out of an `overflow: hidden` box |
| `clipped-text` | text is cut by an `overflow: hidden/clip` box (its own or an ancestor's) without `text-overflow: ellipsis` / line clamp |
| `squeezed-text` | text is squeezed to under 3 characters per line over 3+ lines (e.g. a title crushed by header actions) |
| `overlap` | the centre of a control is covered by another control |
| `tap-target` | touch viewports only: a control smaller than 32 px (inline links in running text and inputs inside a big enough `<label>` are exempt) |
| `console`, `request` | console errors / uncaught exceptions, failed requests, HTTP 5xx |
| `load` | the route didn't load (HTTP ≥ 400, timeout, `--wait-for` missing) |

```
# real backend with seeded data (../scripts/dev-setup.sh, scripts/seed-real.mjs)
npm run viewports -- --base http://localhost:3000 --login ada:password123
# mock backend (npm run dev:mock / vite preview): add --mock or ?mock to --base
npm run viewports -- --base http://localhost:4173 --mock --routes /,/acme/api
# quick look at one page
npm run viewports -- --routes /acme/api/issues --viewports phone,1440 --themes light --no-resize
```

It writes `test-results/viewports/<route>__<viewport>__<theme>.png` (and
`__resize-<width>__` shots for steps that found something new) plus
`report.json`, prints a route × viewport summary table and the issues, and
exits 1 when any issue isn't baselined. Known issues go in an `--allow`
file — one rule per line, `check route viewport theme selector`, `*`
wildcards, comma lists, missing fields match anything:

```
tap-target * phone-s,phone,tablet-p,tablet-l * div.feedLine > a.feedActor
clipped-text /acme/api/issues * dark *
```

`--write-allow <file>` writes every current issue as a baseline. Selectors
use the readable part of CSS-module class names, so they survive rebuilds.
The default route set (`/`, `/notifications`, `/acme`, `/ada`, `/acme/api`,
issues, an issue, pulls, `/settings`) at all viewports takes about 2–3
minutes with `--jobs 4`. `--help` lists every option. Playwright and
Chromium come preinstalled (`PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers`);
never run `playwright install`. The checks themselves are tested against
fixture pages in `scripts/viewport-matrix/viewport-matrix.test.mjs` (part
of `npm test`; skipped where Chromium isn't available).

## Bundle budget

Enforced by `scripts/size-check.mjs`, run as part of `npm run build`:

| What | Budget |
|------|--------|
| Initial JS (everything `index.html` loads before first render), gzip | **≤ 150 KB** |
| Initial CSS, gzip | ≤ 30 KB |
| Any lazily loaded chunk, gzip | ≤ 60 KB |
| On-demand diagram chunks (only reachable through the Mermaid entry), gzip | ≤ 150 KB each |

Current: ~131 KB gzip initial JS (React DOM ≈ 63 KB, MobX ≈ 12 KB, app shell
+ route table + sync engine ≈ 50 KB). Keep shared code out of the entry:
overlays (command palette, shortcut help, new-issue dialog), the palette's
command lists, inbox indicators and REST-backed route prefetchers
(`app/routePrefetch.ts`) are lazy chunks, and shell code imports small
modules (`ui/Avatar`, `ui/Kbd`) rather than barrels that drag in page-only
icons. `npm run analyze` lists the initial chunks; modulepreload lists skip
chunks the entry already loaded (`bghPreloadDedupe` in `build/plugins.ts`).
Route pages, markdown (marked + DOMPurify), the
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
