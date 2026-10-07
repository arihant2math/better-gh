Integration: landed
Diff viewer: server-side syntax highlighting, context expansion, image/rich diffs, file actions, inline check annotations, generated-file collapse; full gate green after merging the integration branch.

# P37 — Diff viewer

Branch `bgh/p37-diff-viewer`. Scope: `docs/PHASE4_PLAN.md` §P37 (no §5
quick fixes are assigned to P37). No migrations (4900–4999 unused).

## Endpoints

| Endpoint | Crate | Notes |
|---|---|---|
| `GET /_bgh/repos/{o}/{r}/blob-lines/{commitish}?path=&start=&end=&hl=1&text=0` | bgh-repos `browse/lines.rs` | Lines of a file at a commit. `{commitish}`: a commit SHA, `{base}...{head}` (merge base of two SHAs = a PR's old side) or a ref. `start`/`end` 1-based inclusive (default whole file, capped at 20 000 lines, clamped to the file). `hl=1` adds highlighted HTML (`html`) sliced from the shared, Redis-cached `/_bgh/render/blob` highlighter (keyed by blob SHA); `text=0` drops plain `lines`. Binary/LFS content: metadata only (`binary`, `image`, `mime`, `size`, `raw_url` pinned to the resolved commit). SHA forms are immutable (`max-age=31536000, immutable` + request ETag/304); refs get the short TTL. `422` missing `path`; `404` unknown repo/commit/path, directory, submodule, non-SHA `a...b` |
| `GET /_bgh/repos/{o}/{r}/commits/{sha}/annotations` | bgh-pulls `diffview.rs` | All check-run annotations of a commit in one query (`check_runs_sha_idx` + `check_run_annotations_run_idx`), ordered by path/line, ≤ 1000: `[{check_run_id, check_run_name, path, start_line, end_line, start_column, end_column, annotation_level, title, message, raw_details}]`. `404` for non-SHA / no access |

Both are documented in `docs/SYNC_PROTOCOL.md` (private `/_bgh` table) and
mocked in `web/src/mock/pulls.ts` (`npm run dev:mock`).

## Web

* `components/diff/useDiffExtras.ts`: per-file extras, enabled by a new
  optional `source: DiffSource` prop on `DiffView` / `DiffViewer`
  (`{owner, repo, oldRef, newRef, annotations?, editRef?}`). Without a
  source, DiffView behaves as before.
  * **Highlighting**: when a file header renders (i.e. the file is near the
    viewport in the virtual list) both sides are fetched once
    (`blob-lines … hl=1&text=0`, immutable cache) and mapped onto diff lines
    by line number (`highlight.ts` `lineHtml`; deletions → old side, the rest
    → new side). A sample of lines is compared with the patch text
    (`highlightMatches`) and highlighting is dropped on mismatch. Files with
    > 5000 changed lines, binaries and blobs the server won't highlight
    (> 512 KB, unknown language) render as plain text. All highlighting is
    server-side: no tokenizer in any JS chunk.
  * **Context expansion** (`expand.ts`, pure): gaps above, between and below
    hunks; "expand up" / "expand down" (20 lines) and "expand all", in
    unified and split views. Lines come from `blob-lines` ranges on the new
    side (old numbers = new + offset), kept per file; fully revealed gaps
    merge adjacent hunks and hunk headers are recomputed. The tail gap uses
    the line count from the highlighting response.
  * **Binary and image diffs** (`BinaryDiff.tsx`, lazy chunk): sizes of
    both versions; images get 2-up, swipe and onion-skin views from the raw
    URLs at both commits. PR files without a patch and with no line changes
    (GitHub's shape for binaries) are now treated as binary; empty files /
    mode changes say so.
  * **Rich Markdown diff** (`RichDiff.tsx`, lazy chunk): source/rich toggle
    in the header of `.md` files; before/after rendered side by side (or one
    of them) with the shared `ui/Markdown` renderer — the seam for P35,
    which upgrades that renderer.
  * **File actions** in the header: copy path, "View file" at the new
    commit, "Edit file" on `editRef` (PR: open, same-repo head, viewer has
    write+).
  * **Check annotations** inline below the annotated end line (new side),
    with level icon, run name, title, message and raw details; annotations
    outside the visible lines go to the end of the file. The fetch is
    versioned by the completed check runs of the head commit, so it refreshes
    when CI finishes.
  * **Generated files** (`highlight.ts` `isGenerated`: lockfiles, `*.min.js`,
    `*.pb.go`, `__generated__/`, snapshots…) are collapsed by default in the
    PR Files tab and badged "Generated". `.gitattributes`
    `linguist-generated` is P78's hook: replace the heuristic call in
    `FilesTab.tsx` once it lands.
* `pages/pulls/FilesTab.tsx` (additive): builds the `source`
  (`oldRef = base...head`), default-collapses generated files, binary
  detection fix. `pages/commits/CommitPage.tsx`: single-parent commits pass
  a source; `pages/pulls/ComparePage.tsx`: same-repository compares with
  the full commit list pass `merge_base_commit` → last commit (highlighting,
  expansion, images, file actions).
* Fixed a CSS collision in `DiffViewer.module.css`: the empty-state `.empty`
  rule (32px padding) also hit empty split cells, making one-sided split
  rows 64px tall. Renamed to `.emptyState`.
* Bundle: initial JS unchanged (143.3 KB gzip); RichDiff/BinaryDiff are
  separate lazy chunks, the rest lives in the pull/commit route chunks.

## Shared-code changes (additive)

* `bgh-repos/src/browse/render.rs`: highlighting factored into
  `pub(crate) highlighted_blob()` (same cache key, reused by `blob-lines`).
* `web/src/ui/icons.ts`: `FoldUpIcon`, `FoldDownIcon`.
* `web/src/api/{types,endpoints}.ts`: `BlobLines`, `CommitAnnotation`,
  `getBlobLines`, `listCommitAnnotations`.

## Tests

* `crates/bgh-repos/tests/it/diff_lines.rs`: ranges, clamping, `hl`/`text`,
  merge-base spec, refs vs immutable caching, ETag 304, binary/image
  metadata, 422/404s, private repo.
* `crates/bgh-pulls/tests/it/diffview.rs`: annotations across runs, other
  commits excluded, ordering and shape, 404s.
* Vitest: `components/diff/expand.test.ts` (gaps, expand steps, controls,
  merging, header recomputation, unloaded lines),
  `components/diff/highlight.test.ts` (line→token mapping, mismatch
  detection, generated/markdown detection).
* Playwright smoke against a real server (seeded PR: 1000-line file, PNG
  change, Markdown change, 5000-line added file, failing check run with an
  annotation on line 900): highlighting present; "expand up" turns
  `@@ -97,7 +97,7 @@` into `@@ -77,27 +77,27 @@`; "expand all" reaches line
  1; the annotation row sits directly below line 900; image diff swipe and
  onion views with both images loaded; rich diff renders both versions;
  View/Edit links; no page errors. Same flows checked in mock mode.
* Scroll profile (headless Chromium, 5000-line highlighted diff, 300
  frames scrolling 90 000 px): every frame 16.7 ms, unified and split.

## Known gaps / TODOs

* Expansion and highlighting use the new side only for context lines; a
  whitespace-insensitive diff (`?w=1`) still maps by line number, which is
  correct since both sides' numbers come from the same blobs.
* Annotations attach to the new side only (GitHub does the same).
* Generated-file detection is filename-based until P78
  (`linguist-generated`).
