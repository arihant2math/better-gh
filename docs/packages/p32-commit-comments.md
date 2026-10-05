# P32 commit-comments — status

**Done.** Branch `bgh/p32-commit-comments`. Scope: `docs/PHASE4_PLAN.md` §P32
(no §5 quick fixes are assigned to P32).

## Endpoints (bgh-repos `commit_comments.rs`)

| Endpoint | Notes |
|---|---|
| `GET /repos/{o}/{r}/commits/{sha}/comments` | oldest first, paginated; `{sha}` may be any commit-ish (short SHA, branch) |
| `POST /repos/{o}/{r}/commits/{sha}/comments` | `body` (422 missing_field), `path`, `position` or `line`. The other value is computed from the commit's diff against its first parent (`position` = line index below the first `@@`, hunk headers counted). A `position` that is a deleted line / header, or a `path` not in the diff → 422. A `line` outside the diff is kept without `position` (GitHub accepts it). 201 + `Location`. Read access suffices (like GitHub); archived → 403 |
| `GET /repos/{o}/{r}/comments` | every commit comment of the repository |
| `GET`/`PATCH`/`DELETE /repos/{o}/{r}/comments/{id}` | author or write access may edit/delete (403 for other readers); delete removes its reactions |
| `GET`/`POST /repos/{o}/{r}/comments/{id}/reactions`, `DELETE …/reactions/{reaction_id}` | `content` filter; 201 new / 200 existing; own reactions (admins: any) |

Bodies honour `application/vnd.github.{raw,text,html,full}+json`.
`commit.comment_count` in commit JSON (list, single, branches) is now the
real count (one batched query per render). `GET /commits/{ref}/pulls`
already existed in bgh-pulls (no change needed).

## Webhook / notifications / activity

* New event `Event::CommitCommentCreated { repo_id, comment_id, actor_id,
  commit_author_id }` (`commit_author_id` resolved from the commit author's
  verified email at creation time).
* Webhook `commit_comment` `created` (`comment`, `repository`, `sender`):
  `bgh-notify/src/payloads/commit_comments.rs` + one arm in `payloads/mod.rs`;
  removed from `NOT_PRODUCIBLE_YET` in the coverage test.
* Notifications (`fanout.rs::commit_commented`): subject `Commit` (thread key =
  first comment id of `(repo, sha)`, `subject_key` = SHA, title = commit
  summary), commit author → `author`, mentions → `mention`/`team_mention`,
  participants and watchers; `latest_comment_url` → `/repos/{o}/{r}/comments/{id}`
  (`threads.rs`). Email `EmailKind::CommitComment` (threaded per commit).
* Activity `CommitCommentEvent` (`bgh-search/src/activity/record.rs`).
* Admin stats `total_commit_comments` is a real count.

## Shared-code changes (additive)

* `bgh-core/src/commit_comments.rs` (new): `CommitCommentRow`, `BodyFormat`,
  `render` (batch users / author_association / reaction rollups; used by REST,
  webhook and activity).
* `bgh-core/src/events.rs`: variant `CommitCommentCreated`.
* `bgh-core/src/node_id.rs`: `NodeType::CommitComment`.

## Migrations

`4400_commit_comments.sql`: `commit_comments` table, indexes
`(repo_id, commit_id, id)`, `(repo_id, id)`, `(user_id)`. Reactions reuse
`reactions` (`subject_type = 'commit_comment'` was already allowed).

## Web

* `web/src/api/commitComments.ts` (REST client + cache keys).
* `web/src/pages/commits/CommitComments.tsx` (+ module CSS): thread at the
  bottom of the commit page (rendered as the diff's footer row) and inline
  line threads in the diff (reuses `Review.module.css`; `DiffViewer` got
  optional `annotations`/`footer`, `DiffAnnotations.sides` limits the gutter
  "+" to new-side lines). Edit/delete menu, reply box, reactions toggle.
* Mock: `web/src/mock/commitComments.ts` (+ test), seeded comments on each
  repo's default-branch head.
* Playwright smoke (mock mode): general + line comment posted, both visible
  after a full reload, reaction toggled.

## Tests

* `crates/bgh-repos/tests/it/commit_comments.rs`: shapes, line↔position
  mapping, short SHAs, Link pagination, media types, edit/delete permissions,
  reactions, private-repo 404s, `comment_count`.
* `crates/bgh-notify/tests/it/commit_comments.rs`: webhook delivery, author +
  mention notifications in one thread, reply notifies participants,
  `CommitCommentEvent`.
* Unit: diff position mapping (`bgh-repos`), html→text (`bgh-core`).

## Known gaps

* No sync model for commit comments (the web client fetches them over REST).
* `GET /commits/{sha}` with a full SHA is served `immutable`; a cached copy
  can show a stale `comment_count`.
* The REST shape has no "viewer reacted" flag; the web UI highlights only
  reactions toggled in the current session (toggling stays correct).
* No GraphQL `CommitComment` type yet.
