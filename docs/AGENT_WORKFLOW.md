# Agent workflow (phase 5: issues → PRs → review)

All work is tracked as GitHub issues on `arihant2math/better-gh` and lands in
`main` through pull requests. Every agent acts as the same GitHub user, so
ownership is expressed with labels, not assignees. Use the GitHub MCP tools
for every GitHub interaction.

## Operating principles (context discipline)

Inspired by the pi coding agent's minimalism: small context, few tools,
observable agents, state in files rather than in memory.

* **One agent, one issue, one short session.** A worker session owns one
  issue; it ends when its PR merges and is archived. Each review round is a
  fresh reviewer session. Long-lived sessions accumulate stale context.
* **GitHub is the shared memory.** Claims, progress notes, decisions and
  handoffs go into concise issue/PR comments; PR descriptions say what
  changed and how it was tested. No agent ever needs another agent's
  transcript, and coordinators read labels/PR state, not transcripts.
* **Read only what the task needs.** Start from the issue, `CLAUDE.md` and
  the specific doc section it points to. Don't load whole plans/audits;
  grep for the relevant part.
* **Few tools, loaded narrowly.** Load GitHub MCP tools with
  `ToolSearch("select:<exact names>")`, request minimal fields
  (`fields`, `minimal_output`, small `perPage`), and avoid broad list calls.
* **No hidden sub-agents by default.** Workers and reviewers do their work
  inline. If work genuinely needs parallelism, say so in the issue and the
  foreman spawns separate, visible sessions.
* **Terse outputs.** Summarize command output (test counts + failing
  names, `tail`/`grep` of logs); never paste full logs or diffs into
  context or comments.
* **CI is the shared gate.** The PR's CI runs the full workspace suite;
  reviewers rely on green CI for that and run targeted tests plus the
  viewport matrix themselves, rather than rebuilding everything.
* **Design questions go to GitHub Discussions** (category "Ideas", or an
  issue labelled `T-Docs` + `S-Blocked` if Discussions are unavailable),
  linked from the issue, so decisions are recorded once.

## Labels (`[letter]-[word]`)

| Prefix | Meaning | Values |
|---|---|---|
| `C-` | claim | `C-Claimed` — an agent is working on this issue (one agent per issue) |
| `T-` | type | `T-Bug`, `T-Feature`, `T-Polish`, `T-Perf`, `T-Docs`, `T-Test` |
| `A-` | area | `A-Frontend`, `A-Responsive`, `A-Backend`, `A-API`, `A-GraphQL`, `A-Git`, `A-Actions`, `A-Auth`, `A-Admin`, `A-Issues`, `A-Pulls`, `A-Projects`, `A-Packages`, `A-Security`, `A-Notify`, `A-Search`, `A-Sync`, `A-Ops` |
| `P-` | priority | `P-Critical`, `P-High`, `P-Medium`, `P-Low` |
| `S-` | PR status | `S-NeedsReview`, `S-ChangesRequested`, `S-Approved`, `S-Blocked` |
| `O-` | origin | `O-QA` (filed by a QA agent), `O-Audit` (from docs/AUDIT.md) |

Labels are created automatically the first time they are applied. Don't
invent new prefixes; add a value to this table in a PR if one is missing.

## Workers

1. **Claim:** pick an open issue without `C-Claimed` (the foreman usually
   assigns one). Immediately add `C-Claimed` (keep existing labels) and
   comment `Claimed by <session title>.` If you abandon it, remove the label
   and comment why.
2. **Branch:** `agent/<issue-number>-<slug>` from the latest `origin/main`.
3. **Build** per `CLAUDE.md` conventions with tests. Keep PRs focused on
   the issue; split large issues into several PRs if needed.
4. **Gate before opening the PR:** `cargo fmt --all --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test -p <touched crates>` (full `--workspace` if you touched
   `bgh-core`), and in `web/`: `npm run typecheck && npm run lint && npm test
   && npm run build`.
5. **Frontend changes must be tested on devices** (see below) — attach the
   viewport report summary to the PR body.
6. **Open the PR** against `main`: title `<scope>: <imperative summary>`,
   body with `Fixes #N`, what changed, how it was tested (commands, viewport
   matrix result), and screenshots described (paths in the branch under
   `web/test-results/` are not committed — describe what you checked). Label
   the PR `S-NeedsReview` plus the issue's `T-`/`A-` labels.
7. **Branches are deleted when done.** The `Branch cleanup` workflow
   deletes a PR's head branch when it merges and sweeps branches already
   merged into `main` daily (run it manually from the Actions tab or via
   `workflow_dispatch` any time). If you abandon work, delete your own
   branch (`git push origin --delete <branch>`) and say so on the issue.
   Coordinators delete their scratch branches when they finish.
8. **After review:** if the reviewer requests changes (`S-ChangesRequested`),
   fix, push, reply to each review comment (or push back with reasoning if
   you disagree), then set `S-NeedsReview` again. Keep your branch mergeable
   with `main` (merge `origin/main` in; never force-push a branch under
   review).

## Reviewers

A reviewer is a different agent session from the author.

1. Read the linked issue, the diff, and the PR description. Check out the
   branch locally.
2. Verify: correctness and edge cases, GitHub API compatibility (shapes,
   status codes), permissions/security, tests actually cover the change,
   performance (no N+1, no initial-bundle growth), code consistent with
   `docs/BACKEND_PATTERNS.md` / `docs/FRONTEND.md`.
3. Check CI is green (it runs the full gate). Run targeted tests for the
   touched crates/components and try the change yourself. For any UI
   change, run the viewport matrix yourself and look at the screenshots
   (Read the PNGs) — don't trust the author's description.
4. Submit a GitHub review (event `COMMENT`, since GitHub doesn't allow
   approving a PR opened by the same account) whose first line is
   `Verdict: APPROVE` or `Verdict: REQUEST_CHANGES`, with inline comments
   for concrete problems.
5. **Approve:** set `S-Approved`, make sure CI is green and the branch is up
   to date with `main` (update it if needed and re-run the gate), then merge
   with the **squash** method. The `Fixes #N` closes the issue; remove
   `C-Claimed` from it if still present.
6. **Request changes:** set `S-ChangesRequested`. The author (or a fixer
   agent if the author is gone) addresses it; the next review round can be
   done by any reviewer.
7. Nits don't block: approve and file a `T-Polish` issue instead.

## Device testing (frontend)

`web/scripts/viewport-matrix.mjs` screenshots routes against a running
server at a matrix of viewports and fails on layout problems:

* viewports: 360×740 (small phone), 390×844 (phone), 768×1024 (tablet
  portrait), 1024×768 (tablet landscape), 1280×800 (laptop), 1440×900,
  1920×1080, 2560×1080 (ultrawide 21:9), 1080×1920 (portrait monitor), plus
  a live-resize pass (1440 → 360 width in steps) on each route;
* automatic checks per viewport: no horizontal page overflow
  (`scrollWidth > clientWidth`), no element overflowing its scroll
  container unintentionally, no text clipped by `overflow:hidden` without
  ellipsis, tap targets ≥ 32px on touch viewports, nothing off-screen that
  should be reachable, no console errors;
* light and dark themes.

Usage: `node web/scripts/viewport-matrix.mjs --base http://localhost:3000
--routes /,/acme/api,/acme/api/issues --out web/test-results/viewports`
(or `npm run viewports -- …` in `web/`; add `--login ada:password123` for
signed-in pages, `--mock` against the mock backend, `--allow <file>` to
baseline known issues). It prints a summary table and writes PNGs +
`report.json`; checks and options are documented in `web/README.md` →
"Device testing (viewport matrix)".
(`PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers`; never `playwright install`.)

## QA agents

QA agents continuously exercise the running app (real backend, seeded
data) like a demanding user: every page, at every viewport, resizing,
keyboard-only, dark mode, long names/titles, empty states, huge lists,
slow network. For each problem: search existing issues first (avoid
duplicates; comment on an existing one with new evidence instead), then
file an issue with labels `O-QA`, `T-Bug` (or `T-Polish`), the `A-` area,
`A-Responsive` for layout/resize problems, a `P-` priority, exact steps,
viewport size, theme, expected vs actual, and the route.
