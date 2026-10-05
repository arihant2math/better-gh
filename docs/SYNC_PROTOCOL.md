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

---

## 2. Server ordering guarantee (important for `bgh-sync`)

Sync ids must become **visible in id order**. With concurrent transactions a
plain `BIGSERIAL` can commit id 101 before id 100, and a client replaying
`since=101` would silently miss 100. `bgh_core::sync::record` therefore takes
`pg_advisory_xact_lock(<SYNC_LOCK>)` before inserting into `sync_actions`, so
writers of synced data serialize on the insert and commit order equals id
order. (Writes that do not touch synced models are unaffected.)

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
  fork: boolean;
  archived: boolean;
  defaultBranch: string;
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
  createdAt: Timestamp;
  updatedAt: Timestamp;
  closedAt: Timestamp | null;
  isPr: boolean;
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
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

interface Review {            // PR review (LAZY model)
  id: ID;
  repoId: ID;
  issueId: ID;                // the PR's issue id
  authorId: ID;
  state: 'APPROVED' | 'CHANGES_REQUESTED' | 'COMMENTED' | 'DISMISSED' | 'PENDING';
  body: string;
  commitId: string;
  submittedAt: Timestamp | null;
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
    | 'parent_issue_added' | 'parent_issue_removed';
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
user was deleted (GitHub's "ghost"). `stateReason` `duplicate` is sent as
`not_planned`; `mergeableState` `has_hooks` as `clean`, `draft` as
`blocked`. `reactions` is always present (`{}` when empty) so a removed
last reaction reaches the client.

### 3.1 Users

`user` rows are not owned by a scope. The server includes every user
*referenced* by rows it sends (authors, assignees, members, reviewers,
actors) in the same response (`models.user` for bootstrap/partial, `refs`
for deltas). A profile change (login/name/avatar) is recorded as a `user`
action in the `user:{id}` scope **and** in each `org:{id}` scope the user is a
member of. Clients may hold slightly stale users for outside contributors
until the next bootstrap; that is accepted.

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
  "lastSyncId": 9123,            // max sync id included in this snapshot
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

The snapshot must be consistent with `lastSyncId`: read it in a
`REPEATABLE READ` transaction and take `lastSyncId = max(id)` from
`sync_actions` in that same transaction. Missing model keys mean "no rows".

The client stores the rows, `lastSyncId` and `scopes` in IndexedDB and
then opens the WebSocket with `since = lastSyncId`. On later page loads it
hydrates from IndexedDB and skips the bootstrap entirely.

---

## 5. WebSocket `GET /_bgh/sync/ws`

Same-origin, authenticated by the session cookie (the server must verify the
`Origin` header). One JSON object per text frame. Field `t` is the type.

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
   compose.
3. Send requests FIFO, one at a time.
   * `2xx` without `X-Bgh-Sync-Id` → drop the overlay.
   * `2xx` with `X-Bgh-Sync-Id: N` → keep the overlay until a delta with
     this `tx` is applied (drop it in the *same* store action, so there is no
     flicker), or until `lastSyncId ≥ N`, or 30 s passed.
   * `4xx` (except 408/429) → roll back: drop the overlay (base values
     reappear), delete the tx, show the error message.
   * `401` → pause the queue, keep txs, route to login.
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

`user` is `null` when signed out. When boot data is missing or older than
5 minutes (e.g. the shell came from the service worker cache) the client
refreshes it in the background from `GET /_bgh/boot` (same JSON).

## 10. Other private endpoints used by the web client

| Endpoint | Request | Response |
|----------|---------|----------|
| `GET /_bgh/boot` | — | boot JSON (§9) |
| `POST /_bgh/auth/login` | `{"login","password"}` | `200` boot JSON + session cookie; `422 {"message"}` on bad credentials |
| `POST /_bgh/auth/signup` | `{"login","email","password"}` | `201` boot JSON + session cookie; `422` validation errors |
| `POST /_bgh/auth/logout` | — | `204`; server closes the user's sync sockets with `4001` |
| `GET /_bgh/render/blob/{owner}/{repo}/{sha}?path=src/main.rs` | — | `{"language":"rust","lines":["<span class=\"hl-k\">fn</span> main() {", …]}` — one HTML string per source line, `Cache-Control: public, max-age=31536000, immutable`. `404` when no highlighter applies (client renders plain text). |
| `DELETE /_bgh/notifications/threads/{id}/read` | `X-Client-Tx` | `204`; marks a thread unread (GitHub's REST API has no endpoint for this) |
| `GET /_bgh/repos/{owner}/{repo}/issue-templates[?ref=]` | — | `{commit_sha, templates: [{filename, type: "markdown"\|"form", name, about, title, labels, assignees, body, form}], config: {blank_issues_enabled, contact_links}, errors}`; the client addresses templates by basename (`?template=bug_report.yml`) |
| `PUT\|DELETE /_bgh/repos/{owner}/{repo}/issues/{n}/pin` | `X-Client-Tx` | `204`; pin / unpin (max 3 per repo → `422`) |
| `GET /_bgh/repos/{owner}/{repo}/issues/{n}/viewer-reactions` | — | `{"issue": ["+1"], "comments": {"<comment id>": ["heart"]}}` — the viewer's own reactions (rows only carry counts) |
| `DELETE /_bgh/repos/{owner}/{repo}/issues/{n}/reactions/{content}` | `X-Client-Tx` | `204`; removes the viewer's reaction with that content (GitHub's REST API needs the reaction id); `204` when there is none |
| `DELETE /_bgh/repos/{owner}/{repo}/issues/comments/{id}/reactions/{content}` | `X-Client-Tx` | same for a comment |

Highlight classes (`hl-*`): `k` keyword, `s` string, `c` comment, `n`
number/constant, `t` type, `f` function/macro name, `a` attribute/tag. The
server must HTML-escape source text; the client inserts the lines as HTML.

All POST/PATCH/PUT/DELETE requests from the web client carry
`X-CSRF-Token: <boot.csrf>`; the server rejects cookie-authenticated
mutations without it (`403`). Token-authenticated API clients don't need it.
