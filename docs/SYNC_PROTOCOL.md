# Sync protocol (v1)

This is the wire contract between the web client's local-first store
(`web/src/sync/`) and the `bgh-sync` crate. Both sides implement it exactly;
if you change it, bump `schemaVersion`, update this file, the TypeScript
interfaces in `web/src/sync/models.ts` and the Rust structs in the same commit.

The in-browser mock backend (`web/src/mock/`) is a reference implementation
of the server half — when in doubt, behave like it.

---

## 1. Concepts

| Term | Meaning |
|------|---------|
| **model** | A synced entity type: `user`, `org`, `membership`, `team`, `repo`, `viewerRepo`, `label`, `milestone`, `issue`, `comment`, `review`, `issueEvent`, `notification`. |
| **row** | One model instance in its *compact client shape* (section 3). Not the GitHub REST shape. Keys are camelCase. |
| **scope** | A permission/subscription unit: `repo:{id}`, `org:{id}`, `user:{id}`. Every row (except `user`, see 3.1) belongs to exactly one scope. |
| **sync action** | A row of `sync_actions(id BIGSERIAL, scope, model, model_id, action, data, tx, created_at)`. `id` is the global, monotonically increasing **sync id**. |
| **lastSyncId** | The highest sync id the client has applied. Stored in IndexedDB. |
| **tx** | A client-generated UUID identifying one optimistic mutation (one REST request). |

### Encoding rules

* JSON, UTF-8. Responses are compressed (`br`/`gzip`) by the server.
* ids are JSON numbers (int64 database ids; safe up to 2^53).
* Timestamps are strings `YYYY-MM-DDTHH:MM:SSZ` (UTC, second precision —
  same as the REST API). Because the format is fixed they sort correctly as
  strings.
* `null` means "no value"; an **absent key** means "unchanged / not loaded"
  (see lazy fields and merge semantics).
* Colors are 6 hex digits without `#` (like GitHub).

### Scope membership of models

| model | scope | in bootstrap | notes |
|-------|-------|--------------|-------|
| `user` | *(referenced)* | yes, referenced ones | see 3.1 |
| `org`, `membership`, `team` | `org:{orgId}` | yes | |
| `repo`, `label`, `milestone`, `issue` | `repo:{repoId}` | yes | `issue.body` is lazy |
| `comment`, `review`, `issueEvent` | `repo:{repoId}` | **no** (lazy) | loaded per issue via partial sync; deltas *are* streamed |
| `viewerRepo`, `notification` | `user:{userId}` | yes | viewer-specific data |

`notification` rows of repositories the user can no longer read are never
loaded (bootstrap, partial sync or deltas; SQL `bgh_can_read_repo`), and
threads are deleted with a `D` action when the user loses read access
(`bgh-notify` `privacy.rs`, P21).

---

## 2. Server ordering guarantee (important for `bgh-sync`)

Clients must see a **gap-free, strictly increasing** id sequence: a client
at `lastSyncId = N` must eventually receive every action with id `> N`, and
never one `<= N` later. Writers do **not** serialize (a global lock capped
synced commits at a few hundred per second, #241): ids come from the
`sync_actions` identity, and with concurrent transactions id 101 may commit
before id 100, or id 100 may roll back and never commit.

The server therefore delivers only up to the **commit-order watermark** `W`
(`bgh_core::seqlog`):

> **Invariant.** Every id `<= W` has a committed row in `sync_actions`, and
> `W` never decreases. Hence no action with id `<= W` can ever commit
> afterwards (the primary key already holds a committed row for it), and a
> reader that consumes `(cursor, W]` in id order misses and reorders
> nothing.

* `W` advances over contiguous visible ids. A missing id is either in
  flight or burned by a rollback. Once the row after it is older than a
  grace period (250 ms) the reader inserts a filler row (scope `!gap`) for
  it with `ON CONFLICT (id) DO NOTHING` and a short `lock_timeout`. If the
  writer is still in flight the insert waits on the unique index: it times
  out and the gap stays open, or the writer finishes meanwhile and its own
  row stays (committed) or the filler goes in (rolled back). Fillers match
  no client scope and are never sent.
* A writer whose drawn id was filled first (only possible if it stalled
  between drawing the id and inserting the row for longer than the grace
  period) gets a short `ON CONFLICT DO NOTHING` insert back and re-inserts
  the transaction's actions with fresh ids, so they stay contiguous and in
  order.
* `W` is persisted (`log_watermarks`), and compaction never deletes rows
  above it.
* Live delivery (the hub), replay (`(since, L]` with `L <= W`), `hello.head`
  and every `lastSyncId` (bootstrap, partial sync, pull boot data) use `W`.
  Bootstrap and partial sync read `W` *before* opening their snapshot, so
  the snapshot reflects every action `<= W`. It may also reflect some
  actions `> W`. The client replays those and re-applies them, which is
  harmless because deltas carry the row's state (§5 merge semantics) and
  every later change to the same key is replayed after them.
* The event outbox (`event_outbox`) uses the same scheme (filler kind
  `!gap`): durable listeners never read past its watermark.

`X-Bgh-Sync-Id` and `lastSyncId` keep their meaning. Only the "visible ⇒
every lower id committed" guarantee moved from the writers (lock) to the
readers (watermark). A delta can arrive a few milliseconds later than
before while a lower id is still committing.

Retention: actions older than the retention window (default 7 days) may be
pruned. The server must remember `min_retained_id`.

---

## 3. Model shapes

These are normative. TypeScript is used for brevity; `?` = key may be absent,
`| null` = key present but may be null. The same declarations live in
`web/src/sync/models.ts`.

```ts
type ID = number;
type Timestamp = string; // "2024-01-01T00:00:00Z"
type Permission = 'read' | 'triage' | 'write' | 'maintain' | 'admin';
type ReactionCounts = Partial<Record<'+1' | '-1' | 'laugh' | 'hooray' | 'confused' | 'heart' | 'rocket' | 'eyes', number>>;

interface User {
  id: ID;
  login: string;
  name: string | null;
  avatarUrl: string;          // may be "" → client renders initials
  type: 'User' | 'Bot';
}

interface Org {
  id: ID;
  login: string;
  name: string | null;
  avatarUrl: string;
  description: string | null;
}

interface Membership {        // org membership, scope org:{orgId}
  id: ID;                     // membership row id
  orgId: ID;
  userId: ID;
  role: 'admin' | 'member';
}

interface Team {
  id: ID;
  orgId: ID;
  slug: string;
  name: string;
  description: string | null;
  privacy: 'closed' | 'secret';
  parentId: ID | null;
  memberIds: ID[];
  repoIds: ID[];
}

interface Repo {
  id: ID;
  ownerId: ID;                // user or org id
  owner: string;              // owner login (denormalized for routing)
  name: string;
  description: string | null;
  private: boolean;
  visibility: "public" | "private" | "internal"; // `private` is true for internal too
  fork: boolean;
  archived: boolean;
  defaultBranch: string;
  mirrorUrl: string | null;   // upstream of a pull mirror (git writes refused)
  language: string | null;
  topics: string[];
  stars: number;
  forks: number;
  watchers: number;
  openIssues: number;         // issues only (not PRs)
  openPulls: number;
  hasIssues: boolean;
  hasProjects: boolean;
  hasWiki: boolean;
  pushedAt: Timestamp | null;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

interface ViewerRepo {        // scope user:{viewer}; id === repo id
  id: ID;
  permission: Permission;
  starred: boolean;
  watching: 'subscribed' | 'ignored' | 'participating';
}

interface Label {
  id: ID;
  repoId: ID;
  name: string;
  color: string;              // "d73a4a"
  description: string | null;
}

interface Milestone {
  id: ID;
  repoId: ID;
  number: number;
  title: string;
  description: string | null;
  state: 'open' | 'closed';
  dueOn: Timestamp | null;
  openIssues: number;
  closedIssues: number;
  createdAt: Timestamp;
  updatedAt: Timestamp;
  closedAt: Timestamp | null;
}

interface Issue {             // issues and pull requests share this model
  id: ID;
  repoId: ID;
  number: number;
  title: string;
  body?: string | null;       // LAZY: absent in bootstrap (section 6)
  bodyEditedAt?: Timestamp | null; // LAZY, sent with `body`: latest body edit (edit history), null if never edited
  state: 'open' | 'closed';
  stateReason: 'completed' | 'not_planned' | 'reopened' | 'duplicate' | null;
  authorId: ID;
  assigneeIds: ID[];
  labelIds: ID[];
  milestoneId: ID | null;
  comments: number;           // comment count
  locked: boolean;
  activeLockReason?: 'off-topic' | 'too heated' | 'resolved' | 'spam' | null;
  reactions?: ReactionCounts;
  parentId?: ID | null;       // sub-issues: parent issue id
  subIssueIds?: ID[];         // sub-issues in priority order (may be in other repos)
  pinned?: boolean;           // pinned to the repo's issue list
  linkedPullIds?: ID[];       // PRs that close this issue (keyword or manual link; may be in other repos)
  createdAt: Timestamp;
  updatedAt: Timestamp;
  closedAt: Timestamp | null;
  isPr: boolean;
  issueType?: { id: ID; name: string; color: IssueTypeColor | null } | null; // org issue type
  duplicateOfId?: ID | null;  // closed as a duplicate of this issue (may be in another repo)
  blockedByIds?: ID[];        // dependencies: issues blocking this one (may be in other repos)
  openBlockedBy?: number;     // how many of blockedByIds are open ("Blocked" badge)
  blockingIds?: ID[];         // issues this one blocks
  // Present iff isPr === true:
  draft?: boolean;
  merged?: boolean;
  mergedAt?: Timestamp | null;
  mergedById?: ID | null;
  headRef?: string;
  headRepoId?: ID | null;     // null when the fork was deleted
  headSha?: string;
  baseRef?: string;
  baseSha?: string;
  mergeable?: boolean | null; // null = not computed yet
  mergeableState?: 'clean' | 'dirty' | 'blocked' | 'behind' | 'unstable' | 'unknown';
  reviewDecision?: 'approved' | 'changes_requested' | 'review_required' | null;
  requestedReviewerIds?: ID[];
  requestedTeamIds?: ID[];
  checks?: 'success' | 'failure' | 'pending' | 'neutral' | null;
  additions?: number;
  deletions?: number;
  changedFiles?: number;
  commits?: number;
}

interface Comment {           // issue/PR conversation comment (LAZY model)
  id: ID;
  repoId: ID;
  issueId: ID;
  authorId: ID;
  body: string;               // markdown source; the client renders it
  authorAssociation: 'OWNER' | 'MEMBER' | 'COLLABORATOR' | 'CONTRIBUTOR' | 'FIRST_TIME_CONTRIBUTOR' | 'NONE';
  reactions?: ReactionCounts;
  minimizedReason?: MinimizedReason | null; // hidden by a triager (P42)
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

type MinimizedReason = 'spam' | 'abuse' | 'off-topic' | 'outdated' | 'duplicate' | 'resolved';

interface Review {            // PR review (LAZY model)
  id: ID;
  repoId: ID;
  issueId: ID;                // the PR's issue id
  authorId: ID;
  state: 'APPROVED' | 'CHANGES_REQUESTED' | 'COMMENTED' | 'DISMISSED' | 'PENDING';
  body: string;
  commitId: string;
  submittedAt: Timestamp | null;
  minimizedReason?: MinimizedReason | null;
}

interface IssueEvent {        // timeline event (LAZY model)
  id: ID;
  repoId: ID;
  issueId: ID;
  actorId: ID | null;
  event:
    | 'labeled' | 'unlabeled' | 'assigned' | 'unassigned'
    | 'milestoned' | 'demilestoned' | 'renamed' | 'closed' | 'reopened'
    | 'merged' | 'referenced' | 'locked' | 'unlocked'
    | 'review_requested' | 'review_request_removed'
    | 'ready_for_review' | 'convert_to_draft' | 'head_ref_force_pushed'
    | 'mentioned' | 'subscribed' | 'cross-referenced' | 'pinned' | 'unpinned'
    | 'transferred' | 'sub_issue_added' | 'sub_issue_removed'
    | 'parent_issue_added' | 'parent_issue_removed'
    | 'issue_type_added' | 'issue_type_changed' | 'issue_type_removed'
    | 'blocked_by_added' | 'blocked_by_removed' | 'blocking_added' | 'blocking_removed'
    | 'marked_as_duplicate' | 'unmarked_as_duplicate';
  data: {                     // only the keys relevant to `event`
    labelId?: ID; labelName?: string; labelColor?: string;
    assigneeId?: ID; reviewerId?: ID;
    milestoneTitle?: string;
    from?: string; to?: string;  // renamed
    stateReason?: string;
    commitId?: string;
    lockReason?: string;                          // locked
    sourceIssueId?: ID; sourceCommentId?: ID;     // cross-referenced
    sourceNumber?: number; sourceRepository?: string; sourceIsPr?: boolean; // ("owner/repo")
    subIssueId?: ID; subIssueNumber?: number; subIssueRepository?: string;          // sub_issue_*
    parentIssueId?: ID; parentIssueNumber?: number; parentIssueRepository?: string; // parent_issue_*
    fromRepository?: string;                      // transferred ("owner/repo")
    issueTypeName?: string; issueTypeColor?: string;          // issue_type_*
    prevIssueTypeName?: string; prevIssueTypeColor?: string;  // issue_type_changed
    // blocked_by_* / blocking_* (the other issue), marked/unmarked_as_duplicate
    // (the original), closed as duplicate (the original):
    otherIssueId?: ID; otherIssueNumber?: number; otherIssueRepository?: string;
  };
  createdAt: Timestamp;
}

interface Notification {      // scope user:{viewer}
  id: ID;                     // thread id
  repoId: ID;
  subjectType: 'Issue' | 'PullRequest' | 'Commit' | 'Release' | 'Discussion' | 'CheckSuite';
  subjectId: ID | null;       // issue id for Issue/PullRequest
  title: string;
  reason: 'assign' | 'author' | 'comment' | 'mention' | 'review_requested'
        | 'state_change' | 'subscribed' | 'team_mention' | 'manual'
        | 'ci_activity' | 'security_alert';
  unread: boolean;
  updatedAt: Timestamp;
  lastReadAt: Timestamp | null;
}
```

Server notes: `authorId` / `actorId` / `mergedById` are `null` when the
user was deleted (GitHub's "ghost"). `IssueTypeColor` is one of GitHub's
issue type colors (`gray`, `blue`, `green`, `yellow`, `orange`, `red`,
`pink`, `purple`). `mergeableState` `has_hooks` as `clean`, `draft` as
`blocked`. `reactions` is always present (`{}` when empty) so a removed
last reaction reaches the client.

### 3.0 Pull-request extension models (lazy)

Recorded by bgh-pulls in `repo:{id}` scopes and streamed as ordinary deltas;
not part of the bootstrap. A client loads them per PR with
`GET /_bgh/repos/{o}/{r}/pulls/{n}/sync` (same envelope as partial sync:
`{lastSyncId, models}` with `reviewComment`, `review` (incl. the viewer's
pending one), `reaction`, `checkSuite`, `checkRun`, `commitStatus` of the
head commit, the viewer's `viewedFile` rows, `user`). Clients that don't know a model ignore its deltas.
Pending reviews and their comments are never broadcast: writes to them
return the rows in the response (see `TxApply` in `web/src/sync/transactions.ts`).
The `issue` row of a PR additionally carries `mergeCommitSha`, `rebaseable`,
`maintainerCanModify`, `autoMerge {mergeMethod, enabledById, ...} | null` and
`reviewComments`.

```ts
interface ReviewComment {     // thread = root (inReplyToId null) + replies
  id: ID; repoId: ID; issueId: ID; reviewId: ID | null; inReplyToId: ID | null;
  authorId: ID; body: string; path: string; commitId: string; originalCommitId: string;
  subjectType: 'line' | 'file'; side: 'LEFT' | 'RIGHT' | null; startSide: 'LEFT' | 'RIGHT' | null;
  line: number | null;        // null when outdated
  originalLine: number | null; startLine: number | null; originalStartLine: number | null;
  position: number | null; originalPosition: number | null; outdated: boolean;
  resolvedAt: Timestamp | null; resolvedById: ID | null;   // on the root
  diffHunk: string; createdAt: Timestamp; updatedAt: Timestamp;
}
interface Reaction { id: ID; subjectType: 'pull_request_review_comment'; subjectId: ID; userId: ID; content: ReactionContent; issueId: ID }
interface CheckSuite { id: ID; repoId: ID; headSha: string; headBranch: string | null; appSlug: string; status: string; conclusion: string | null; latestCheckRunsCount: number }
interface CheckRun { id: ID; repoId: ID; checkSuiteId: ID | null; headSha: string; name: string; status: string; conclusion: string | null; detailsUrl: string | null; title: string | null; startedAt: Timestamp | null; completedAt: Timestamp | null }
interface CommitStatus { id: ID; repoId: ID; sha: string; state: 'error' | 'failure' | 'pending' | 'success'; context: string; description: string | null; targetUrl: string | null; creatorId: ID | null; createdAt: Timestamp }
```

### 3.1 Users

`user` rows are not owned by a scope. The server includes every user
*referenced* by rows it sends (authors, assignees, members, reviewers,
actors) in the same response (`models.user` for bootstrap/partial, `refs`
for deltas). A profile change (login/name/avatar) is recorded as a `user`
action in the `user:{id}` scope **and** in each `org:{id}` scope the user is a
member of. Clients may hold slightly stale users for outside contributors
until the next bootstrap; that is accepted.

### 3.2 Extensions (server notes)

Additive keys and models; clients that don't know them ignore them. They
are built by the same shape loader as everything else
(`bgh_core::sync::shapes`), so deltas always equal what a load returns.

* `issue` rows of pull requests also carry `mergeCommitSha` (string |
  null), `rebaseable` (boolean | null), `maintainerCanModify` (boolean),
  `autoMerge` (`{enabledById, mergeMethod}` | null) and `reviewComments`
  (number) and `closingIssueIds` (ID[]: issues the PR closes on merge,
  possibly in other repositories). Every `issue` row carries
  `activeLockReason`, `parentId`, `pinned` and `linkedPullIds` (declared
  optional above). Both id lists come from `issue_pr_links`; a link change
  re-syncs the issue and the PR row.
* `issueEvent.data` may also carry `teamId` (team review requests),
  `before`/`after` (force pushes), `ref` (head ref deleted/restored),
  `reviewId`/`dismissalMessage` (review dismissed) and `mergeMethod`
  (auto-merge).
* Delta-only models in `repo:{repoId}` (not in the bootstrap or partial
  sync): `reviewComment` (PR inline comment: `id, repoId, issueId,
  reviewId, inReplyToId, authorId, body, path, commitId, originalCommitId,
  subjectType, side, startSide, line, originalLine, startLine,
  originalStartLine, position, originalPosition, diffHunk, outdated,
  resolvedAt, resolvedById, reactions, minimizedReason, createdAt, updatedAt`; comments of
  pending reviews are never sent; the PR page loads the current rows, incl.
  the viewer's own pending ones, from `GET /_bgh/repos/{o}/{r}/pulls/{n}/sync`,
  built from the same shapes), `checkRun` (`id, repoId, checkSuiteId, headSha,
  name, status, conclusion, detailsUrl, title, startedAt, completedAt`),
  `checkSuite` (`id, repoId, headSha, headBranch, appSlug, status,
  conclusion, latestCheckRunsCount`) and `commitStatus` (`id, repoId, sha,
  state, context, description, targetUrl, creatorId, createdAt`).
* `viewedFile` (P38; delta-only, in the owner's `user:{userId}` scope): `id,
  repoId, issueId, userId, path, blobSha, updatedAt` — a PR file the user
  marked "Viewed"; it counts as viewed only while the PR diff entry's `sha`
  equals `blobSha`. Unmarking records a `D`. The PR page loads the viewer's
  rows from `GET /_bgh/repos/{o}/{r}/pulls/{n}/sync` (`models.viewedFile`).
* `repoImport` (delta-only, `repo:{repoId}`): `id, repoId, status, phase,
  error` when a repository import changes status (queued, importing,
  complete, failed, cancelled). Progress counters are polled from
  `GET /_bgh/repos/{o}/{r}/import`.
* Reactions have no model: the reacted `issue`, `comment` or
  `reviewComment` row is re-sent with its `reactions` counts. (The PR
  page's `/sync` snapshot also lists per-user `Reaction` rows of review
  comments so the client knows the viewer's own reactions; they are never
  sent as deltas — counts come from `reviewComment.reactions`.)
* A `D` action's `d` is `null`.

---

## 4. Bootstrap

```
GET /_bgh/sync/bootstrap[?scopes=repo:1,org:2,user:3]
Cookie: session
```

* Without `scopes`: the server picks the **default scope set** for the viewer:
  `user:{viewer}`, `org:{id}` for every org membership, and `repo:{id}` for
  every repo the viewer has explicit access to (owner, collaborator, team
  grant, or org base permission ≥ read).
* With `scopes`: only those (used to add a scope on demand, e.g. visiting a
  public repo outside the default set). Scopes the viewer cannot read are
  returned in `denied` (never an error; `repo:` ids the viewer can't see
  look exactly like nonexistent ones).

Response `200 application/json`:

```jsonc
{
  "schemaVersion": 1,
  "lastSyncId": 9123,            // sync watermark: every action <= it is reflected
  "userId": 3,
  "scopes": ["user:3", "org:2", "repo:1"],
  "denied": [],
  "models": {
    "user":         [ /* User */ ],
    "org":          [ /* Org */ ],
    "membership":   [ ],
    "team":         [ ],
    "repo":         [ ],
    "viewerRepo":   [ ],
    "label":        [ ],
    "milestone":    [ ],
    "issue":        [ /* Issue WITHOUT body */ ],
    "notification": [ ]
  }
}
```

The snapshot must reflect every action `<= lastSyncId`: take `lastSyncId =`
the commit-order watermark (§2) *before* opening the `REPEATABLE READ`
transaction the rows are read in. The snapshot may also reflect later
actions; replaying them is idempotent. Missing model keys mean "no rows".

The client stores the rows, `lastSyncId` and `scopes` in IndexedDB and
then opens the WebSocket with `since = lastSyncId`. On later page loads it
hydrates from IndexedDB and skips the bootstrap entirely.

---

## 5. WebSocket `GET /_bgh/sync/ws`

Same-origin, authenticated by the session cookie (the server must verify the
`Origin` header). One JSON object per text frame. Field `t` is the type.

Access grants: there is no dedicated "scope granted" message. A non-delete
`viewerRepo` delta for a repo the client isn't subscribed to (new, shared or
transferred repo), or a `membership` delta for the viewer in an unsubscribed
org, makes the client load and `sub` the scope `repo:{mid}` / `org:{orgId}`
(`SyncClient.applyDeltas`).

### Client → server

| message | meaning |
|---------|---------|
| `{"t":"sub","scopes":["repo:1","user:3"],"since":9123}` | Subscribe. Server replays every action with `id > since` in those scopes, then streams live. May be sent again later to add scopes (with that scope's own `since`, e.g. the `lastSyncId` of an on-demand bootstrap). |
| `{"t":"unsub","scopes":["repo:9"]}` | Stop streaming those scopes. |
| `{"t":"ping"}` | Keep-alive, every 25 s. Server answers `pong`. |

### Server → client

| message | meaning |
|---------|---------|
| `{"t":"hello","userId":3,"head":9200}` | Sent once after the upgrade. `head` = current max sync id. |
| `{"t":"delta", ...Delta}` | One action (below). |
| `{"t":"batch","id":9130,"items":[Delta, ...]}` | Several actions, ascending ids; `id` = last item's id. Clients apply a batch atomically. |
| `{"t":"ready","scopes":[...],"id":9200}` | Replay for that `sub` is complete; `id` = head at that moment. The client sets `lastSyncId = max(lastSyncId, id)`. |
| `{"t":"revoke","scope":"repo:1","reason":"forbidden"}` | Viewer lost access (or the repo was deleted/transferred away). Client deletes every row in that scope and forgets it. |
| `{"t":"rebootstrap","reason":"too_old"}` | `since` is older than `min_retained_id` (or `"schema"` when `schemaVersion` changed). Client discards its database and bootstraps from scratch. |
| `{"t":"pong"}` | Reply to ping. |
| `{"t":"error","code":"bad_request","message":"..."}` | Non-fatal protocol error. |

Close codes: `4001` unauthenticated (client goes to login), `4009` rebootstrap
required (equivalent to the message), anything else → reconnect.

Server notes (bgh-sync; compatible with the client above):
* The client may pass its schema version as `?v=1`; a mismatch answers
  `rebootstrap` (`"schema"`) and closes with `4009`.
* A connection whose outgoing queue overflows gets
  `{"t":"error","code":"slow_consumer",...}` and is closed with `1013`;
  the client reconnects and resumes from `lastSyncId` (nothing is lost).
* Signing out (`/_bgh/auth/logout`, session revocation, suspension)
  closes the affected sockets with `4001`.
* Deltas in scopes the viewer can't read are never sent: on `sub`, every
  unreadable or malformed scope is answered with a `revoke`, and `ready.scopes`
  lists only the subscribed (readable) ones.
* `ready.id` is the server head when the replay finished; every action
  `<= id` of the subscribed scopes has been sent before `ready`.

### Delta

```jsonc
{
  "t": "delta",
  "id": 9124,                 // sync id
  "scope": "repo:1",
  "model": "issue",
  "mid": 42,                  // model id
  "a": "U",                   // "I" insert | "U" update | "D" delete
  "d": { "id": 42, "title": "New title", "updatedAt": "..." },  // null for D
  "tx": "1f0c…",              // present iff caused by a request with X-Client-Tx
  "refs": { "user": [ /* User rows referenced by d, optional */ ] }
}
```

Merge semantics (both `I` and `U`): for each key present in `d`, replace the
local value (arrays are replaced wholesale, never merged). Keys absent from
`d` are left unchanged. The server SHOULD send the complete row minus lazy
fields; lazy fields (`issue.body`) are included only when they changed.
`I` for an id the client already has, or `U` for one it lacks, are treated
as upserts. `D` removes the row; deleting an `issue` also removes its
`comment`/`review`/`issueEvent` rows locally.

### Ordering, batching, liveness

* On one connection ids are strictly ascending (replay is merged with the
  live stream server-side; actions already sent are not repeated). A later
  `sub` that adds a scope replays that scope from its own `since`, so its
  replay batch may contain ids below ones already streamed for other scopes.
* Replay is sent in `batch`es of ≤ 500 items. Live actions SHOULD be
  coalesced in windows of ~10 ms into a `batch`.
* The client treats ≥ 60 s without any server message as a dead connection.
  The server closes connections that sent nothing for 90 s.
* Reconnect: exponential backoff starting at 500 ms, ×2, capped at 30 s,
  ±30 % jitter; reset after a `ready`. The client reconnects immediately on
  the `online` event and when the tab becomes visible.
* After reconnecting the client sends one `sub` with all its scopes and
  `since = lastSyncId`.

---

## 6. Partial sync (lazy models and fields)

```
GET /_bgh/sync/partial?model=comment,review,issueEvent&issue=42
GET /_bgh/sync/partial?model=issue&id=42
```

| `model` | filter | returns |
|---------|--------|---------|
| `comment`, `review`, `issueEvent` (any subset, comma separated) | `issue=ID` | all rows of those models for the issue, **plus the `issue` row with `body`** |
| `issue` | `id=ID` | the full issue row including `body` |

Response:

```jsonc
{ "lastSyncId": 9130, "models": { "issue": [...], "comment": [...], "user": [...] } }
```

`404` if the viewer can't read the issue. The client calls this when an issue
is opened (and on hover-prefetch of an issue link) unless it already loaded
that issue in this database.

**Stale-response rule (client):** the client remembers, per row, the sync id
of the last delta applied to it. Rows in a partial (or on-demand bootstrap)
response are skipped if the local row was updated by a delta with
`id > response.lastSyncId` — the response is older than what the client has.

---

## 7. Mutations and optimistic reconciliation

Mutations use the normal GitHub-compatible REST API (`/api/v3/...`); there is
no separate mutation channel.

### Request

```
PATCH /api/v3/repos/acme/api/issues/42
X-Client-Tx: 6f1c2a8e-8d4b-4e7a-9c1e-0b8b8f3f5a10
X-CSRF-Token: <boot.csrf>
Content-Type: application/json

{"title":"New title"}
```

### Server obligations

1. Every `sync_actions` row written while handling the request carries the
   tx (`sync_actions.tx UUID NULL`), and the resulting deltas echo it as `tx`.
2. Response header `X-Bgh-Sync-Id: <max sync id written>`; omitted when the
   request wrote no synced data.
3. **Idempotency:** the server remembers `(user_id, tx) → (status, body,
   sync id)` for 24 h. A repeated request with the same tx returns the stored
   response without re-executing it (header `Idempotent-Replayed: true`).
   This makes client retries after a reload safe. While the first request
   with a tx is still executing, a duplicate gets `429` with
   `Retry-After: 1` (the client keeps its overlay and retries).

### Client algorithm

1. Create `tx` (UUID v4); compute the overlay — a list of ops
   (`update` with a patch, `insert` with a full row under a temporary
   negative id, `delete`); apply it to the store; persist
   `{tx, request, overlay, attempts}` to the IndexedDB `txs` store.
2. The visible value of every row is always `base ⊕ overlays of pending txs
   (in creation order)`. Deltas update `base`; overlays are re-applied on
   top, so concurrent remote edits to *other* fields show up immediately and
   our pending edit stays visible. Array patches may be expressed as
   `{ "$add": [..], "$remove": [..] }` so concurrent label/assignee edits
   compose; object patches as `{ "$merge": {key: value} }` (`null` removes
   the key) so edits to different keys compose.
3. Send requests FIFO, one at a time.
   * `2xx` without `X-Bgh-Sync-Id` → drop the overlay.
   * `2xx` with `X-Bgh-Sync-Id: N` → keep the overlay until a delta with
     this `tx` is applied (drop it in the *same* store action, so there is no
     flicker), or until `lastSyncId ≥ N`, or 30 s passed.
   * `4xx` (except 408/429) → roll back: drop the overlay (base values
     reappear), delete the tx, show the error message.
   * `401` "Sudo mode required…" (sensitive action, e.g. repo delete or
     transfer) → hold the queue, show the sudo prompt (`TxHooks.onSudoRequired`
     → `requestSudo()`), retry the tx once on success, roll it back on cancel
     or a second sudo 401. Never pauses the queue or expires the session.
   * any other `401` → pause the queue, keep txs, route to login.
   * `5xx`, `408`, `429`, network error → keep the overlay, retry with
     backoff (1 s ×2, max 60 s; honour `Retry-After`). Pending txs are
     reloaded and resent after a page reload.
4. Inserts: the overlay row has a negative temporary id. The echoed `I`
   delta brings the real row; dropping the overlay removes the temp row in
   the same action. Edits to a row that only exists as an overlay are not
   allowed until it is confirmed.

---

## 8. Client persistence (informative)

IndexedDB database `bgh-{userId}` (`bgh-mock-{userId}` in mock mode):
one object store per model (keyPath `id`), `meta` (`lastSyncId`, `scopes`,
`schemaVersion`, per-issue partial-loaded markers) and `txs` (pending
transactions, keyPath `tx`). Writes are batched (≤ 1 per 300 ms). Logging out
deletes the database.

## 9. Boot data

The server inlines boot data into `index.html` by replacing the
`<!--BGH_BOOT-->` comment:

```html
<script>window.__BGH_BOOT__={"user":{"id":3,"login":"ada","name":"Ada Lovelace","avatarUrl":""},"csrf":"…","config":{"siteName":"Better GitHub","signupEnabled":true,"version":"0.1.0"},"ts":"2024-01-01T00:00:00Z"}</script>
```

`user` is `null` when signed out. `user.twoFactorSetupRequired: true` is
added when the site requires two-factor authentication
(`auth_providers.require_2fa`) and the account has none: the client then
keeps the user on `/settings/security` (the server answers 403 to the
session outside the 2FA setup endpoints). When boot data is missing or older than
5 minutes (e.g. the shell came from the service worker cache) the client
refreshes it in the background from `GET /_bgh/boot` (same JSON).

## 10. Other private endpoints used by the web client

| Endpoint | Request | Response |
|----------|---------|----------|
| `GET /_bgh/boot` | — | boot JSON (§9) |
| `POST /_bgh/auth/login` | `{"login","password"}` | `200` boot JSON + session cookie; `422 {"message"}` on bad credentials; `401 {"message","twoFactorRequired":true,"twoFactorToken","twoFactorMethods"}` when the account has two-factor authentication (`twoFactorMethods`: `totp`, `recovery_code`, plus `webauthn` with security keys; `429` when throttled) |
| `POST /_bgh/auth/2fa/webauthn/challenge` → `POST /_bgh/auth/2fa/webauthn` | `{"twoFactorToken"}` → `{"twoFactorToken","id","credential"}` | second factor with a security key: `{id, options}` then `200` boot JSON + cookie; `422` when verification fails |
| `POST /_bgh/auth/login/passkey/challenge` → `POST /_bgh/auth/login/passkey` | — → `{"id","credential"}` | passwordless sign-in with a passkey (discoverable credential): `200` boot JSON + cookie |
| `POST /_bgh/auth/2fa` | `{"twoFactorToken","code"}` (TOTP or recovery code) | `200` boot JSON + session cookie; `422` wrong code; `401` pending login expired (sign in again) |
| `POST /_bgh/auth/signup` | `{"login","email","password"}` | `201` boot JSON + session cookie; `422` validation errors |
| `POST /_bgh/auth/logout` | — | `204`; server closes the session's sync sockets with `4001` (the server emits `Event::SessionEnded {user_id, session_id}`, which bgh-sync consumes; also emitted when sessions are revoked or a password is reset, then with `session_id: null` = all sessions) |
| `GET /_bgh/render/blob/{owner}/{repo}/{sha}?path=src/main.rs` | — | `{"language":"rust","lines":["<span class=\"hl-k\">fn</span> main() {", …]}` — one HTML string per source line, `Cache-Control: public, max-age=31536000, immutable`. `404` when no highlighter applies (client renders plain text). |
| `GET /_bgh/repos/{owner}/{repo}/blob-lines/{commitish}?path=&start=&end=&hl=1&text=0` | — | Diff viewer (P37): `{commit, path, sha, size, binary, image, mime, total_lines, start, end, lines, html, language, raw_url}` — plain (`lines`) and highlighted (`html`, with `hl=1`) lines `start..=end` (1-based, default the whole file, at most 20 000). `{commitish}` is a commit SHA, `{base}...{head}` (two SHAs: their merge base) or a ref; SHA forms are immutable. Binary content: metadata only. `422` without `path`, `404` unknown path / commit |
| `GET /_bgh/repos/{owner}/{repo}/commits/{sha}/annotations` | — | Every check-run annotation of the commit (≤ 1000, by path and line): `[{check_run_id, check_run_name, path, start_line, end_line, start_column, end_column, annotation_level, title, message, raw_details}]` |
| `DELETE /_bgh/notifications/threads/{id}/read` | `X-Client-Tx` | `204`; marks a thread unread (GitHub's REST API has no endpoint for this) |
| `GET /_bgh/repos/{owner}/{repo}/issue-templates[?ref=]` | — | `{commit_sha, templates: [{filename, type: "markdown"\|"form", name, about, title, labels, assignees, body, form}], config: {blank_issues_enabled, contact_links}, errors}`; the client addresses templates by basename (`?template=bug_report.yml`) |
| `GET /_bgh/repos/{owner}/{repo}/pull-templates[?ref=]` | — | `{commit_sha, source: "repo"\|"org"\|null, default: {filename, name, body}\|null, templates: [{filename, name, body}]}` — `pull_request_template.md` and `PULL_REQUEST_TEMPLATE/*.md` in `.github/`, the root or `docs/` (any case); when the repo has none, the owner's public `.github` repo (read concurrently). Cached in Redis by commit SHA; the compare page picks templates by basename (`?template=feature.md`) |
| `PUT\|DELETE /_bgh/repos/{owner}/{repo}/issues/{n}/pin` | `X-Client-Tx` | `204`; pin / unpin (max 3 per repo → `422`) |
| `GET /_bgh/repos/{owner}/{repo}/issues/{n}/viewer-reactions` | — | `{"issue": ["+1"], "comments": {"<comment id>": ["heart"]}}` — the viewer's own reactions (rows only carry counts) |
| `DELETE /_bgh/repos/{owner}/{repo}/issues/{n}/reactions/{content}` | `X-Client-Tx` | `204`; removes the viewer's reaction with that content (GitHub's REST API needs the reaction id); `204` when there is none |
| `DELETE /_bgh/repos/{owner}/{repo}/issues/comments/{id}/reactions/{content}` | `X-Client-Tx` | same for a comment |
| `PUT\|DELETE /_bgh/repos/{owner}/{repo}/minimized/{kind}/{id}` | `X-Client-Tx`; PUT `{"reason": "spam"\|"abuse"\|"off-topic"\|"outdated"\|"duplicate"\|"resolved"}` (GraphQL classifier spellings like `OFF_TOPIC` too) | `200 {"id","minimizedReason"}`; hide / unhide a comment (triage access). `kind` = `comment`, `review`, `review_comment`, `commit_comment`. The synced row (`comment`, `review`, `reviewComment`) is re-sent |
| `GET /_bgh/repos/{owner}/{repo}/minimized/{kind}?ids=1,2` | — | `[{"id","minimizedReason"}]` for the minimized ones among `ids` (commit comments aren't synced) |
| `GET /_bgh/repos/{owner}/{repo}/edits/{kind}/{id}` | — | edit history, newest first: `[{"id","editor","body","previous_body","edited_at","deleted_at","deleted_by"}]` (`body` = text after the edit, `previous_body` = before; `null` once deleted). `kind` also takes `issue` (`id` = the issue's number) |
| `DELETE /_bgh/repos/{owner}/{repo}/edits/{kind}/{id}/{edit_id}` | — | `204`; deletes that revision's text (content author or repo admin; the current revision → `422`). `edit_id` `0` deletes the original (pre-edit) text |
| `DELETE /_bgh/repos/{owner}/{repo}/issues/{n}` | `X-Client-Tx` | `204`; deletes an issue (repo admin; PRs → `422`). The number stays reserved: `GET /repos/{o}/{r}/issues/{n}` answers `410` afterwards; clients get a `D` for the issue |

Highlight classes (`hl-*`): `k` keyword, `s` string, `c` comment, `n`
number/constant, `t` type, `f` function/macro name, `a` attribute/tag. The
server must HTML-escape source text; the client inserts the lines as HTML.

All POST/PATCH/PUT/DELETE requests from the web client carry
`X-CSRF-Token: <boot.csrf>`; the server rejects cookie-authenticated
mutations without it (`403`). Token-authenticated API clients don't need it.
The token is derived from the session cookie (`bgh_core::auth::csrf_token`)
and enforced by `bgh_core::auth::csrf_middleware`; sign-in endpoints
(`/_bgh/auth/login|signup|2fa`, password reset) and server-rendered OAuth
forms (which carry their own nonce) are exempt.

---

## 11. Extension models: Projects (bgh-projects)

Added by package B11 as a separate section so the core protocol above stays
untouched. Rows live in the **owner's** scope: `org:{ownerId}` for
organization projects, `user:{ownerId}` for user projects. They are part of
the bootstrap of those scopes (provided to `bgh-sync` through the
`bgh_core::sync::ScopeProvider` hook, see `bgh_core::sync::load_provided`):
owners/org members/site admins get every project of the owner, other viewers
only `public` ones. Deltas use the same rules as every other model. No lazy
fields. Deleting a `project` also deletes its fields, views, items and
workflows locally (the server records those deletes as well).

| model | scope | in bootstrap |
|-------|-------|--------------|
| `project`, `projectField`, `projectView`, `projectItem`, `projectWorkflow` | `org:{ownerId}` / `user:{ownerId}` | yes |

```ts
interface Project {
  id: ID;
  ownerId: ID;                // user or org id
  number: number;             // per owner
  title: string;
  shortDescription: string | null;
  readme: string | null;      // markdown
  public: boolean;
  closed: boolean;
  closedAt: Timestamp | null;
  creatorId: ID | null;
  linkedRepoIds: ID[];
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

type ProjectFieldType = 'title' | 'assignees' | 'status' | 'labels' | 'repository' | 'milestone'
                      | 'text' | 'number' | 'date' | 'single_select' | 'iteration';
type OptionColor = 'GRAY' | 'BLUE' | 'GREEN' | 'YELLOW' | 'ORANGE' | 'RED' | 'PINK' | 'PURPLE';

interface ProjectField {
  id: ID;
  projectId: ID;
  name: string;
  dataType: ProjectFieldType; // title..milestone are built-ins backed by the issue
  position: number;
  options: { id: string; name: string; color: OptionColor; description: string }[] | null; // single_select, status
  iterations: {
    startDate: string;        // "YYYY-MM-DD"
    duration: number;         // days, default for new iterations
    iterations: { id: string; title: string; startDate: string; duration: number }[]; // gaps = breaks
  } | null;                   // iteration
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

interface ProjectView {
  id: ID;
  projectId: ID;
  number: number;
  name: string;
  layout: 'table' | 'board' | 'roadmap';
  position: number;
  filter: string;             // query string, e.g. 'is:open label:bug status:"In Progress"'
  groupByFieldId: ID | null;
  columnFieldId: ID | null;   // board columns (status / single_select / iteration)
  dateFieldId: ID | null;     // roadmap (date / iteration)
  sortBy: { fieldId: ID; direction: 'asc' | 'desc' }[];
  visibleFieldIds: ID[];      // ordered: table column order
  hiddenColumnIds: string[];  // board option/iteration ids
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

interface ProjectItem {
  id: ID;
  projectId: ID;
  contentType: 'Issue' | 'PullRequest' | 'DraftIssue';
  issueId: ID | null;         // issue/PR id (repo scope); null for drafts
  title: string | null;       // drafts only
  body: string | null;        // drafts only
  assigneeIds: ID[];          // drafts only (issues use issue.assigneeIds)
  archived: boolean;
  position: string;           // fractional index (base-62, bytewise order)
  viewPositions: Record<string, string>; // per-view override, keyed by view id
  values: Record<string, string | number>; // custom field values keyed by field id:
                              // text → string, number → number, date → "YYYY-MM-DD",
                              // single_select/status → option id, iteration → iteration id
  creatorId: ID | null;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

interface ProjectWorkflow {
  id: ID;
  projectId: ID;
  kind: 'item_added' | 'item_reopened' | 'item_closed' | 'pr_merged' | 'auto_add' | 'auto_archive';
  enabled: boolean;
  config: { statusOptionId?: string; repoIds?: ID[]; filter?: string };
  updatedAt: Timestamp;
}
```

An item may reference an issue in a repository whose scope the client has
not synced (or cannot read); clients fetch
`GET /_bgh/projects/{id}` / `GET /_bgh/owners/{owner}/projects/{number}`,
which returns the project's rows plus compact `issue`/`repo`/`label`/
`milestone`/`user` rows for readable repositories. Mutations go through the
private endpoints listed in `docs/packages/projects-wiki.md` and follow §7
(they accept `X-Client-Tx`).

