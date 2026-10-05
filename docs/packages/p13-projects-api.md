# P13 projects-api — status

**Done.** Branch `bgh/p13-projects-api`. Scope: `docs/PHASE4_PLAN.md` §2
P13 (no §5 quick fixes are assigned to P13). Migration: `2500`.

Projects v2 now have GitHub's public APIs: the REST `projectsV2`
endpoints in `bgh-projects` and the GraphQL `ProjectV2` surface in
`bgh-graphql`. Both call the same service functions as the private web
API, so permissions (`bgh_projects::access`), sync records, workflows and
validation are shared.

## REST (`crates/bgh-projects/src/rest.rs`)

Same set for `/orgs/{org}/projectsV2` and `/users/{username}/projectsV2`
(the wrong kind of account is a `404`):

| Method / path | Response |
|---|---|
| `GET …/projectsV2?q=&per_page=&before=&after=` | `[projects-v2]` (newest number first; `q`: `is:open`/`is:closed` + title words) |
| `GET …/projectsV2/{n}` | `projects-v2` |
| `GET …/projectsV2/{n}/fields` | `[projects-v2-field]` (position order) |
| `GET …/projectsV2/{n}/fields/{field_id}` | `projects-v2-field` |
| `GET …/projectsV2/{n}/items?q=&fields=` | `[projects-v2-item-with-content]` |
| `POST …/projectsV2/{n}/items` `{type, id}` \| `{type, owner, repo, number}` | `201` `projects-v2-item-simple` (`200` when already on the project) |
| `GET …/projectsV2/{n}/items/{item_id}?fields=` | `projects-v2-item-with-content` |
| `PATCH …/projectsV2/{n}/items/{item_id}` `{fields: [{id, value}]}` | `projects-v2-item-with-content` (with the patched fields) |
| `DELETE …/projectsV2/{n}/items/{item_id}` | `204` |
| `POST /orgs/{org}/projectsV2/{n}/drafts`, `POST /user/{user_id}/projectsV2/{n}/drafts` `{title, body?}` | `201` `projects-v2-item-simple` |

* Shapes per GitHub's OpenAPI description: `projects-v2` (`description` =
  readme, `state`, `deleted_*`/`latest_status_update` null, `is_template`
  false), fields (`status` is reported as `single_select`; options and
  iteration titles are `{raw, html}`; iteration fields have
  `configuration {start_day, duration, iterations[{…, completed}]}`),
  items (`content` = REST `issue` JSON incl. `repository`, or the draft
  `{id, node_id (DI_…), title, body, user, …}`; `content: null` when the
  repository isn't readable; `archived_at`; `item_url`).
* `fields=1,2` / `fields[]=1&fields[]=2` selects field values (default:
  Title only). Values: title `{raw, html, number, url, issue_id, state,
  state_reason, is_draft}`, assignees `[simple-user]`, labels `[label]`,
  milestone, repository, single select / iteration option objects,
  text/number/date raw values, `null` when unset.
* `PATCH` values: number fields accept numeric strings (`"3"`), `null`
  clears; invalid values → `422`.
* Pagination: GitHub's cursor style (`before`/`after`/`per_page`, default
  30, max 100, `Link` `rel="next"|"prev"`; bad cursor → `422`).
* Item filter `q` (also GraphQL `items(query:)`), `filter_items`:
  `is:issue|pr|draft|open|closed|archived`, `label:`, `assignee:`,
  `repo:`, `milestone:`, `no:<label|assignee|milestone|field>`,
  `<field name>:<value>` (single select option name, iteration title,
  text/number/date), title words; `-` negates, commas OR values.
  **Archived items are excluded unless the query contains `is:archived`.**
* Permissions: read = project visible (owner role or public, tokens need
  `read:project` for private projects; else `404`); writes need the
  project write role (`403` for readers) and, for issues, read access to
  the issue's repository.

## GraphQL (`crates/bgh-graphql/src/model/project.rs`, `mutation/projects.rs`)

* Types: `ProjectV2` (`id`, `databaseId`, `number`, `title`,
  `shortDescription`, `readme`, `public`, `closed`, `closedAt`,
  `template`, `url`, `resourcePath`, `owner: ProjectV2Owner`, `creator`,
  `createdAt`/`updatedAt`, `viewerCanUpdate/Close/Reopen`,
  `fields`/`field(name)`, `items(query:)`, `views`/`view(number)`,
  `repositories`, `teams`); `ProjectV2Field`, `ProjectV2SingleSelectField`
  (`options`), `ProjectV2IterationField` (`configuration { duration
  startDay iterations completedIterations }`) in the
  `ProjectV2FieldConfiguration` union; `ProjectV2Item` (`type`, `content:
  DraftIssue | Issue | PullRequest`, `fieldValues`, `fieldValueByName`,
  `isArchived`, …); field values Text/Number/Date/SingleSelect/Iteration/
  Label/Milestone/Repository/User/PullRequest/Reviewer (`PullRequest` and
  `Reviewer` values are never produced: no such fields yet);
  `ProjectV2View`; `DraftIssue`.
* Entry points: `User/Organization.projectV2(number)` (NOT_FOUND when
  missing) and `projectsV2(query, orderBy)`, `Repository.projectsV2`
  (linked or holding its issues), `Issue/PullRequest.projectItems`
  (`includeArchived`, default true) and `projectsV2`,
  `Organization.viewerCanCreateProjects`, `node()` for projects, items,
  fields, views and draft issues, and the new root `resource(url:)`
  (issues, PRs, repositories, users/orgs, projects; any host).
* Mutations: `createProjectV2` (+ `repositoryId`/`teamId`),
  `updateProjectV2`, `deleteProjectV2`, `addProjectV2ItemById`
  (idempotent), `addProjectV2DraftIssue`, `updateProjectV2DraftIssue`,
  `updateProjectV2ItemFieldValue` (exactly one of
  text/number/date/singleSelectOptionId/iterationId),
  `clearProjectV2ItemFieldValue`, `deleteProjectV2Item`,
  `archiveProjectV2Item`/`unarchiveProjectV2Item`,
  `updateProjectV2ItemPosition` (`afterId`, null = top),
  `createProjectV2Field` (TEXT/NUMBER/DATE/SINGLE_SELECT/ITERATION),
  `deleteProjectV2Field`, `linkProjectV2ToRepository`/`unlink…`,
  `linkProjectV2ToTeam`/`unlink…`.
* `createIssue(projectV2Ids:)` adds the new issue to the projects (all ids
  resolved first); `createPullRequest(projectV2Ids:)` too (bgh extension).
* Node ids: legacy format with new `NodeType`s `ProjectV2`,
  `ProjectV2Item`, `ProjectV2Field`, `ProjectV2View`, `DraftIssue`; draft
  issue ids carry GitHub's `DI_` prefix (`gh project item-edit` checks it).

## Tests

* `cargo test -p bgh-projects`: `tests/it/rest.rs` (shapes, visibility,
  token scopes, cursor pagination, fields, items CRUD, filters, archived,
  drafts, redaction of unreadable issues, reader 403s).
* `cargo test -p bgh-graphql`: `tests/it/projects.rs` replays gh 2.89's
  project queries/mutations (create, list, field-list/create/delete,
  item-add via `resource(url:)`, item-create, item-edit incl. drafts,
  item-list, item-archive, close, delete, `gh issue view` projectItems),
  the actions/add-to-project fixture (`getProject` +
  `addIssueToProject` + `addDraftIssueToProject`), `createIssue
  projectV2Ids`, repo/team links and outsider visibility.
* `scripts/gh-compat.sh`: new `-- projects` section — `gh project
  create/list/view/field-list/field-create/item-add/item-list/item-edit/
  item-create/item-archive/close` and `gh issue create --project`
  (checked via item-list). 54/54 pass.

## Migration `2500_projects_api.sql`

* `project_items.archived_at` (backfilled from `updated_at`); set/cleared
  by item updates and the auto-archive workflow.
* `project_linked_teams (project_id, team_id)`.

## Shared-code changes (additive)

* `bgh_core::node_id`: `NodeType::{ProjectV2, ProjectV2Item,
  ProjectV2Field, ProjectV2View, DraftIssue}`; `DI_` prefix for draft
  issues in `encode`/`decode_raw`.
* `bgh-graphql/src/scalars.rs`: scalars now keep GitHub's exact names —
  `URI` and `HTML` were exposed as `Uri`/`Html`, so variables declared
  `$url: URI!` failed with "Unknown type". New `Date` scalar.
* `bgh-graphql`: `query.rs` one `node()` arm + `resource(url:)`;
  `model/misc.rs` lost the v2 stubs; `actor.rs`/`repo.rs`/`issue.rs`
  resolvers now real; new dependency on `bgh-projects`.
* `bgh-projects`: depends on `bgh-issues` (issue JSON for item content);
  handler bodies split into reusable service fns
  (`projects::{create_project, update_project, delete_project,
  link_repo_row, link_team_row}`, `items::{add_item, update_item,
  delete_item, move_item}`, `fields::{create_field, delete_field}`);
  `filter::tokens` is `pub(crate)`.

## Known gaps

* Linked pull requests, reviewers, parent issue, sub-issue progress and
  issue type fields don't exist in bgh-projects, so those values are
  never returned.
* REST item `content` for pull requests uses the REST `issue` shape (with
  `pull_request`), not `pull-request-simple`.
* REST views (`POST …/views`, `GET …/views/{n}/items`) and REST field
  creation are not implemented (GraphQL `createProjectV2Field` is).
* `orderBy` on fields/items/field values is accepted and ignored
  (position order).
* Status updates (`latest_status_update`) and templates are not modelled.
