# Package F3: issues-web (`web/`)

Status: **complete**. Branch `bgh/issues-web`, merged with the integration
branch (all packages, incl. accounts' `/_bgh/auth` + CSRF). Verified against
a real `bgh-server` + Postgres (seeded through the REST API and git push,
signed in through the login form) and in mock mode.

## What's in the UI

| Area | Where | Notes |
|------|-------|-------|
| Issue list | `pages/issues/IssueListPage.tsx`, `IssueRow.tsx` | Pinned issues as cards (unpin inline), Labels/Milestones links with counts, **New issue** (`⇧C`) → template chooser; rows show sub-issue progress (`2 / 5`) and a lock marker. Filter query (`?q=`) was already URL-synced; milestone/label links elsewhere deep-link into it. `g l` / `g m` go to labels / milestones. |
| Timeline | `pages/issues/Timeline.tsx`, `timelineGroups.ts` | Every `IssueEvent` type with GitHub wording and Octicons: labeled/unlabeled and assigned/unassigned grouped per actor (“added a b and removed c labels”, “self-assigned this”), milestoned/demilestoned, renamed (strike-through), closed as completed / not planned / duplicate (+ commit), reopened, merged, referenced (commit), cross-referenced (state icon + title; `owner/repo#n` for other repos or unreadable ones), locked (with reason) / unlocked, mentioned, pinned/unpinned, transferred (from repo), sub_issue_added/removed, parent_issue_added/removed, review_requested/removed, ready_for_review, convert_to_draft, head_ref_force_pushed. `subscribed` is hidden like on GitHub. |
| Comments | `Timeline.tsx` | Edit (author or write), delete with confirmation, copy link (`#issuecomment-ID` anchors), copy text, Author + association badges, locked-conversation notice (non-collaborators get no composer). Close button with reason menu (completed / not planned) or Reopen. |
| Reactions | `pages/issues/Reactions.tsx`, `sync/viewerReactions.ts` | Picker (8 reactions, keyboard ←/→) + pills on the issue and every comment; counts and “you reacted” are optimistic (overlay on the row + local viewer set, reverted on failure). |
| Markdown editor | `components/editor/MarkdownEditor.tsx`, `format.ts`, `caret.ts` | Write/Preview, toolbar + `⌘B/⌘I/⌘E/⌘K`, list continuation on Enter, `@login` and `#number`/title autocomplete from the local store (popup at the caret; ↑/↓, Enter/Tab, Esc). Used by comments, issue body, new issue, milestone description. |
| Sub-issues | `pages/issues/SubIssuesPanel.tsx` | Under the description: progress, add existing (picker, “will be moved” for issues with a parent → `replace_parent`), create new (creates the issue, then links it), remove, reorder by drag, ↑/↓ buttons or `Alt+↑/↓`. Children from other repos are fetched via REST for display. Parent shown in the sidebar (removable). |
| Lock / pin / transfer | `pages/issues/IssueActions.tsx` | Sidebar actions + command palette; `⇧L` lock/unlock (dialog with reason), `⇧P` pin/unpin (3-pin limit checked client-side). Transfer dialog lists same-owner repos you can write to (never private→public) and navigates to the new location. |
| Pickers | `IssueSidebar.tsx`, `ui/Menu.tsx` | “assign yourself”, create a label from the label picker (`SelectPanel.onCreate`), Edit labels / Manage milestones footers, milestone picker shows open milestones + “Clear milestone”, milestone links to its page. |
| New issue | `pages/issues/new/NewIssuePage.tsx`, `issueForm.ts` | `/issues/new/choose`: templates, forms, contact links, blank issue (skipped when there are no templates); template parse errors shown. `/issues/new[?template=&title=&body=&labels=&assignees=&milestone=]`: markdown templates prefill title/body/labels/assignees; issue forms render markdown/input/textarea (`render` → code block)/dropdown/checkboxes with required validation and submit GitHub’s `### Label` markdown. Triage users get assignee/label/milestone pickers. Optimistic: back to the list instantly, then opens the issue once numbered. |
| Labels | `pages/labels/LabelsPage.tsx`, `components/labels/ColorPicker.tsx` | Search (`?q=`), sort (`?sort=`), open-issue counts linking to the filtered list, inline create/edit with live preview, hex input / random / preset swatches, duplicate-name check, delete with confirmation (removes it from issues optimistically). `n`, `j/k`, `e`, `/`. |
| Milestones | `pages/milestones/*` | List with Open/Closed tabs (`?state=`), sort (`?sort=`), progress bars, due / overdue, close/reopen/delete; detail page = header (progress, due, description) + the filterable issue list of the milestone; create/edit form (date input, markdown description, close/reopen, delete). `n`, `j/k`, `Enter`, `e`. |

All writes go through `sync/mutations.ts` (overlay ops + REST request):
`lockIssue`, `unlockIssue`, `setPinned`, `transferIssue` (not optimistic:
the row changes scope), `addSubIssue`, `removeSubIssue`, `moveSubIssue`,
`toggleReaction`, `createLabel`/`updateLabel`/`deleteLabel`,
`createMilestone`/`updateMilestone`/`deleteMilestone`, `createIssue` with a
milestone. The mock backend (`src/mock/server.ts`) implements every one of
these endpoints plus issue templates; its seed adds a deterministic
“Timeline showcase” issue in `acme/api` (every event type, sub-issues, pins,
a locked issue). Mock state version bumped to 4.

## Backend changes (small, additive)

* `bgh-issues`: `GET /_bgh/repos/{o}/{r}/issues/{n}/viewer-reactions` and
  `DELETE /_bgh/repos/{o}/{r}/issues/{n}/reactions/{content}` /
  `…/issues/comments/{id}/reactions/{content}` (GitHub’s REST API can only
  delete a reaction by id, which the client doesn’t have). Synced issue rows
  carry `subIssueIds` (ordered). New `cross-referenced` events store
  `source_number`, `source_repository`, `source_is_pull_request`; deltas
  expose them and the sub/parent issue number + repository.
* `bgh-core::sync::shapes`: bootstrap/partial issue rows now include
  `activeLockReason`, `parentId`, `subIssueIds`, `pinned` (correlated
  subqueries on PK/indexed columns), and issueEvent data includes the same
  keys as `bgh_issues::json::event_sync_json` (lock reason, sources, sub /
  parent issues, transferred-from). Before, partial-loaded events lacked
  them.
* Docs: `SYNC_PROTOCOL.md` (§3 issue + event data, §10 endpoints),
  `docs/packages/issues.md`, `docs/FRONTEND.md`.
* Tests: `crates/bgh-issues/tests/web_client.rs` (viewer reactions, sync
  row fields in deltas and bootstrap, partial event data == deltas).
* `bgh-sync` bootstrap shape test expects the new issue fields.
  (The default-labels test interference I hit before the integration pass
  was fixed the same way on the integration branch; I took its version.)
  `cargo test --workspace`: everything I touch is green; 3 bgh-accounts
  rate-limit tests (`users::authenticated_user_and_patch`,
  `users::api_root_and_rate_limit`,
  `sso_avatars_ratelimit::rate_limits_are_enforced`) fail on the merged
  integration branch itself (`GET /rate_limit` → 404 “Rate limiting is not
  enabled.” from bgh-admin, no `X-RateLimit-*` headers) — unrelated to this
  package's diff (no accounts/admin/config changes here).

Shared web changes: `ui/Menu.tsx` (`SelectPanel` `onCreate`/`createLabel`/
`footer`; filter input focused in an effect — `autoFocus` ran while the
popover was still hidden, so pickers dropped typed input), `ui/icons.ts`
(more Octicons), `sync/overlay.ts` (`Patch` allows array patches on optional
array fields), `app/TopBar.tsx` breadcrumbs for labels/milestones/new issue,
`app/routes.ts`.

## Verification

* `npm run typecheck && npm run lint && npm test && npm run build` green;
  initial JS 121 KB gzip (budget 150), largest lazy chunk 24.5 KB.
  New unit tests: editor transforms, issue-form state/markdown, milestone
  due dates, timeline grouping.
* `web/scripts/seed-real.mjs` seeds a running server (accounts via
  `bgh admin`, org `acme`, repos `api`/`web`, labels, milestones, ~25
  issues, comments, reactions, templates/forms pushed with git, a showcase
  issue hitting every event type, pins, a lock, a transfer, commit
  references incl. `Fixes #n`).
* `web/scripts/real-smoke.mjs` (Playwright, real server via `npm run dev`):
  filter URL + server parity, all timeline event types, reactions
  add/remove, @mention autocomplete, comment create/edit/delete, lock with
  reason/unlock, unpin/pin, sub-issue add/reorder/remove, label
  create/rename/delete, milestone create/close/delete, issue form → markdown
  body + template labels, transfer — each checked in the UI and then on the
  server via REST. All 52 checks pass on the integrated server (real login,
  CSRF enforced), repeatable on the same data.
* `scripts/smoke.mjs` (mock) extended: showcase timeline, reactions, `⇧L`
  lock, `Alt+↓` reorder, label create + rollback on 422, milestone create,
  issue form submit. `scripts/screenshots.mjs` adds showcase, labels,
  milestones, milestone, chooser and form shots (light + dark checked).

## Known gaps / notes for integration

* **Image paste/drop upload:** there is no upload endpoint on the server
  (no package provides attachments). The editor intercepts pasted/dropped
  files and explains that uploads aren’t supported (link an image URL
  instead). Needs a `POST /_bgh/uploads` (or similar) + storage.
* `real-smoke.mjs` signs in through the login form; on a server without
  `/_bgh/auth` it falls back to minting a session row and stubbing
  `/_bgh/boot`.
* The issue body has no author association in the sync model, so its header
  shows “Author” only.
* Sub-issue children in other repos render from REST (title/state) and
  don’t update live unless that repo is synced.
* Git push in this container printed “push negotiation failed” (git
  transport package) but the push applied.
