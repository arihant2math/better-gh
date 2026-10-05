Integration: landed
P29 runners: OS/arch, runner groups (org + site), site runners, JIT configs, admin runner UI.

# P29 — Runners

Branch `bgh/p29-runners`. Crate `bgh-actions` (+ `bgh-runner`), web.
Migration `4100_runner_groups.sql`.

## Implemented

* **OS / arch.** `bgh-runner register` reports the host OS/arch
  (`--os`/`--arch` override); `RegisterRequest` gained optional `os`,
  `arch`, `runner_group` (serde default, additive). The server normalizes
  (`darwin`→`macOS`, `aarch64`→`ARM64`, ...; 422 on unknown) and stores
  `actions_runners.os` / new `arch`, system labels
  `self-hosted, <os>, <arch>` (lower-cased, matching is case-insensitive).
  `RunnerConfig.os/arch` (default: host) drive `RUNNER_OS`, `RUNNER_ARCH`
  and `runner.os/arch`. The built-in runner records the host OS/arch.
* **Runner groups** (`api/runner_groups.rs`), GitHub shapes:
  `GET|POST /orgs/{org}/actions/runner-groups` (`visible_to_repository`),
  `GET|PATCH|DELETE …/{id}`, `GET|PUT …/{id}/repositories`,
  `PUT|DELETE …/{id}/repositories/{repository_id}`, `GET|PUT
  …/{id}/runners`, `PUT|DELETE …/{id}/runners/{runner_id}`. Visibility
  `all|selected|private`, `allows_public_repositories` (default false for
  new groups, true for the default group), `restricted_to_workflows` +
  `selected_workflows` (`owner/repo/path[@ref]`; ref matches the run's
  ref, branch or sha). Each scope has a default group (created on demand;
  can't be renamed or deleted; deleting a group returns its runners to
  the default group). Org admins only (members 403, others 404). Audited
  (`runner_group.*`).
* **Job matching** (`server::GROUP_ALLOWS_JOB` in `try_acquire`): a
  runner with a group only takes jobs its group allows. Repository runners
  and the built-in runner have no group (unrestricted).
* **Site runners** (`api/site_runners.rs`, site admins):
  `GET /_bgh/admin/actions/runners` (`status=online|offline|busy|idle`,
  `q=`; adds `scope`, `owner`, `repository`, `builtin`, `arch`,
  `runner_group_name`, `last_seen_at`, `created_at`), `DELETE …/{id}`
  (422 busy/built-in), `POST …/registration-token` / `remove-token`
  (tokens with `repo_id` and `org_id` NULL → site runners in site group
  1), `POST …/generate-jitconfig`, `GET /_bgh/admin/actions/queue`
  (`status=queued|in_progress`), site runner groups under
  `/_bgh/admin/actions/runner-groups` in the GHES enterprise shape
  (`selected_organizations_url`, `/organizations` sub-resource with
  `selected_organization_ids`; visibility `all|selected`).
* **JIT**: `POST /repos/{o}/{r}/actions/runners/generate-jitconfig`,
  `POST /orgs/{org}/actions/runners/generate-jitconfig` → 201
  `{runner, encoded_jit_config}` (409 on duplicate name, 422 without
  labels / bad group). `encoded_jit_config` uses GitHub's container
  (base64 JSON of base64 files): `.runner` with the official keys
  (`agentId`, `agentName`, `poolId`, `serverUrl`, `gitHubUrl`,
  `workFolder`, `ephemeral`), `.credentials` with scheme `BghRunnerToken`
  (`protocol::JitConfig`). P86 should replace the credentials with the
  official OAuth/RSA scheme. Ephemeral/JIT runners take at most one job
  and are deleted when it completes; `bgh-runner` tolerates the 401 on
  its final unregister.
* **Web**: `/site-admin/actions/runners` (Runners / Queue / Runner groups
  tabs, new runner dialog with registration token or JIT config, remove),
  `/organizations/:org/settings/actions/runner-groups[/:id]`; repo/org
  Runners list shows OS · arch and the group. All lazy chunks; mock
  endpoints in `web/src/mock/extra/runners.ts` (mock viewer is now a site
  admin).

## Tests

`crates/bgh-actions/tests/it/runner_groups.rs`: macOS/ARM64 runner matches
`runs-on: [self-hosted, macOS]` and sees `RUNNER_OS=macOS`; group CRUD and
JSON shapes, pagination, validation; a group restricted to repo A skips
repo B; public-repo and workflow allowlist; JIT runner runs one job then
is gone; ephemeral runner takes one job; site runners/groups/queue/JIT.
`protocol::jit_tests` round-trip. Web: `web/src/mock/extra/runners.test.ts`,
Playwright smoke in mock mode.

## Shared-code changes

None outside `bgh-actions` and `web/` (additive routes in `lib.rs`:
two `.route` + two `.merge`).

## Gaps / TODO

* `/enterprises/{e}/actions/runners*` aliases are not mounted (site API is
  `/_bgh/admin/actions`).
* Official actions/runner wire protocol (P86).
* The built-in runner keeps `BGH_ACTIONS_RUNNER_LABELS` as is (defaults
  `linux,x64`) even on other hosts; only its `os`/`arch` columns follow the
  host.
