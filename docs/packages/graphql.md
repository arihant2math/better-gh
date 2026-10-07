# Package B9: graphql (`bgh-graphql`)

Status: **complete** for the B9 scope. `scripts/gh-compat.sh`: **40/40
PASS**; extended matrix `crates/bgh-graphql/scripts/gh-extended.sh`:
**38/38 PASS** (gh 2.89.0, GHES mode). Branch `bgh/graphql`. No migrations.

## Surface

* `POST /api/graphql` (queries + mutations), `GET /api/graphql?query=…`
  (queries only; mutations over GET → `FORBIDDEN`). Same auth as REST
  (`token`/`Bearer`/Basic-PAT, session cookie; cookie POSTs are CSRF-checked
  by the server-wide `bgh_core::auth::csrf_middleware`). Bad credentials →
  401 REST error body. Anonymous callers can read public data; `viewer`
  and mutations need auth. `X-OAuth-Scopes` comes from the shared auth
  middleware; requests count against the shared `graphql` budget of
  `bgh_core::ratelimit` (headers set by its root middleware), which the
  `rateLimit` object reports.
* Resource limits (`src/cost.rs`, a schema extension run after
  validation), as on GitHub: connections selecting `nodes`/`edges` need
  `first`/`last` (`MISSING_PAGINATION_BOUNDARIES`), at most 100
  (`EXCESSIVE_PAGINATION`); a query may request at most 500,000 nodes
  (product of page sizes down each connection path, summed;
  `MAX_NODE_LIMIT_EXCEEDED`). Fragments are expanded at any depth, and
  `nodes(ids:)` takes at most 100 ids (`ARGUMENT_LIMIT`), each multiplying
  what is selected below it. Before parsing checks and validation (which
  expand fragments without memoising), a document whose fragment-expanded
  size exceeds 100,000 selections is rejected in linear time. The cost is the number of connection fetches
  / 100, rounded, at least 1: the middleware counts 1 point, the extension
  charges the rest (`ratelimit::charge`) and refreshes the `X-RateLimit-*`
  headers; with enforcement on, an over-budget query is rejected with
  `RATE_LIMITED`. `rateLimit { cost nodeCount }` report the computed
  values (`dryRun` is accepted but still charges).
* Pre-parse guard (`src/guard.rs`, issue #335): before async-graphql's
  recursive parser sees it, the raw query is scanned once (strings and
  comments skipped) and rejected with a 200 `errors[]` if it is longer
  than 256 KiB (`MAX_QUERY_LENGTH_EXCEEDED`) or nests `{`/`(`/`[` deeper
  than 128 (`MAX_NESTING_EXCEEDED`), so a deep query can't overflow the
  stack. `variables` are bounded by serde_json's recursion limit (128).
* `GET /api/v3/meta` (GHES shape, `installed_version: "3.17.0"` =
  `bgh_graphql::COMPAT_GHES_VERSION`). `gh` gates GraphQL feature detection
  on it: 3.17 = classic issue-search syntax, no classic projects.
* Errors use GitHub's shape: top-level `type` (`NOT_FOUND`, `FORBIDDEN`,
  `UNPROCESSABLE`, `INTERNAL`, `INVALID_CURSOR_ARGUMENTS`) + `path` +
  `locations`; messages match GitHub where `gh` parses them
  (`Could not resolve to a Repository with the name 'o/r'.`). No read
  access ⇒ `NOT_FOUND`, never `FORBIDDEN`.
* Schema SDL: `bgh_graphql::sdl()`. Type/field/enum/input names follow
  GitHub's v4 schema; introspection works (gh feature detection queries
  `__type(name: …)` for Repository, PullRequest,
  StatusCheckRollupContextConnection, WorkflowRun, SearchType,
  LinkedBranch, Release).

### Query root
`viewer`, `repository(owner,name,followRenames)`, `repositoryOwner(login)`,
`user(login)`, `organization(login)`, `node(id)`, `nodes(ids)`,
`search(query,type,first,last,after,before)` (ISSUE, REPOSITORY, USER;
DISCUSSION empty), `rateLimit`.

### Types (main fields)
* **Actors**: `User`, `Organization` (`teams`, `team`, `projects`,
  `projectsV2`, `viewerIsAMember`, …), `Bot`, `Mannequin`, `Team`;
  interfaces `Actor`, `RepositoryOwner` (incl. `repository`,
  `repositories(privacy, isFork, ownerAffiliations, orderBy, …)`), `Node`.
* **Repository**: every `gh repo view --json` field (ids, names, owner,
  parent, templateRepository, urls, dates, has*/…Allowed flags, counts,
  watchers/stargazers/forks, `defaultBranchRef`, `isEmpty`, `visibility`,
  `licenseInfo`, `viewer*`, `repositoryTopics`, `primaryLanguage`,
  `languages`, `issueTemplates`/`pullRequestTemplates`, `labels`, `label`,
  `milestones`, `milestone`, `latestRelease`, `releases`, `release`,
  `assignableUsers`, `mentionableUsers`, `collaborators`,
  `projects`/`projectsV2`), plus `issues`/`issue`/`issueOrPullRequest`,
  `pullRequests`/`pullRequest`, `refs`, `ref`, `object(oid|expression)`,
  `forks`.
* **Git**: `Ref` (`target`, `compare(headRef)` → `Comparison`,
  `associatedPullRequests`), `GitObject` = `Commit | Tag | Tree | Blob`,
  `Commit` (message*, dates, `author`/`committer`/`authors` (+
  `Co-authored-by` trailers), `parents`, `statusCheckRollup`, `status`),
  `GitActor` (user resolved by verified or noreply email).
* **Issue / PullRequest** (shared conversation fields via `MergedObject`):
  all `gh … --json` fields incl. `comments(first|last)`, `assignees`,
  `assignedActors`, `labels`, `milestone`, `reactionGroups` (always the 8
  groups, `viewerHasReacted`), `projectCards`/`projectItems`,
  `participants`, `authorAssociation`, `isPinned`, `stateReason`,
  `closedByPullRequestsReferences`, `linkedBranches`. PR: state incl.
  MERGED, head/base ref names + oids, `headRepository(Owner)`,
  `isCrossRepository`, `mergeable`, `mergeStateStatus`, `reviewDecision`,
  `reviews`, `latestReviews`, `latestOpinionatedReviews`, `reviewRequests`
  (`RequestedReviewer` = User|Team|Bot|Mannequin), `reviewThreads`,
  `commits` (gh's `statusCheckRollup: commits(last: 1)` alias works),
  `files`, `autoMergeRequest`, `closingIssuesReferences`, `mergeCommit`,
  `mergedBy`, `baseRef`/`headRef` (`compare`), `viewerCan*`,
  `isInMergeQueue`, `isMergeQueueEnabled`, `mergeQueue`,
  `mergeQueueEntry`.
* **Merge queue** (`bgh_pulls::merge_queue`): `Repository.mergeQueue(branch)`
  (null without a `merge_queue` rule) → `MergeQueue` (`entries`,
  `configuration` = `MergeQueueConfiguration`, `url`; node id key
  `"{repo_id}:{branch}"`), `MergeQueueEntry` (node; `position`, `state`,
  `enqueuer`, `headCommit`, `baseCommit`, `pullRequest`, `jump`, `solo`;
  `estimatedTimeToMerge` is null until estimated).
* **Checks**: `StatusCheckRollup` (`state`, `contexts` =
  `CheckRun | StatusContext`, `checkRunCount`, `checkRunCountsByState`,
  `statusContextCount`, `statusContextCountsByState`), `CheckSuite`
  (`app`, `workflowRun { event workflow { name } }`).
* `IssueComment`, `PullRequestReview`, `PullRequestReviewComment`,
  `PullRequestReviewThread`, `Label`, `Milestone`, `Release`,
  `ReleaseAsset`, `License`, `Language`, `RepositoryTopic`, `RateLimit`,
  `SearchResultItem` = Issue|PullRequest|Repository|User|Organization.

### Mutations
createIssue, updateIssue, closeIssue (stateReason), reopenIssue,
addComment, updateIssueComment, deleteIssueComment, addLabelsToLabelable,
removeLabelsFromLabelable, addAssigneesToAssignable,
removeAssigneesFromAssignable, replaceActorsForAssignable, lockLockable,
unlockLockable, pinIssue, unpinIssue, transferIssue, createLinkedBranch;
createPullRequest, updatePullRequest (incl. labels/assignees/milestone),
closePullRequest, reopenPullRequest, mergePullRequest (method,
expectedHeadOid), markPullRequestReadyForReview,
convertPullRequestToDraft, addPullRequestReview (pending or with event,
comments/threads), submitPullRequestReview, dismissPullRequestReview,
requestReviews (union or replace), requestReviewsByLogin,
enablePullRequestAutoMerge, disablePullRequestAutoMerge,
resolveReviewThread, unresolveReviewThread, updatePullRequestBranch,
enqueuePullRequest (jump, expectedHeadOid), dequeuePullRequest (entry
id). `enablePullRequestAutoMerge` on a base branch with a merge queue adds
the PR to the queue instead (GitHub's behaviour, used by `gh pr merge
--auto`).
createRepository (user or org owner), updateRepository,
cloneTemplateRepository, archiveRepository, unarchiveRepository, addStar,
removeStar, createRef, updateRef, deleteRef.

Payload fields `labelable`/`assignable`/`lockedRecord`/`unlockedRecord`/
`subject` use the `Lockable` interface (Issue | PullRequest; `id`,
`locked`, `activeLockReason`).

## Design

* **Writes never duplicate domain logic**: each mutation translates node
  ids to the REST path/body and calls the domain crate's handler function
  directly (`bgh_issues::issues::update`, `bgh_pulls::merge::merge`,
  `bgh_repos::settings::update_repo`, …), building bodies with
  `serde_json::from_value` (robust to new optional fields) and reading the
  response as JSON (`mutation::into_json`). Validation, permissions, sync
  records, events, jobs and webhooks are exactly the REST path's. The
  payload object is re-read afterwards.
* **Reads** go to the core tables through per-request async-graphql
  `DataLoader`s (`loaders.rs`): users, teams, repositories (row + owner +
  the viewer's effective permission, batched with
  `perms::repo_permissions` + `perms::effective`), issues, PR rows, issue
  labels/assignees, milestones, reactions, author associations, comments
  (windowed per issue with `row_number() OVER (PARTITION BY …)`), reviews,
  review requests, review comments, status rollups (`DISTINCT ON` latest
  status per context / check run per name), latest release, release
  assets, closing issues, git refs/commits/emptiness (one git open per
  repo per batch), users by commit email. Relations never issue per-row
  queries; list queries use `QueryBuilder` with bound parameters;
  `assignableUsers`/`mentionableUsers` compute grants in one recursive
  query.
* **Connections** (`conn.rs`): Relay args → offset window (cursor =
  base64 `cursor:v2:{position}`); `first`/`last`/`after`/`before` all
  supported, pages clamped to 100. `totalCount` and nodes are only computed
  when selected (`ctx.look_ahead()`); `last` without `before` counts first.
* **Node ids** are `bgh_core::node_id` (GitHub legacy format) and identical
  to REST `node_id` (tests assert round trips for repos, issues, PRs,
  comments). String-keyed ids: commits `Commit "{repo_id}:{sha}"`, refs
  `Ref "{repo_id}:{refs/…}"`, review threads
  `PullRequestReviewComment "thread:{root_comment_id}"`.
* Permissions: entry points (`repository`, `node`, `nodes`, `search`,
  owner repository lists, `parent`, `headRepository`, …) check read
  access; objects reached through an authorized parent reuse it.
* Search (`search.rs`) parses GitHub qualifiers (`repo:`, `user:`/`org:`,
  `is:`, `state:`, `author:`, `assignee:`, `mentions:`, `involves:`,
  `commenter:`, `review-requested:` (incl. teams), `reviewed-by:`,
  `review:`, `label:`, `no:`, `milestone:`, `head:`, `base:`, `in:`,
  `sort:`, `-` negation, `@me`) over issues/PRs (`ILIKE` on title/body),
  repositories and users, capped at 1000 candidates and filtered by
  batched permissions. (`gh search` itself uses B6's REST search.)
* PR `commits`/`files` read git directly (`git rev-list base..head`,
  `git diff --numstat/--name-status -M`) in the base repository (forks'
  heads are mirrored there by bgh-pulls).

## Tests

`cargo test -p bgh-graphql` (21 integration + 2 unit tests):
`tests/schema.rs` (SDL names, auth + scopes header, 401, GET vs mutation,
error `type`, introspection feature detection, `/meta`), `tests/queries.rs`
(repository fields + owner lists + privacy, issues connection
filters/pagination/`last`, issue view with comments/reactions/node round
trip, PR fields/reviews/requests/statuses, labels/milestones/search,
assignable vs mentionable users), `tests/mutations.rs` (issue lifecycle
incl. comments/labels/assignees/lock/pin/close reasons/linked branches, PR
lifecycle incl. draft/ready/review requests/pending review + submit/review
threads/close/reopen/squash merge, auto-merge, repository
create/update/archive/star/template/refs, permission and error types).

Workspace: `cargo clippy --workspace --all-targets -D warnings` clean;
`cargo test -p bgh-graphql`: 21 integration + 2 unit tests pass after the
merge of integration 895a904 (rate limiter consolidation).

Schema check against gh's query corpus: every GraphQL operation gh 2.89.0
sends in its own test suite (~120) was replayed against the server; the
only remaining unknown fields are github.com-only or out of scope:
`suggestedReviewerActors` (github.com actor reviewers), `deleteIssue`,
`revertPullRequest`, `Release.immutable` (only queried when introspection
reports it).

## gh-compat (`scripts/gh-compat.sh`, 40 commands)

| Result | Command |
|---|---|
| PASS | `gh auth status` |
| PASS | `gh auth setup-git` |
| PASS | `gh api user` |
| PASS | `gh api repos/{owner}/{repo}` |
| PASS | `gh api --paginate user/repos` |
| PASS | `gh api graphql viewer` |
| PASS | `gh repo create` |
| PASS | `gh repo view` |
| PASS | `gh repo view --json` |
| PASS | `gh repo list` |
| PASS | `gh repo clone` |
| PASS | `gh repo edit --description` |
| PASS | `gh label list` |
| PASS | `gh label create` |
| PASS | `gh issue create` |
| PASS | `gh issue list` |
| PASS | `gh issue list --json` |
| PASS | `gh issue view` |
| PASS | `gh issue comment` |
| PASS | `gh issue edit --add-label` |
| PASS | `gh issue close` |
| PASS | `gh issue reopen` |
| PASS | `gh pr create` |
| PASS | `gh pr list` |
| PASS | `gh pr view` |
| PASS | `gh pr view --json` |
| PASS | `gh pr diff` |
| PASS | `gh pr checkout` |
| PASS | `gh pr status` |
| PASS | `gh pr comment` |
| PASS | `gh pr review --comment` |
| PASS | `gh pr merge --merge` |
| PASS | `gh release create` |
| PASS | `gh release list` |
| PASS | `gh release view` |
| PASS | `gh release upload` |
| PASS | `gh search repos` |
| PASS | `gh search issues` |
| PASS | `gh status` |
| PASS | `gh repo delete` |

## Extended matrix (`crates/bgh-graphql/scripts/gh-extended.sh`)

Runs the gh-compat fixtures, then:

| Result | Command |
|---|---|
| PASS | `gh label create` |
| PASS | `gh repo view --json all` |
| PASS | `gh repo archive` |
| PASS | `gh repo unarchive` |
| PASS | `gh repo create --template` |
| PASS | `gh issue create --label --assignee` |
| PASS | `gh issue list filters` |
| PASS | `gh issue view --comments` |
| PASS | `gh issue view --json all` |
| PASS | `gh issue edit --title --add-assignee` |
| PASS | `gh label edit` |
| PASS | `gh label delete` |
| PASS | `gh issue status` |
| PASS | `gh issue lock` |
| PASS | `gh issue unlock` |
| PASS | `gh issue pin` |
| PASS | `gh issue unpin` |
| PASS | `gh issue develop` |
| PASS | `gh issue develop --list` |
| PASS | `gh issue close --reason` |
| PASS | `gh pr create --draft` |
| PASS | `gh pr view --json all` |
| PASS | `gh pr view --comments` |
| PASS | `gh pr checks` |
| PASS | `gh pr edit` |
| PASS | `gh pr ready --undo` |
| PASS | `gh pr ready` |
| PASS | `gh pr review --approve (own PR fails)` |
| PASS | `gh pr close` |
| PASS | `gh pr reopen` |
| PASS | `gh pr list --json` |
| PASS | `gh pr list --search` |
| PASS | `gh pr merge --squash --delete-branch` |
| PASS | `gh search prs` |
| PASS | `gh release view --json` |
| PASS | `gh api graphql paginate` |
| PASS | `gh api graphql node` |
| PASS | `gh status` |

## Shared-code changes (additive)

* `bgh-repos`: made `stars::{star, unstar}`, `forks::{generate,
  create_fork}`, `gitdb::{create_ref, update_ref, delete_ref}` and the
  `gitdb::{CreateRef, UpdateRef}` bodies `pub` (called by mutations).
* `bgh-git`: `patch::child_stream` is now fused. Without it `gh pr diff`
  failed with "unexpected EOF": the compression layer polls the body again
  after the end, `futures::stream::Unfold` panics on that and the gzip
  stream was truncated (REST `.diff`/`.patch` media types of bgh-pulls).
* New dependency: `async-graphql` 7 (`dataloader` feature only).

## Known gaps / TODO

* Classic projects connections are empty (GitHub sunset them). Projects v2
  are implemented in `model/project.rs` / `mutation/projects.rs` (P13, see
  `docs/packages/p13-projects-api.md`).
* `languages` reports only the primary language (sized by repo size);
  `issueTemplates`/`pullRequestTemplates` are empty (bgh-issues parses
  templates for the web client and could provide them).
* Linked branches are derived from branch names (`{number}-…`); there is
  no link table, so branches created with other names aren't listed.
* `isRequired` on checks is always false, `potentialMergeCommit` is null,
  review `reactionGroups` are empty, comment edit history (`lastEditedAt`,
  `editor`) is approximated.
* The cost model is static (page sizes requested, not items returned),
  like GitHub's; query depth is also limited (32).
* `deleteIssue`, `revertPullRequest`, discussions, gists and sponsorships
  are not implemented.
