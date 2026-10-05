# B11 projects-wiki — status

Branch `bgh/projects-wiki`. Crates `bgh-projects` (migrations 1100-1199) and
`bgh-wiki` (1200-1299), both mounted in bgh-server.

Status: **in progress**.

## Projects (bgh-projects)

GitHub Projects (v2) semantics. Owners are users or orgs (`users` row).

### Permissions

| who | project role |
|-----|--------------|
| site admin, user owner, org admin | `admin` |
| org member (any role) | `write` |
| anyone (incl. anonymous) on a `public` project | `read` |
| others | none → `404` |

`read`: view. `write`: items, values, views, fields, workflows, title/description/readme.
`admin`: `public`, `closed`, delete. Readers lacking a role get `403`.
Adding an issue/PR item requires read access to its repository; issue
contents in snapshots are only included for repositories the viewer can read.

### Sync models (scope `org:{ownerId}` or `user:{ownerId}`)

Every write records sync actions (camelCase compact shapes, see
`docs/SYNC_PROTOCOL.md` §11). Rows are included in bootstrap for those scopes
through the `bgh_core::sync` scope-provider hook (see "Shared code").

```ts
interface Project { id; ownerId; number; title; shortDescription: string|null; readme: string|null;
  public: boolean; closed: boolean; closedAt: Timestamp|null; creatorId: ID|null;
  linkedRepoIds: ID[]; createdAt; updatedAt }
type ProjectFieldType = 'title'|'assignees'|'status'|'labels'|'repository'|'milestone'
                      |'text'|'number'|'date'|'single_select'|'iteration';
interface ProjectField { id; projectId; name; dataType: ProjectFieldType; position: number;
  options: {id: string; name: string; color: string; description: string}[] | null;   // single_select + status
  iterations: {startDate: string /*YYYY-MM-DD*/; duration: number /*days*/;
               iterations: {id: string; title: string; startDate: string; duration: number}[]} | null;
  createdAt; updatedAt }
interface ProjectView { id; projectId; number; name; layout: 'table'|'board'|'roadmap'; position: number;
  filter: string; groupByFieldId: ID|null; columnFieldId: ID|null /*board columns*/;
  sortBy: {fieldId: ID; direction: 'asc'|'desc'}[]; visibleFieldIds: ID[] /*ordered = column order*/;
  hiddenColumnIds: string[] /*board option ids hidden*/; dateFieldId: ID|null; /*roadmap*/
  createdAt; updatedAt }
interface ProjectItem { id; projectId; contentType: 'Issue'|'PullRequest'|'DraftIssue';
  issueId: ID|null; title: string|null /*draft only*/; body?: string|null /*draft only*/;
  assigneeIds: ID[] /*draft only*/; archived: boolean; position: string /*fractional key*/;
  viewPositions: Record<string /*viewId*/, string>; values: Record<string /*fieldId*/, Value>;
  creatorId: ID|null; createdAt; updatedAt }
// Value: text → string, number → number, date → "YYYY-MM-DD", single_select/status → option id,
// iteration → iteration id.
interface ProjectWorkflow { id; projectId; kind: 'item_added'|'item_reopened'|'item_closed'|'pr_merged'|'auto_add'|'auto_archive';
  enabled: boolean; config: {statusOptionId?: string; repoIds?: ID[]; filter?: string} ; updatedAt }
```

Ordering: items sort by `viewPositions[viewId] ?? position` (fractional keys,
base-62 `0-9A-Za-z`, compared bytewise, never ending in `0`).

### Private JSON API (`/_bgh/...`, cookie or token auth; mutations accept `X-Client-Tx`)

| Method / path | Body | Response |
|---|---|---|
| `GET /_bgh/owners/{owner}/projects?state=open\|closed\|all&q=` | | `{projects: Project[], users: User[]}` |
| `GET /_bgh/owners/{owner}/projects/{number}` | | snapshot (below) |
| `GET /_bgh/repos/{o}/{r}/projects` | | `{projects: Project[]}` linked to the repo or containing its items |
| `POST /_bgh/projects` | `{owner, title, shortDescription?, public?}` | `201` Project (with default fields + "View 1" table view) |
| `GET /_bgh/projects/{id}` | | snapshot |
| `PATCH /_bgh/projects/{id}` | `{title?, shortDescription?, readme?, public?, closed?}` | Project |
| `DELETE /_bgh/projects/{id}` | | `204` |
| `PUT/DELETE /_bgh/projects/{id}/repos/{repoId}` | | Project (link/unlink repo) |
| `POST /_bgh/projects/{id}/fields` | `{name, dataType, options?, iterations?}` | `201` ProjectField |
| `PATCH /_bgh/projects/{id}/fields/{fieldId}` | `{name?, options?, iterations?, position?}` | ProjectField |
| `DELETE /_bgh/projects/{id}/fields/{fieldId}` | | `204` (built-ins: `422`) |
| `POST /_bgh/projects/{id}/items` | `{issueId}` \| `{owner, repo, number}` \| `{draft: {title, body?}}` (+ `position?`) | `201` ProjectItem (`200` if the issue already is an item) |
| `PATCH /_bgh/projects/{id}/items/{itemId}` | `{archived?, position?, viewId?+viewPosition?, title?, body?, assigneeIds?, values?: {fieldId: Value\|null}}` | ProjectItem |
| `DELETE /_bgh/projects/{id}/items/{itemId}` | | `204` |
| `POST /_bgh/projects/{id}/views` | `{name?, layout?, ...view fields}` | `201` ProjectView |
| `PATCH /_bgh/projects/{id}/views/{viewId}` | any ProjectView field except ids | ProjectView |
| `DELETE /_bgh/projects/{id}/views/{viewId}` | | `204` (last view: `422`) |
| `PUT /_bgh/projects/{id}/workflows/{kind}` | `{enabled, config?}` | ProjectWorkflow |

Snapshot: `{project, fields, views, items, workflows, issues: Issue[], repos: Repo[],
users: User[], labels: Label[], milestones: Milestone[], role: 'read'|'write'|'admin'}` —
`issues`/`repos`/`labels`/`milestones` use the sync compact shapes and only cover readable
repositories referenced by items.

Errors are GitHub-style JSON (`422` with `errors[]`, `404`, `403`).

### Workflows-lite

Event listener `projects.workflows`:
* `auto_add` — on `IssueOpened` / `PullRequestOpened` (and `IssueEdited`)
  in a repo listed in `config.repoIds` whose issue matches `config.filter`
  (`is:issue`, `is:pr`, `label:x`, `-label:x`, `is:open`, plain words in the title), add it.
* `item_added` — set Status to `config.statusOptionId` when an item is added.
* `item_closed` / `pr_merged` / `item_reopened` — set Status on
  `IssueClosed`+`PullRequestClosed` / `PullRequestMerged` / `IssueReopened`+`PullRequestReopened`.
* `auto_archive` — archive items when closed/merged.

## Wiki (bgh-wiki)

Git-backed `{data_dir}/repos/{xx}/{repo_id}.wiki.git`, branch `master`.

* Smart HTTP: `/{owner}/{repo}.wiki.git/info/refs|git-upload-pack|git-receive-pack`
  (routed through bgh-repos' git routes, which delegate names ending in
  `.wiki`/`.wiki.git` to `bgh_wiki::git`). Same auth as repos. `has_wiki`
  false → 404. Push requires write permission, or any signed-in reader when the
  repo's `wikiAnyoneCanEdit` setting is on. Pushing to a wiki that does not exist yet creates it.
* Pages are files at the tree root (`Home.md`, `My-Page.md`); slug = filename
  without extension; title = slug with `-` → space. `_Sidebar` / `_Footer` are special.

### Private JSON API (`/_bgh/repos/{o}/{r}/wiki/...`)

| Method / path | Body | Response |
|---|---|---|
| `GET /wiki` | | `{exists, canEdit, anyoneCanEdit, home: "Home", pages: [{slug, title, path}], sidebar: Rendered\|null, footer: Rendered\|null}` |
| `GET /wiki/pages/{slug}?rev=sha` | | `{slug, title, path, format, raw, html, sha, commit, sidebar, footer}` |
| `GET /wiki/pages/{slug}/raw?rev=` | | `text/plain` page source |
| `POST /wiki/pages` | `{title, body, message?}` | `201` page |
| `PUT /wiki/pages/{slug}` | `{title?, body, message?, expectedCommit?}` | page (`409` when `expectedCommit` is stale) |
| `DELETE /wiki/pages/{slug}` | `{message?}` | `204` |
| `GET /wiki/pages/{slug}/history?page=&per_page=` | | `[Commit]` |
| `POST /wiki/pages/{slug}/revert` | `{sha, message?}` | page (restores the page as of commit `sha`) |
| `GET /wiki/compare/{base}...{head}?slug=` | | `{base, head, diff}` unified diff |
| `GET /wiki/history` | | `[Commit]` (whole wiki) |
| `GET /wiki/search?q=` | | `{results: [{slug, title, snippet}]}` |
| `GET/PATCH /wiki/settings` | `{anyoneCanEdit}` (admin) | `{anyoneCanEdit, hasWiki}` |

`Commit = {sha, message, author: {name, email, login|null, avatarUrl|null}, date}`;
`Rendered = {slug, html}`. Wiki links `[[Page Name]]` / `[[Text|Page Name]]`
and relative links resolve to `/{o}/{r}/wiki/{slug}`; links to missing pages get class `wiki-missing`.

## Migrations

* `1100_projects.sql` — projects, project_fields, project_views, project_items,
  project_item_values, project_workflows, project_linked_repos, project_counters.
* `1200_wiki.sql` — `repositories.wiki_anyone_can_edit`.

## Shared-code changes (additive)

* Workspace `Cargo.toml`, `bgh-server` (mount both crates).
* `bgh_core::sync`: scope-provider hook for bootstrap (see below).
* `bgh-git`: `RepoStore::wiki()` (same layout, `.wiki.git` suffix).
* `bgh-repos::git_http`: delegate `*.wiki(.git)` names to `bgh_wiki::git`.

## Known gaps / TODO

* Project collaborators / per-project roles beyond owner membership.
* Converting a draft into an issue (needs the issues create service from B3).
* GitHub REST/GraphQL Projects v2 API.
