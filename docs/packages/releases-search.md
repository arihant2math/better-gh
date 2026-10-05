# B6 releases-search — status

Branch `bgh/releases-search`. Crates `bgh-releases`, `bgh-search`.
Migrations `0600_releases.sql`, `0700_search.sql`, `0701_code_index.sql`,
`0702_code_index_gc.sql`, `0710_activity.sql`.

## Status: complete

All scope items of WORKPLAN B6 are implemented with integration tests
(`cargo test -p bgh-releases -p bgh-search`: 9 + 13 integration tests, plus
unit tests). The 100k-issue palette benchmark is an `#[ignore]` test.

## Releases (`bgh-releases`)

Endpoints (all under `/api/v3` unless noted):

| Method | Path | Notes |
|---|---|---|
| GET/POST | `/repos/{o}/{r}/releases` | list (drafts only for writers, newest first, paginated); create (201 + `Location`) |
| GET | `/repos/{o}/{r}/releases/latest` | explicit `make_latest` mark, else newest published non-prerelease; `make_latest:false` excluded |
| GET | `/repos/{o}/{r}/releases/tags/{tag}` | published releases only |
| GET/PATCH/DELETE | `/repos/{o}/{r}/releases/{id}` | PATCH `draft:false` publishes (sets `published_at`, creates tag, emits `ReleasePublished`); DELETE keeps the tag |
| POST | `/repos/{o}/{r}/releases/generate-notes` | "What's Changed" (merged PRs in `prev..head`), "New Contributors", "Full Changelog" compare link |
| GET/POST | `/repos/{o}/{r}/releases/{id}/assets` | list; upload (`?name=&label=`, raw body, streamed) |
| POST | `{base}/api/uploads/repos/{o}/{r}/releases/{id}/assets` | uploads host advertised in `upload_url` (`…/assets{?name,label}`) |
| GET/PATCH/DELETE | `/repos/{o}/{r}/releases/assets/{id}` | JSON, or the content with `Accept: application/octet-stream` (streamed, `Content-Disposition`, counts the download) |
| GET | `/{o}/{r}/releases/download/{tag}/{name}` (web) | `browser_download_url`, counts downloads |
| GET/POST | `/repos/{o}/{r}/releases/{id}/reactions` | `?content=` filter; 201 new / 200 existing; release-allowed contents only |
| DELETE | `/repos/{o}/{r}/releases/{id}/reactions/{reaction_id}` | own reaction or repo admin |

Behavior notes:

* Tags: a published release whose tag doesn't exist creates
  `refs/tags/{tag}` at `target_commitish` (default branch when omitted) and
  emits `Event::Push` for the new tag; unknown target → 422
  `target_commitish invalid`. Drafts create the tag on publish.
* `make_latest`: `"true"` (default on publish) clears other explicit marks,
  `"false"` excludes the release, `"legacy"` = computed by date.
* Media types `application/vnd.github.{html,text,full}+json` add
  `body_html` / `body_text`. `reactions` rollup is included when non-empty,
  `mentions_count` when the body mentions users. Asset `digest` is
  `sha256:…`.
* Asset storage: `storage::AssetStorage` trait (content-addressed by
  SHA-256, idempotent `put_file`, streaming `open`, `delete`);
  `DiskStorage` under `{data_dir}/files/release-assets/{sha[..2]}/{sha}`.
  Uploads are spooled to `{data_dir}/files/tmp` while hashing (2 GiB max),
  then moved. Blobs are deleted when no asset references them (asset or
  release delete). An S3 backend can be installed via
  `state.with_extension(SharedStorage(..))`.
* Writes record sync actions (`release`, `release_asset` models in
  `repo:{id}`) and audit entries (`release.create|update|destroy`).
* Events emitted: `ReleaseCreated`, `ReleasePublished`, `ReleaseUpdated`
  (also on asset upload), `ReleaseDeleted`, `Push` (tag creation).

## Search (`bgh-search`)

| Path | Backend |
|---|---|
| `/search/issues` | `issues.search` tsvector (`websearch_to_tsquery`), comment tsvector index for `in:comments`; qualifiers `is:`/`type:` (issue, pr, open, closed, merged, unmerged, draft, locked, unlocked, public, private, archived), `state:`, `reason:`, `author:`, `assignee:`, `mentions:`, `commenter:`, `involves:`, `review-requested:`, `reviewed-by:`, `label:` (comma = OR, repeated = AND), `milestone:`, `no:label/milestone/assignee`, `repo:`/`org:`/`user:` (repeated = OR, negatable), `in:title,body,comments`, `created:`/`updated:`/`closed:`/`merged:` ranges, `comments:`, `reactions:`, `interactions:`, `head:`, `base:`, `draft:`, `archived:`, `language:`, `@me`; sort `created`, `updated`, `comments`, `reactions`, `reactions-*`, `interactions` |
| `/search/repositories` | name trigram + weighted name/description tsvector; `in:name,description,topics`, `user:`, `org:`, `repo:`, `language:`, `topic:`, `topics:`, `stars:`, `forks:`, `size:`, `created:`, `pushed:`, `updated:`, `is:public/private/template/archived/fork`, `fork:true/only` (forks excluded by default), `archived:`, `template:`, `license:`; sort `stars`, `forks`, `help-wanted-issues`, `updated` |
| `/search/users` | login/name/email trigram; `type:`, `in:`, `repos:`, `followers:`, `created:`, `location:`, `language:`, `fullname:`; sort `followers`, `repositories`, `joined`; suspended users hidden |
| `/search/commits` | `commit_index` (default branches) tsvector; `author:`, `committer:`, `author-name:`, `committer-name:`, `author-email:`, `committer-email:`, `author-date:`, `committer-date:`, `merge:`, `hash:`, `parent:`, `tree:`, `repo:`/`org:`/`user:`, `is:`; sort `author-date`, `committer-date` |
| `/search/labels` | `repository_id` + `q`, trigram/similarity score; sort `created`, `updated` |
| `/search/topics` | aggregated from readable repositories' topics; `repositories:` range |
| `/search/code` | trigram index over default-branch blobs: substring (case-insensitive), `"phrases"`, `/regex/` (validated with the `regex` crate, run as Postgres `~*`), `NOT`/`-`, `OR`, `repo:`/`org:`/`user:`, `language:`, `path:` (substring, globs, `/anchored`, `/regex/`), `extension:`, `filename:`, `size:`, `in:file,path`, `fork:`, `is:`; `text_matches` fragments + `line_numbers` |
| `/_bgh/search` (web) | palette: `{q, took_ms, issues[], repos[], users[]}`, prefix tsquery for issues, `owner/name` repo matching, `#n` with `repo=`; `org=<login>` limits issues/repos to that owner (unknown → empty; `repo=` wins); 3 queries in parallel |

Common: `{total_count, incomplete_results, items[] + score}` envelope,
`Link` header (`last` capped to the 1000-result window), 422 for missing
`q` (`code: missing`), unknown/unreadable users or repos in qualifiers
(GitHub's "cannot be searched" message), bad ranges, invalid regex, bad
sort, and pages beyond 1000 results. `application/vnd.github.text-match+json`
adds `text_matches` (issues: title/body, repos: name/description, users:
login/name, labels, commits: message, code: content). Code search runs
with a 10 s statement timeout; a timeout returns `incomplete_results: true`.

Permissions: every query filters by `bgh_core::perms::readable_repos`
(public repos + the caller's private repos computed with one UNION of
index lookups; everything for site admins; public only for tokens without
`repo`).

### Code index — why Postgres trigrams, not tantivy

Permission filtering lives in Postgres, a `gin_trgm_ops` index answers both
substring (`ILIKE`) and regex (`~*`) queries (tantivy is token based and
would need a separate n-gram field + regex post-filter), and the index stays
transactional with no extra on-disk state to back up or rebuild.
`code_blobs(sha, content)` is shared across repositories (forks cost only
`code_files` rows).

Indexing: listener `search.code_index` enqueues `search.index_repo
{repo_id}` on pushes to the default branch and `RepositoryCreated/Updated/
Forked` (when the default branch exists), debounced by 2 s and deduplicated
while pending (bursts of pushes coalesce; other crates' `drain_jobs` counts
are unaffected); the job walks the tree, diffs
`(path, blob_sha)` against `code_files`, reads only blobs not already
stored (≤ 384 KB, non-binary; larger/binary files are indexed by path
only), upserts files, deletes removed paths, GCs orphaned blobs, then
indexes commits `rev-list head ^previous_head` (≤ 10k per run; authors
matched by verified email). `RepositoryDeleted` sets the
`code_index_gc.pending` marker; the next index run deletes orphaned blobs
(`search.gc_code_blobs` forces a collection). Skips `node_modules/`; ≤ 100k files per repo.

### Palette benchmark (100k issues)

`cargo test -p bgh-search --test palette -- --ignored --nocapture` seeds 2k
users, 200 repos (10% private), 100k issues, then runs 20 queries × 9
rounds (after a warm-up round) anonymously and as a user with 5 private
repos. Debug build, 4-core container, local Postgres 16:

| caller | p50 | p95 | max |
|---|---|---|---|
| anonymous | 11.2 ms | 21.7 ms | 38.5 ms |
| user | 13.5 ms | 23.5 ms | 29.4 ms |

(Before the plan-choice probe below, p50 was ~7 ms but p95 ~50 ms: rare
prefix terms made the planner walk `issues_updated_idx` over all rows. A
bounded GIN probe (`LIMIT 500`) now picks between that index walk for
common terms and a bitmap scan + sort for rare ones.)

## Activity / Events API (`bgh-search::activity`)

Table `activity_events(type, actor_id, repo_id, repo_name, org_id, public,
payload, created_at)`, recorded by the `search.activity` event listener
with a payload snapshot; actor/org/repo name are rendered at read time.
`public` = repository public at event time (and still public when read).

| Path | Visibility |
|---|---|
| `/events` | public |
| `/repos/{o}/{r}/events` | repo readers |
| `/networks/{o}/{r}/events` | fork network (`source_id` root), readable repos |
| `/orgs/{org}/events` | public events of the org |
| `/users/{u}/events` | own private events when authenticated as `u`, else public |
| `/users/{u}/events/public` | public |
| `/users/{u}/events/orgs/{org}` | authenticated as `u` only; readable org repos |
| `/users/{u}/received_events[/public]` | events of watched/starred repos and followed users (excluding `u`'s own) |
| `/_bgh/feed?before=&limit=&org=` (web) | received + own, readable, cursor pagination (`next_before`); `org=<login>` keeps events of repos owned by that account |

Timelines are capped at 300 events (GitHub). Event mapping:

| Domain event | GitHub event |
|---|---|
| `Push` | `PushEvent` (≤ 20 commits, `size`; new branches list commits not on the default branch), `CreateEvent` (branch/tag), `DeleteEvent` |
| `RepositoryCreated` (non-fork) | `CreateEvent` (`ref_type: repository`) |
| `RepositoryForked` | `ForkEvent` (`forkee`) on the parent |
| `RepositoryStarred` | `WatchEvent` (`started`) |
| `RepositoryPublicized` | `PublicEvent` |
| `CollaboratorAdded` | `MemberEvent` (`added`) |
| `IssueOpened/Closed/Reopened/Edited` | `IssuesEvent` (with `changes` for edited; PR issues skipped) |
| `IssueCommentCreated` | `IssueCommentEvent` |
| `PullRequestOpened/Reopened/Closed/Merged` | `PullRequestEvent` (merge → one `closed` with `merged: true`) |
| `PullRequestReviewSubmitted` | `PullRequestReviewEvent` |
| `PullRequestReviewCommentCreated` | `PullRequestReviewCommentEvent` |
| `ReleasePublished` | `ReleaseEvent` (`published`) |

## Shared-code changes (all additive)

* `bgh-core/src/events.rs`: new `Event` variants `ReleaseCreated`,
  `ReleaseUpdated`, `ReleaseDeleted { tag_name }`, `RepositoryStarred`,
  `RepositoryForked { fork_id }`, `RepositoryPublicized`,
  `CollaboratorAdded { user_id }`, `PullRequestReviewCommentCreated
  { comment_id }` (+ `name()/repo_id()/actor_id()` arms). **Other packages
  should emit** `RepositoryStarred` (stars), `RepositoryForked` (forks),
  `RepositoryPublicized` (visibility → public), `CollaboratorAdded`,
  `PullRequestReviewCommentCreated` so the Events API records them; if a
  package already added an equivalently named variant, map it in
  `bgh-search/src/activity/record.rs`.
* `bgh-core/src/perms.rs`: `ReadableRepos` + `readable_repos(db, auth)`.
* `bgh-git/src/read.rs`: `GitRepo::rev_list(include, exclude, limit)`.
* `bgh-git/src/write.rs` (bug fix): `FileChange::Delete` used
  `update-index --force-remove`, which fails in bare repositories ("must be
  run in a work tree"); now removes the entry via `--index-info` mode 0.
* Workspace `Cargo.toml`: `regex = "1"`.

## Known gaps / TODO

* Release notes ignore `.github/release.yml` / `configuration_file_path`
  categories (no YAML dependency yet); `discussion_category_name` accepted
  and ignored. Asset download streams directly (GitHub redirects to a
  signed URL); no HTTP range requests yet.
* Search: `in:readme`, `team:`, `linked:`, `project:`, `is:sponsorable`,
  `mirror:` are accepted but not meaningful; scores are Postgres
  `ts_rank`-based, not GitHub's. No search rate limiting (30/min GitHub).
  Code search indexes the default branch only and orders by repository
  stars, then path.
* Events older than 90 days are not pruned (GitHub hides them); no
  activity events for org-only actions (`OrgMemberAdded`).
* Release events/payloads are only as complete as the emitting packages:
  issues/pulls/repos must emit the events above.
