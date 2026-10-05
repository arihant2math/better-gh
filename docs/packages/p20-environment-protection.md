Integration: in progress
Environment protection rules, deployment approvals, job deployments, deployment triggers, required_deployments.

# P20 — Environment protection rules and deployment approvals — status

Branch `bgh/p20-environment-protection`. Migration
`3200_environment_protection.sql` (range 3200–3299). Base: P19
(deployments API).

## REST endpoints (relative to `/api/v3`)

| Method + path | Who | What |
|---|---|---|
| `GET /repos/{o}/{r}/environments[/{name}]` | readers | environment JSON now carries `can_admins_bypass`, `protection_rules` (`wait_timer` {`wait_timer`}, `required_reviewers` {`prevent_self_review`, `reviewers: [{type: User\|Team, reviewer}]`}, `branch_policy`; ids derived `env.id*10+n`, node ids `NodeType::EnvironmentProtectionRule` = "Gate") and `deployment_branch_policy` (`{protected_branches, custom_branch_policies}` or `null`) |
| `PUT /repos/{o}/{r}/environments/{name}` | admins | body optional; `wait_timer` (0–43200), `prevent_self_review`, `can_admins_bypass` (default true), `reviewers` (≤ 6, `{type, id}`; users need read access, teams must belong to the owning org), `deployment_branch_policy` (exactly one of the two flags true, or `null` = any ref). Omitted fields are kept. 422 GitHub validation errors. Audited (`environment.update_protection_rule`) |
| `GET /repos/{o}/{r}/environments/{env}/deployment-branch-policies` | readers | `{total_count, branch_policies: [{id, node_id, name, type}]}`, `Link` pagination |
| `POST …/deployment-branch-policies` | admins | `{name, type: branch\|tag}` → 200; duplicate → 303 `Location` of the existing one; 404 unless the environment uses `custom_branch_policies` |
| `GET\|PUT\|DELETE …/deployment-branch-policies/{id}` | readers / admins | PUT renames (`{name}`) |
| `GET /repos/{o}/{r}/actions/runs/{id}/pending_deployments` | readers | one entry per environment a waiting job of the run waits for: `{environment {id,node_id,name,url,html_url}, wait_timer, wait_timer_started_at, current_user_can_approve, reviewers}` |
| `POST /repos/{o}/{r}/actions/runs/{id}/pending_deployments` | reviewers | `{environment_ids, state: approved\|rejected, comment}` → array of the reviewed jobs' `deployment` objects. 422 when nothing is pending, an id isn't pending in the run, or the caller may not review it |
| `GET /repos/{o}/{r}/actions/runs/{id}/approvals` | readers | `[{environments: [...], state, user, comment}]` oldest first |

Who may review an environment: a listed user or a member of a listed
team — not the run's triggering actor when `prevent_self_review` — or a
repository admin when `can_admins_bypass`.

## Engine (`bgh_actions::gates`)

* When `advance_run` materializes a job with `environment:` (string or
  `{name, url}`), `gates::job_gate` ensures the environment exists and:
  * branch policy (`protected`: classic rule or active ruleset on the
    branch, never tags; `custom`: fnmatch patterns per `branch`/`tag`; pull
    request runs use their head branch) → if not allowed the job is
    inserted completed/`failure` with GitHub's message (`Branch "x" is
    not allowed to deploy to production due to environment protection
    rules.`) in its log;
  * `wait_timer` > 0 or reviewers → job status `waiting` plus an
    `actions_job_gates` row (`wait_until`, `needs_review`); the run becomes
    `waiting` while nothing else is queued or running (check suite stays
    queued/in_progress). Runners only claim `queued` jobs and secrets are
    decrypted at claim time, so a waiting job never sees environment
    secrets.
* `gates::release_ready` (maintenance loop every 30 s, and after every
  approval) queues gates whose timer elapsed and whose review (if needed)
  approved them. Rejection completes the job as `failure` (log line
  "The deployment was rejected by …"); dependents are skipped as usual.
* Every non-blocked environment job creates a deployment through P19's
  service (creator `github-actions[bot]`, task `deploy`, ref = branch/tag,
  `run_id`/`job_id` set, `production_environment` for `production`) with
  status `queued`; claim → `in_progress`; completion → `success` /
  `failure` / `error` (cancelled), `log_url`/`target_url` = job page,
  `environment_url` = `environment.url` evaluated at completion against
  github/needs/matrix/inputs/vars/strategy (`StoredJob.environment_url`),
  `auto_inactive` on success.
* Events: `Event::DeploymentReview { repo_id, run_id, action
  (requested|approved|rejected), actor_id, reviewer_ids, payload }`
  (bgh-core, additive). Requested once per run and environment.
* Triggers: `on: deployment` and `on: deployment_status`
  (`trigger_events.rs` arms + `on_deployment`): workflows at the
  deployment's SHA, `GITHUB_REF` = `refs/heads|tags/<ref>` when such a ref
  exists. Deployments created by job tokens/the Actions bot never trigger
  workflows (loop guard).

## Notifications and webhooks

* Webhook `deployment_review` (requested / approved / rejected) with
  `environment, reviewers, since, workflow_job_run, workflow_run`
  (+ `requestor`, or `approver, comment, workflow_job_runs`). Removed from
  P10's not-producible list; sampled in the coverage test.
* Inbox + email: reason `approval_requested` (new `Reason` variant, web
  label "Approval requested") to the environment's reviewers (teams
  expanded), thread = the run's check suite, `EmailKind::DeploymentReview`.

## required_deployments (bgh-pulls evaluator)

* Ruleset rule `required_deployments` (`parameters.required_deployment_environments`)
  is accepted by `bgh-repos` rulesets (validated) and enforced.
* Classic protection: new column
  `branch_protections.required_deployment_environments`, settable through
  an extension key `required_deployment_environments` in `PUT
  /branches/{b}/protection` (GitHub's REST has no field; GraphQL calls it
  `requiredDeploymentEnvironments`); the legacy rule JSON's
  `required_deployments_enforcement_level` reflects it.
* Blockers: `Required deployment to "env" is expected.` / `… is in
  progress.` / `… has failed.` until the head commit's latest deployment
  to the environment is `success` (or `inactive` after success).

## Web

* Run page: "A deployment is waiting: production" banner for `waiting`
  runs (reviewers, wait timer) with a **Review deployments** dialog
  (environment checkboxes, comment, Reject / Approve and deploy) —
  `pages/actions/PendingDeployments.tsx`.
* Environment settings: `/:owner/:repo/settings/environments/:env/edit`
  (lazy chunk `EnvironmentConfig`): required reviewers (user or
  org/team), prevent self-review, wait timer, admin bypass, deployment
  branches (none / protected / selected) with branch and tag rules, plus
  the environment's secrets and variables. The environments list shows
  the number of rules and a "Configure" link.
* The PR's deployment state is P19's `DeploymentsBanner`; failing
  required deployments appear in the merge box requirements.
* Mock: environment protection fields, `PUT` parsing, branch policy
  routes, pending deployments (production in the seeded repos requires the
  viewer's review; the deploy workflow waits until approved);
  `mock/environments.test.ts`.
* Verified with Playwright in mock mode (light + dark): settings page,
  custom branch rule, waiting run banner, review dialog, approval.
* Initial bundle unchanged (143.3 KB gzip).

## Tables (migration 3200)

* `actions_environments` + `wait_timer`, `prevent_self_review`,
  `can_admins_bypass`, `branch_policy`.
* `actions_environment_reviewers(environment_id, position, user_id | team_id)`.
* `actions_environment_branch_policies(id, environment_id, name, type)`.
* `actions_job_gates(job_id PK, run_id, environment_id, wait_timer,
  wait_until, needs_review, review_state, released_at)`.
* `actions_deployment_reviews(id, run_id, user_id, state, comment,
  environment_ids, environment_names)`.
* `branch_protections.required_deployment_environments TEXT[]`.

## Shared-code changes (additive)

* `bgh_core::events::Event::DeploymentReview`.
* `bgh_core::node_id::NodeType::{DeploymentBranchPolicy, EnvironmentProtectionRule}`.
* `bgh_repos::protection::ProtectionRow.required_deployment_environments`;
  `rulesets.rs` `RULE_TYPES` + `required_deployments` normalization
  (P23's branch adds the same rule type: keep P23's version on merge).
* `bgh_notify`: `Reason::ApprovalRequested`, `EmailKind::DeploymentReview`,
  fanout arm, webhook arm.
* `bgh_pulls::protection::SourceRules.required_deployments` + evaluator.
* `engine.rs`: gate hook in `materialize`, `waiting` run status,
  `StoredJob.environment_url`; `server.rs`: deployment status on claim and
  completion.

## Known gaps / TODO

* Custom deployment protection rules (`deployment_protection_rule`
  webhook, GitHub Apps gates) are not implemented.
* `environment.url` cannot use `steps.*.outputs` (step outputs are not
  reported to the server); such expressions evaluate to an empty URL.
* Matrix jobs of one environment share one review (approving releases
  all of them), like GitHub; `max-parallel` is not re-applied when gated
  jobs are released.
* `gh run view` shows the `waiting` status (REST `status: waiting`); `gh
  run view` has no pending-deployment prompt in GitHub's CLI either.
* Classic `required_deployment_environments` has no web control yet
  (rulesets UI/P24 can add the `required_deployments` rule).
