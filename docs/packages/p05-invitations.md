# P5 invitations — status

**Done.** Branch `bgh/p05-invitations`, self-integrated (fast-forward)
into `claude/sleepy-cray-9jj0t3` with the full gate green (fmt, clippy,
`cargo test --workspace`, web typecheck/lint/test/build). Scope: `docs/PHASE4_PLAN.md` §P5
(plus the §5 note "`billing_manager` and leave-org are in P5"). No
migrations (range 1700–1799 unused).

## Backend

* `POST /orgs/{org}/invitations` rejects `role: billing_manager` with a
  GitHub-shaped 422 (`OrganizationInvitation.role`, code `custom`, "role
  must be one of: admin, direct_member, reinstate"); `reinstate` maps to
  `direct_member`.
* `invitation_member_role` only maps `admin`/`direct_member`; pending
  lookups (`pending_invitation`, `GET /user/memberships/orgs`) only match
  granting roles, so a legacy `billing_manager` row never grants a
  membership (accept → 403 "no pending invitation").
* Removing the last owner (`DELETE /orgs/{org}/members|memberships/{me}`)
  is now a GitHub-shaped **403** "You cannot remove the last owner of an
  organization." (was 422). Demoting the last owner stays 422.
* New web endpoints (bgh-accounts `orgs.rs`, `web_router`):
  * `GET /_bgh/orgs/{org}/invitation` → `{state: pending|active,
    organization (organization-simple), organization_name, role:
    admin|member, invitation_id, inviter (simple-user), created_at,
    teams: [name]}`; 404 without an invitation (user id or verified
    email match).
  * `DELETE /_bgh/orgs/{org}/invitation` → 204: the invitee declines (all
    of their pending invitations to the org; audit
    `org.decline_invitation`); 404 when none.
  * `GET /_bgh/user/organizations` → `[{organization, organization_name,
    role, public, sole_owner, members_count}]` (one query; drives the
    settings page).
* bgh-repos `collaborators.rs`: inviting a collaborator now queues the
  `repo_invitation` email (link `{repo html}/invitations`, the routed
  page); the org invitation email already links `/orgs/{org}/invitation`.
  `html_url` of repository invitations verified (`/{o}/{r}/invitations`).
* Accepting a repo invitation emits `CollaboratorAdded` with the
  **inviter** as actor (GitHub `member` event semantics; the feed used to
  read "bob added bob").

## Web

* Routes (no repo layout, so private repos work before accepting):
  `/orgs/:org/invitation` → `pages/invitations/OrgInvitationPage`
  (inviter, org, role, teams; Join → `PATCH /user/memberships/orgs/{org}`,
  Decline → `DELETE /_bgh/orgs/{org}/invitation`) and
  `/:owner/:repo/invitations` → `RepoInvitationPage` (from
  `GET /user/repository_invitations`; Accept `PATCH`, Decline `DELETE`).
  Not-found and already-a-member states included.
* Dashboard banner (`InvitationsBanner`, also on the "no repositories"
  empty dashboard) from `GET /user/repository_invitations` +
  `GET /user/memberships/orgs?state=pending`.
* Signed-out invitation links: the shell's `/login?return_to=` redirect
  plus a notice on the login and sign-up pages ("… to accept your
  invitation to …"); sign-up keeps `return_to` on its "Sign in" links and
  shows the invite-only 403 message instead of the "sign up disabled"
  screen (only a closed instance disables the form).
* `/settings/organizations` (`sections/OrganizationSettings`, nav under
  Access): memberships with role/public pills, Make public/private
  (`PUT|DELETE /orgs/{org}/public_members/{me}`), Leave
  (`DELETE /orgs/{org}/memberships/{me}`, type-to-confirm; disabled with
  a reason for the sole owner), pending org invitations.
* API wrappers: `src/api/invitations.ts`; pure helpers
  `pages/invitations/model.ts`.
* Mock: `src/mock/extra/invitations.ts` (seeded `initech` org invitation
  materialized on accept, one repo invitation, memberships, leave,
  publicity).

## Tests

* `crates/bgh-accounts/tests/it/invitations.rs`: end to end, A invites B
  to an org (with a team) and a repo, the emails link to the routed
  pages, banner sources, invitation page shape, accept both → access; C
  declines both → invitations gone; billing_manager 422 plus a legacy
  row; leave removes access, sole owner 403, publicize,
  `/_bgh/user/organizations` shape. `orgs.rs` last-owner expectation
  updated to 403.
* Vitest: `pages/invitations/model.test.ts` (banner rows, lookup,
  labels, sole owner, return_to parsing), `mock/extra/invitations.test.ts`.
* Playwright smoke (real server, built web): signed-out link → login
  notice → return_to; banner; accept org + repo via the UI; publicize;
  leave; sole-owner Leave disabled; decline both.

## Shared-code changes

None outside bgh-accounts, bgh-repos and web (no `bgh-core` edits).

## Known gaps

* Email-addressed org invitations match only **verified** emails, so a
  user who signs up through an email invitation must verify the address
  before the invitation shows (the page says so).
* No `invitation` notification reason is produced yet (P60).
