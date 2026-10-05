Integration: ready
Rulesets UI (repo + org list/editor/insights, import/export, branch badges, mock); gate green after merging the integration branch; org pages need P23's endpoints and show "not available" until P23 lands.

# P24 — Rulesets UI (repo and org)

Branch `bgh/p24-rulesets-ui`. Built against GitHub's REST contract and
verified end to end against P23's backend (`bgh/p23-rulesets`) in a local,
unpushed merge. Against an integration branch without P23, repository
rulesets CRUD works and the org rulesets / Rule insights pages show "not
available on this server".

No backend changes, no migrations (3600–3699 unused).

## Pages

| Route | What |
|---|---|
| `/:owner/:repo/settings/rules` | Repository rulesets list (own + inherited org rulesets, "Managed by <org>" linking to the org settings), New ruleset menu (branch / tag / push), Import a ruleset (JSON file), per-row Export / Delete, enforcement pill |
| `…/settings/rules/new?target=branch\|tag\|push[&import=1]` | Editor (`import=1` prefilled from the imported file, created only on Create) |
| `…/settings/rules/:id` | Editor for an existing ruleset, Export button; org rulesets opened from a repo are read-only |
| `…/settings/rules/insights[?ref=&actor=&result=&period=&suite=]` | Rule insights: rule suites table filtered by ref, actor, result, time period; drawer with the per-rule evaluations |
| `/organizations/:org/settings/rules[/*]` | Same for organization rulesets (owners only; nav entry "Rulesets", `g r`), plus repository targeting and a repository filter in insights |

Editor sections: name; enforcement (active / evaluate / disabled); bypass
list (picker: repository roles admin/maintain/write, organization admin,
deploy keys, org teams, apps seen on the default branch's check runs plus
GitHub Actions, any user by login or app by numeric ID; per-actor bypass
mode always / pull_request / exempt); org repository targeting (all, dynamic
name include/exclude with "prevent renaming", explicit repository list;
`repository_property` kept verbatim) with a live preview of matching
repositories; ref targets (default branch, all, include/exclude patterns)
with a live preview of matching branches or tags (fnmatch semantics ported
from `protection::pattern_matches`, in `pages/rulesets/match.ts`); one form
per rule type:

* creation, update (fetch and merge), deletion, required_linear_history,
  required_signatures, non_fast_forward;
* pull_request (approvals 0–10, stale dismissal, code owners, last push,
  thread resolution, allowed merge methods);
* required_status_checks (check name autocomplete from the default branch's
  check runs / statuses, per-check app picker "Any source" or an app id,
  strict policy, do_not_enforce_on_create);
* commit_message / commit_author_email / committer_email / branch_name /
  tag_name patterns (operator, pattern, negate, description, a "try a value"
  tester);
* push rules: file_path_restriction, max_file_path_length,
  file_extension_restriction, max_file_size;
* merge_queue, required_deployments, workflows (org only: repository +
  path + ref), code_scanning (tools and thresholds).

Rules are offered by target (push rules only on push rulesets, tag_name
only on tags, workflows only for orgs); rules already present are always
shown, and unknown rule types are preserved on save.

Other UI changes:

* `BranchesSettings`: the classic rule's branch hint links to "Create a
  ruleset", and the section description links to rulesets.
* Branches list (`/:owner/:repo/branches`): each branch protected by active
  branch rulesets shows a badge with the ruleset name (or "N rulesets",
  tooltip lists them), linking to the ruleset for admins. Data: one list
  call plus one detail call per active branch ruleset (`activeBranchRulesets`,
  cached as `rulesets:active:{owner}/{repo}/`), matched client-side.

## Code

Bundle: all ruleset UI is in lazy route / section chunks; the initial bundle only gains the two `/organizations/:org/settings/rules[/*]` route entries (initial JS 142.3 KB gzip on this branch).


* `web/src/api/rulesets.ts` — typed REST calls and shapes (`repository-ruleset`,
  `rule-suite`, `rules/branches`), repo and org scopes.
* `web/src/pages/rulesets/` — `model.ts` (form ⇄ GitHub JSON for every rule
  type, validation, import/export, labels), `match.ts` (ref / repository
  matching), `RulesetsSection.tsx` (router + Rulesets/Insights tabs),
  `RulesetList.tsx`, `RulesetEditor.tsx` (lazy), `RuleForms.tsx`,
  `Targets.tsx`, `BypassList.tsx`, `RuleInsights.tsx` (lazy), `data.ts`.
* `web/src/pages/repo-settings/sections/RulesSettings.tsx`,
  `web/src/pages/orgsettings/OrgRulesetsPage.tsx`; routes and nav entries
  (additive).

## Mock backend

`web/src/mock/extra/rulesets.ts` (registered in `mock/extra/index.ts`):
repo and org rulesets CRUD with the backend's validation messages,
`includes_parents` / `targets`, `rules/branches/{b}`, rule suites (seeded,
filters, detail), `Link` pagination; branches mocks (`/branches`,
`/_bgh/.../branch-list`) mark ruleset-protected branches `protected`. Also
`GET /orgs/{org}/memberships/{user}` (appended, so a fuller mock wins) so
the org settings pages' owner check works in mock mode. Ruleset state is
in memory (lost on reload), like the other repo-settings mocks.

## Tests and verification

* `src/pages/rulesets/model.test.ts` — every rule type round-trips GitHub's
  JSON and its defaults serialize; full ruleset bodies (repo branch ruleset,
  org targeting variants, a Terraform `github_organization_ruleset`
  payload round trip, unknown rules / property conditions kept);
  validation; target filtering; import/export; matcher semantics.
* `src/mock/extra/rulesets.test.ts` — mock CRUD, validation, branch rules,
  org targeting, rule suites.
* `web/scripts/rulesets-smoke.mjs` (mock, Playwright) — create / edit /
  export / import / validation, insights + detail, branches badge, classic
  hint link, org push ruleset with repository targeting. All checks pass.
* `web/scripts/rulesets-real-smoke.mjs` (real server with P23 + `npm run
  dev`) — creates a `release/*` ruleset in the UI, then `git push
  HEAD:release/1.0` is rejected with `GH013: Repository rule violations
  found for refs/heads/release/1.0.`, `feature/x` still pushes, the rejected
  push appears in Rule insights with the failed "Restrict creations" rule.
  All checks pass.

## Notes for P23

* An early local test saw push rules (`max_file_size`) not enforced; P23
  could not reproduce it, and the cause was a stale pre-receive hook in a
  data dir reused across database resets in this container (not a P23 or
  P24 bug). P23 is making the hook install fail closed.

## Known gaps

* App picker lists apps seen in the default branch's check runs plus GitHub
  Actions; a real app directory waits for GitHub Apps (P17).
* Org ref-target preview can't list branches across repositories (shows
  patterns only); repository targeting has its own preview.
* No GraphQL ruleset types (P44).
