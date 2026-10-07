# Frontend cookbook (web/)

How to build features in the web client. Read `docs/ARCHITECTURE.md` (web
section) and `docs/SYNC_PROTOCOL.md` first; this file is the practical part.

```
cd web && npm install
npm run dev:mock     # fully working UI against the in-browser mock backend
npm run dev          # against bgh-server on :3000 (proxied: /api, /_bgh incl. WS, git paths)
npm run typecheck && npm run lint && npm test && npm run build   # must pass
```

Lint enforces the conventions below that are cheap to check statically
(`eslint.config.js`, local rules in `build/eslint-rules.mjs`): `routes.ts`
imports no pages or `api/*` modules; `mermaid`/`temml` are only `import()`ed,
`marked`/`dompurify` stay in `ui/markdown/`, `@tanstack/react-virtual` in
`ui/VirtualList` or page chunks; raw `fetch` only in `api/`, `main.tsx` and
`sw.ts`; a component that calls `store()` or a `sync/selectors` reader during
render must be `observer`.

Open any URL with `?mock` to use the mock backend (sticky for the tab;
`?mock=0` leaves). Extra flags: `&reset` (fresh seed + empty local DB),
`&live=0` (no simulated activity), `&fail=0.3` (30 % retryable mutation
failures), `&latency=0`. A title or comment containing `fail!` gets a `422`
from the mock, handy for testing rollbacks.

## Map

| Path | What |
|------|------|
| `src/main.tsx` | boot: boot data / mock, routes, keyboard listener, render, service worker |
| `src/app/` | shell (sidebar, top bar), `routes.ts`, command palette + `commands.ts`, session, theme, shortcut help |
| `src/router/` | the router (`Link`, `navigate`, `useParams`, `useQuery`, `setQuery`, `prefetch`) |
| `src/sync/` | local-first store: `models.ts`, `schema.ts`, `pool.ts`, `client.ts`, `transactions.ts`, `mutations.ts`, `selectors.ts`, `hooks.ts` |
| `src/api/` | REST client (`api`, `v3()`), `endpoints.ts`, resource cache (`useResource`, `prefetch`) |
| `src/ui/` | design system (import from `ui/…` files or the `ui` barrel) |
| `src/shortcuts/` | `useShortcuts`, `formatKeys` |
| `src/components/` | domain components shared by pages: `diff/DiffViewer`, `editor/MarkdownEditor` (toolbar, preview, `@`/`#` autocomplete), `labels/ColorPicker`, `ConfirmDialog`; `admin/` = kit for admin-style pages |
| `src/pages/<area>/` | route pages, one folder per area, each with its own CSS module |
| `src/mock/` | in-browser backend (reference implementation of the sync protocol) |

## Router choice

A ~250-line custom router (`src/router/index.tsx`) instead of wouter or
TanStack Router: we need exactly (1) lazy route chunks, (2) *data* prefetch
on link intent that talks to our sync client, (3) "keep showing the old page
until the next chunk is ready" (no Suspense flashes), (4) persistent layouts
and (5) scroll restoration — and nothing else. wouter lacks 2–4; TanStack
Router has them but costs ~12 KB gzip and its loader model fights the
local-store model (our pages read synchronously from MobX, not loaders).

## Add a route / page

1. Create `src/pages/<area>/<Name>Page.tsx` with a **default export** wrapped
   in `observer` (it reads the store).
2. Register it in `src/app/routes.ts`:

```ts
{
  path: '/:owner/:repo/releases',
  layout: RepoLayout,                       // optional persistent layout
  load: () => import('../pages/releases/ReleasesPage'),
  prefetch: lazyPrefetch((m, p) => m.prefetchReleases(p)),  // helper in app/routePrefetch.ts
  title: (p) => `Releases · ${p.owner}/${p.repo}`,
},
```

The most specific route wins (static segments beat `:params`, which beat
`*`; ties keep table order), so `/site-admin` beats `/:owner` and
`/:owner/:repo/settings/secrets/actions` beats `/:owner/:repo/settings/*`
wherever they sit in the table. `routes.ts` is part of the initial bundle,
so it must not import REST wrappers (`api/*`) or page modules statically:
put data prefetchers in the lazy `src/app/routePrefetch.ts` (or a page's
own `data.ts`) and reference them through `lazyPrefetch`. Use `<Link to=…>` for every internal link: it prefetches
the chunk and calls the route's `prefetch` on hover/focus/touch.
`useParams()`, `useQuery()`, `setQuery({ q: … })` (replace, keeps scroll) and
`navigate(path)` cover the rest.

Pages inside `RepoLayout` scroll in the layout body. A page that wants a
fixed header and its own scroller (lists, diffs) uses `height: 100%` and a
`flex: 1; min-height: 0` child (see `IssueList.module.css`).

## Read from the store

Everything synced is in the object pool; reads are synchronous and reactive.

```tsx
import { observer } from 'mobx-react-lite';
import { store } from '../../sync';
import { issueByNumber, labelsForRepo, repoByName } from '../../sync/selectors';

export default observer(function MyPage() {
  const repo = repoByName(owner, name);                 // derived key index
  const issue = repo && issueByNumber(repo.id, 42);
  const author = store().get('user', issue?.authorId);  // by id
  const open = store().byIndex('issue', 'repoId', repo!.id).filter((i) => i.state === 'open'); // secondary index
  …
});
```

* Components that read the store **must** be `observer(...)`. They re-render
  only when a field they read changes (field-level tracking).
* Expensive derivations (filter/sort thousands of rows): `useComputed(() => …, deps)`
  from `sync/hooks` caches the result until an input changes.
* Lazy data (issue body, comments, reviews, timeline events) is not in the
  bootstrap: call `useIssueDetails(issue.id)` (or `sync().loadIssue(id)` in a
  route `prefetch`). Render what you have immediately; use `<Skeleton>` only
  for the lazy part.
* Rows are read-only. Never mutate them — use a mutation.
* Data not in the sync model (git trees, blobs, diffs, commits, releases …):
  `useResource(key, loader, { immutable })` from `api/cache` + typed calls in
  `api/endpoints.ts`. Key by SHA and pass `immutable: true` when content
  addressed; warm it from the route's `prefetch`.

## Optimistic mutations

All writes go through `sync/mutations.ts`. A mutation = overlay ops (applied
instantly, persisted, survive reloads) + the GitHub REST request that
performs it. Reconciliation/rollback is automatic (SYNC_PROTOCOL.md §7).

```ts
export function lockIssue(issue: Issue, reason?: string) {
  return commit(`Lock #${issue.number}`, [ops.update('issue', issue.id, { locked: true })], {
    method: 'PUT',
    path: issuePath(issue, '/lock'),
    body: { lock_reason: reason },
  });
}
```

* `ops.update(model, id, patch)` — arrays can use `{ $add: [...], $remove: [...] }`
  so concurrent edits compose; object fields (e.g. `projectItem.values`) can use
  `{ $merge: { key: value } }` (a `null` value removes the key); `ops.insert(model, row)` with `tempId()` for
  creates; `ops.delete(model, id)`.
* Returns `{ tx, done }`. You usually ignore `done`; failures roll back and
  toast automatically. Await `done` only for follow-ups (e.g. navigate to a
  created issue once it has a number).
* The server half: the endpoint must record sync actions with the request's
  `X-Client-Tx` (see ARCHITECTURE.md "Sync engine"). Add the same handler to
  `src/mock/server.ts` so the UI works in mock mode.

## Add a synced model

1. Add the interface to `src/sync/models.ts` and `ModelMap`, and to
   `docs/SYNC_PROTOCOL.md` §3 (+ the Rust struct in `bgh-sync`).
2. Add a `SCHEMA` entry in `src/sync/schema.ts`: `scope`, `indexes`, `keys`,
   `lazyFields`, `lazy`, `cascade`. Bump `CLIENT_SCHEMA_VERSION` if the
   persisted shape changes (local DBs are discarded and re-bootstrapped).
3. Add selectors in `sync/selectors.ts`, mutations in `sync/mutations.ts`.
4. Seed + serve it in `src/mock/seed.ts` / `server.ts`.

## Shortcuts and commands

```tsx
useShortcuts('Releases', {
  n: { handler: () => openNew(), description: 'New release', group: 'Releases' },
  'g r': () => navigate(`${base}/releases`),
});
useCommands([{ id: 'release.new', title: 'Create release', group: 'Releases', shortcut: 'n', run: openNew }], [base]);
```

Keys: single (`j`, `?`, `escape`), chords (`mod+k` = ⌘K/Ctrl+K,
`shift+e`), sequences (`g i`). The most recently mounted scope wins; return
`false` from a handler to let the key fall through. Shortcuts are ignored
while typing in inputs unless they use `mod` or set `allowInInput`.
Everything with a `description` shows up in the `?` help dialog. Conventions:
`j/k` move, `Enter`/`o` open, `x` select, `e` edit / mark done, `l` labels,
`a` assignees, `m` milestone, `r` reply, `c` create, `f` filter, `g <x>` go to.

## UI components

`ui/`: `Button` (`variant`: secondary · primary · success · danger · ghost;
`size`; `leadingIcon`; `kbd` hint; `loading`), `IconButton` (required
`label` → tooltip + aria-label), `Input` / `Textarea` / `Select` / `Field`,
`Popover` (top layer, outside-click/Esc), `Menu` (action list),
`SelectPanel` (filterable single/multi picker — toggles are applied
immediately), `Tooltip`, `Dialog` (native modal), `TabNav` (links) / `Tabs`
(buttons), `Counter`, `Tag`, `LabelPill` (any GitHub color, both themes),
`ColorDot`, `StateIcon` / `StateBadge` (issue/PR state), `Avatar` /
`AvatarStack`, `Kbd`, `Spinner`, `Skeleton`, `EmptyState`, `Box`,
`toast()`, `VirtualList`, `Markdown` (GFM, sanitized, server-parity
references/autolinks/emoji/alerts/footnotes, `onSourceChange` for editable
task lists; lazy chunk, highlighting/math/Mermaid/camo applied by
`ui/markdown/enhance.ts`), `RelativeTime`, `ErrorBoundary` (Retry/Reload
fallback; `variant` `page` | `content` | `silent`, `resetKey` clears it). Icons:
`ui/icons.ts` (Octicons; add names there).

Error boundaries: one around `<App />` (`main.tsx`), one around the routed
page inside `Shell` (reset on navigation, so the sidebar, top bar and palette
survive a page crash), one per bare auth page, and a silent one per lazy
overlay. Failed lazy chunks (stale hashes after a deploy) reload the page
once via `router/chunkError.ts`; a sessionStorage stamp stops reload loops,
after which the boundary shows its error UI. Wrap new lazy UI in a boundary.

Search/filter inputs with GitHub qualifier autocomplete (`is:`, `label:`,
`author:@me`, `repo:`…): `search/QueryInput` with a qualifier set from
`search/qualifiers.ts` (`issues`, `code`, …, or `issue-list` / `pull-list`
for the local filter language) and `storeValueSource(repo?)` for values.
Server search: `search/api.ts` (`paletteSearch`, `search`, `searchCount`);
latency samples: `recordPerf` / `window.__bghPerf.stats()`.

Styling: CSS Modules per component/page + the tokens in `ui/tokens.css`
(`var(--fg-muted)`, `var(--border)`, `var(--radius)`, `var(--sp-3)`…). Never
hard-code colors; both themes must work (check with the theme toggle). Keep
animations ≤ 150 ms and use `var(--dur)` / `var(--ease)`.

## Admin-style pages (site admin, org settings)

Data that isn't synced (site admin, org settings, audit logs, jobs) is read
over REST with `useResource` (detail) or `usePagedList` (lists that follow
`Link: rel="next"`, cached per URL), and written with plain requests;
update caches with `mutate`/`refresh` (`api/cache`) and
`updateLists`/`invalidateLists`. Build pages from `components/admin`:

* `DataTable` — virtualized, sticky header, server sort, infinite scroll,
  `j`/`k`/`Enter`; `kit.tsx` — `PageHeader`, `Panel`, `KeyValue`,
  `StatusPill` (icon + label, never colour alone), `SearchInput` (`/`),
  `Switch`, `RadioCards`, `useConfirm` (type-to-confirm, reason),
  `Drawer`, `JsonView`, `errorMessage`;
* `charts.tsx` — hand-rolled SVG `StatTile`, `Sparkline`, `Meter`,
  `StackedBar`, `BarList` using the `--chart-*` tokens (categorical slots
  in fixed order, validated for colour-blindness in both themes);
* `format.ts` (`formatBytes`, `formatCount`, `plural`, dates), `csv.ts`
  (client-side CSV export, formula-injection safe).

Site admin lives under `/site-admin/*` (`pages/admin`, guarded by
`site.viewerSiteAdmin` from `app/site.ts`), org settings under
`/organizations/:org/settings/*` (`pages/orgsettings`). App-wide
announcement / maintenance banners come from `GET /_bgh/site` and are a lazy
chunk loaded only while one is active. The admin UI has no mock backend
(org rulesets, the org danger zone and admin deleted repositories excepted):
verify it against a real server with `scripts/admin-smoke.mjs`.

## Performance rules

1. **Never await the network to navigate or to show what's in the store.**
   Pages render synchronously from the pool; only lazy parts may show a
   skeleton. Add a route `prefetch` for anything fetched on load.
2. **Virtualize** any list that can exceed ~100 rows (`VirtualList`).
3. **Budget:** initial JS ≤ 150 KB gzip, any lazy chunk ≤ 60 KB gzip,
   enforced by `npm run build` (`scripts/size-check.mjs`; `npm run analyze`
   lists chunks). Before adding a dependency, check its size and import it
   only from lazy code. Heavy libs (markdown, highlighting, charts) must be
   dynamically imported.
4. Don't import from `src/mock/` except via the dynamic import in `main.tsx`.
5. Wrap store-reading components in `observer`, keep them small (rows are
   their own observers), and derive with `useComputed`, not effects.
6. Optimistic first: every user action updates the UI in the same frame.

## Verify visually

```
npm run build && npx vite preview &
PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/screenshots.mjs http://localhost:4173 /tmp/shots
PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/smoke.mjs     # optimistic writes, rollback, reload, keyboard
PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/rulesets-smoke.mjs   # rulesets UI (repo + org) in mock mode
PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/moderation-smoke.mjs # hide comments, edit history, delete issue (mock mode)
# site admin + org settings against a real backend (see the script header)
BGH_BACKEND=http://localhost:3000 npx vite --port 5174 &
PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers node scripts/admin-smoke.mjs http://localhost:5174 /tmp/admin-shots
```

Against the real backend (bgh-server on :3000 + `npm run dev`): seed it with
`node web/scripts/seed-real.mjs` (accounts via `bgh admin`, then REST + git
push), then `node web/scripts/real-smoke.mjs http://localhost:5173` drives
the issues UI and verifies every write through REST. Both need
`DATABASE_URL` and `BGH_BIN`; the smoke test signs in as `ada` through the
login form.

## Feature-local sync code

Synced models used only by lazy pages may keep their selectors and mutations
next to the feature instead of in `sync/selectors.ts` / `sync/mutations.ts`
(which load on first paint). Projects do this in `sync/projects.ts`. Child
models whose scope depends on a parent row (project fields/views/items) get
their scope through the optional `lookup` argument of the schema's `scope`
function.
