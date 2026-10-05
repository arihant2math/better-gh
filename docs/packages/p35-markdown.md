Integration: ready
P35 Markdown rendering parity: client renderer matches comrak (alerts, footnotes, anchors, refs, autolinks, gemoji, math), lazy math/Mermaid/highlighting, task toggling, camo image proxy.

# P35 Markdown rendering parity (client and server) — status

Branch `bgh/p35-markdown`. Scope: `docs/PHASE4_PLAN.md` §P35 (no §5 quick
fixes are assigned to P35). Evidence: `docs/AUDIT.md` (client renderer
weaker than comrak, autolinks never applied, task lists read-only, images
not lazy or proxied). Migrations: none (range 4700–4799 unused).

## Parity corpus

`testdata/markdown/*.md` + server snapshots `*.html` (README there):
alerts, footnotes, heading anchors, references (`#n`, `GH-n`,
`owner/repo#n`, mentions, SHAs, autolinks, instance URL shortening), emoji,
task lists, math markers, images. `bgh_core::markdown::tests::golden_corpus`
pins the server output (`BGH_UPDATE_GOLDEN=1` to refresh);
`web/src/ui/markdown/render.golden.test.ts` (jsdom) asserts the client
renders the same DOM after canonicalization (sorted attributes, `rel` /
`target` dropped, whitespace-only text dropped).

## Server (`bgh-core`)

* `markdown.rs`: comrak `math_dollars` + `math_code` (markers
  `<span data-math-style="inline|display">`, ```` ```math ```` →
  `<pre lang="math">`), `tasklist_classes`; scan adds gemoji `:shortcodes:`
  (`<g-emoji class="g-emoji" alias>`, always, also with references off),
  `GH-123`, repository autolinks (longest prefix first, case-insensitive,
  `class="autolink"`), and autolinked instance URLs shortened to `#12`,
  `owner/repo#12`, `#12 (comment)`, `abc1234`, `owner/repo@abc1234`.
  Footnote links point at the `user-content-` ids. Images get
  `loading="lazy" decoding="async"`; external `src` → camo.
* New public items (additive): `AutolinkRule`, `load_autolinks(db, ids)`
  (batched), `repo_autolinks(db, id)`, `RenderContext::autolinks` +
  `with_autolinks`, `emoji(name)`, `emoji_names()` (for P34's `/emojis`).
* Shared emoji table: `web/src/ui/markdown/emoji.json` (1913 gemoji
  shortcodes from the MIT `gemoji` package, `web/scripts/gen-emoji.mjs`),
  `include_str!`-ed by bgh-core and lazy-loaded by the web client.
* `camo.rs`: per-instance HMAC key `{data_dir}/camo.key` (created on
  first start, `AppState::new` calls `camo::init`), `sign` / `url` /
  `verify` / `is_external`, process-global switch fed by site settings.
* Site setting section `markdown` `{ image_proxy: bool }` (default on);
  admin UI switch under Site settings → Markdown.
* Callers passing autolinks: issue/PR and comment `body_html`/`body_text`
  (bgh-issues `json.rs`, one batched query, only when html/text is
  requested), releases (bgh-releases), commit comments (bgh-core),
  notification emails (bgh-notify).

## Endpoints

| Endpoint | Crate | Notes |
|---|---|---|
| `GET /_bgh/repos/{o}/{r}/autolinks` | bgh-repos | read access (404 otherwise); `[{key_prefix, url_template, is_alphanumeric}]`, longest prefix first |
| `POST /_bgh/render/code` | bgh-repos | `{blocks:[{lang, code}]}` → `{blocks:[{language, lines}\|null]}`; ≤ 50 blocks (422), syntect via new `bgh_git::highlight::highlight_lang` |
| `GET /_bgh/camo/{hmac}/{hex-url}` | bgh-uploads | signature check, SSRF guard (`ssrf::Policy`, same allow-list as webhooks), ≤ 3 redirects each re-checked, 10 s, ≤ 5 MiB, image types only; `nosniff` + sandbox CSP; 404 on any failure or when disabled |
| `POST /_bgh/camo/sign` | bgh-uploads | `{urls:[…]}` (≤ 100) → `{enabled, urls:{url: proxied}}`; non-external URLs / proxy off map to themselves |

## Web

* `ui/markdown/render.ts`: marked extensions for footnotes and math;
  renderer overrides for headings (comrak anchorizer port), alerts,
  `<pre lang>` code, task lists; own DOMPurify instance (its hooks must not
  leak into Mermaid's); DOM pass with `scan.ts` (port of the server scan)
  for references/autolinks/emoji and URL shortening. External images are
  held back in `data-canonical-src` until signed.
* `ui/markdown/enhance.ts` (after render, nodes claimed before awaiting):
  highlighting via `/_bgh/render/code` (cached), temml (MathML; KaTeX is
  76 KB gzip, over the 60 KB lazy cap — temml is KaTeX-syntax compatible,
  59 KB), Mermaid (`securityLevel: strict`, theme follows color scheme),
  camo signing.
* `ui/Markdown.tsx`: loads the render chunk + emoji JSON (`?raw`, 15 KB
  gzip) in parallel; fetches repo autolinks once per repo per session;
  `onSourceChange` enables task checkboxes and rewrites the nth task
  (`tasks.ts`; refuses when rendered/source counts differ); `#anchor`
  clicks scroll to `user-content-` ids.
* `pages/issues/Timeline.tsx`: one-line change passing the existing
  `onEdit` (author or write access) as `onSourceChange`, so ticking a box
  saves through the optimistic issue/comment edit mutation (hotspot file:
  additive prop only).
* Mock backend: the three `/_bgh` endpoints above.
* Bundle: initial JS +0.4 KB. `size-check.mjs` budgets chunks reachable
  only through the Mermaid entry at 150 KB gzip (documented in
  `web/README.md`); everything else keeps the 60 KB cap.

## Verification

* `cargo test -p bgh-core` (golden corpus, camo, emoji/references),
  `-p bgh-repos --test it markdown` (autolink rules endpoint, `body_html`
  of issues and comments with autolinks/emoji/GH-n, `/render/code`),
  `-p bgh-uploads --test it camo` (body_html rewrite, proxy, forged
  signature, non-image, oversized, redirects, signing, site setting off).
* vitest: corpus parity, `scan`, `tasks`, render behaviour, admin settings
  form.
* Playwright (mock mode): alert, emoji, GH-n, autolink, heading anchor,
  footnotes, highlighted rust block, MathML, Mermaid SVG render; temml and
  mermaid chunks requested only after a body containing them; ticking a
  task checkbox updates the issue body source.

## Known gaps

* GraphQL `bodyHTML` (bgh-graphql `render_html`, sync) does not load
  autolinks yet.
* Server `body_html` keeps soft line breaks (comrak `hardbreaks` off) while
  the client renders them as `<br>` like GitHub comments; the corpus avoids
  soft breaks.
* Repeated references to one footnote get a single back-reference on the
  client (comrak numbers `fnref-1-2`).
* `<picture>`/`<video>` external sources are not proxied (images only).
