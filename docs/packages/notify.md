# B5 notify — status

Branch `bgh/notify`, crate `bgh-notify`, migrations `0500-0599`.

## Status

Complete for the B5 scope below; tests green (`cargo test -p bgh-notify`:
unit + `notifications`, `webhooks`, `email`, `payloads` integration
suites), workspace clippy/tests green, merged with the integration branch.

## Implemented endpoints

Notifications (GitHub REST, need the `notifications` or `repo` scope):

* `GET/PUT /notifications` — `all`, `participating`, `since`, `before`,
  `page`/`per_page` (default/max 50); PUT `{last_read_at, read}` → 205.
* `GET/PUT /repos/{o}/{r}/notifications`
* `GET/PATCH/DELETE /notifications/threads/{id}` (PATCH = read → 205,
  DELETE = done → 204; done threads leave every listing and come back on
  new activity).
* `GET/PUT/DELETE /notifications/threads/{id}/subscription` (PUT
  `{"ignored": true}` mutes; DELETE unsubscribes until participating).
* `GET/PUT/DELETE /repos/{o}/{r}/subscription` (watching: row with
  `subscribed` / `ignored`; no row = participating). Maintains
  `repositories.watchers_count` (= `subscribers_count`) and syncs
  `viewerRepo {id, watching}` in `user:{id}`. **B2 (repos) should not
  register this route** (axum panics on duplicates); `/subscribers` and
  `/user/subscriptions` are left to B2 and read the same `watches` table.

Webhooks (repo admins with `repo` or `*:repo_hook`; org owners with
`admin:org_hook`):

* `/repos/{o}/{r}/hooks` GET/POST, `/hooks/{id}` GET/PATCH/DELETE
  (`events`, `add_events`, `remove_events`, `active`, `config`),
  `/hooks/{id}/config` GET/PATCH, `/hooks/{id}/pings` POST, `/hooks/{id}/tests`
  POST, `/hooks/{id}/deliveries` GET (cursor `v1_<id>`, `per_page`,
  `status=success|failure`, `redelivery`), `/deliveries/{id}` GET,
  `/deliveries/{id}/attempts` POST (202).
* Same under `/orgs/{org}/hooks` (no `tests`).
* Creating a hook pings it. Config: `url`, `content_type` json|form (form
  sends `payload=<urlencoded json>`), `secret` (shown as `********`),
  `insecure_ssl` "0"/"1". Events validated against GitHub's list (+ `*`).

Web client (`/_bgh`):

* `GET/PUT /_bgh/notifications/settings` — `{web: {reason: bool},
  email: {reason: bool}, email_enabled, notification_email (verified
  address), own_activity_email}`; synced as `notificationSettings` in
  `user:{id}`.
* `GET/POST /_bgh/notifications/unsubscribe?token=` — signed (HMAC,
  secret in `site_settings['notify.secret']`) thread or all-email tokens;
  GET is a confirmation page, POST (RFC 8058 one-click) applies.
* `GET/PUT/DELETE /_bgh/repos/{o}/{r}/issues/{number}/subscription` —
  subscribe button (reason `manual`).

## Notification fan-out (`fanout.rs`)

Listener `notify.notifications`. Per event: participants are subscribed
to the thread; recipients = direct reasons ∪ thread subscribers ∪ repo
watchers, minus the actor, ignored threads/repos, explicitly unsubscribed
threads (except direct reasons), non-`User`/suspended accounts and users
without read access (`perms::users_repo_permissions`, one query). Highest
ranked reason wins. Rows are upserted with one `INSERT … SELECT unnest`,
each recorded as a `notification` sync action (I/U, D when done); then one
`notify.email` job.

| event | reasons |
|-------|---------|
| IssueOpened (non-PR), PullRequestOpened | watchers `subscribed`; `assign` (assignees), `review_requested` (users + team members), `mention`, `team_mention` (teams of the owning org with notifications enabled, incl. child teams); author subscribed `author` |
| IssueCommentCreated, PullRequestReviewSubmitted (non-pending; mentions in its inline comments count), PullRequestReviewCommentCreated (replies / review-less comments only) | subscribers + watchers + mentions; commenter subscribed `comment`; `latest_comment_url` set |
| IssueClosed/Reopened, PullRequestClosed (non-merged)/Reopened/Merged | subscribers + watchers; actor subscribed `state_change` |
| PullRequestSynchronized | thread subscribers |
| IssueAssigned | assignee `assign` |
| PullRequestReviewRequested | reviewer / team members `review_requested` |
| IssueEdited / PullRequestEdited | title propagated to threads; newly @mentioned users (`changes.body.from`) |
| ReleasePublished | watchers (subject `Release`) |
| CheckSuiteUpdated `completed` with failure-like conclusion | actor `ci_activity` (subject `CheckSuite`) |

**Emit exactly one of `PullRequestMerged` (merges) or `PullRequestClosed`
(closes without merge)** — both map to webhook `closed`, so emitting both
duplicates deliveries.

## Webhook delivery

* Listener `notify.webhooks`: cheap DB check for active hooks (repo,
  owning org, site-wide `repo_id IS NULL AND org_id IS NULL`) subscribed
  to the mapped event names, then `payloads::for_event` builds payloads
  once; one `webhook_deliveries` row (exact body in `payload_raw`) + one
  `notify.deliver_webhook` job per hook, in one transaction.
* Job: headers `X-GitHub-Event`, `X-GitHub-Delivery`, `X-GitHub-Hook-ID`,
  `X-GitHub-Hook-Installation-Target-ID/Type`, `X-Hub-Signature(-256)`,
  `User-Agent: GitHub-Hookshot/bgh-<ver>`; timeout `BGH_WEBHOOK_TIMEOUT_SECS`;
  no redirects, no proxy; response (64 KiB) recorded; hook `last_response`
  updated. 5xx/408/429/timeouts/connection errors retry with the queue's
  backoff (5 attempts); 4xx and blocked targets are final.
* SSRF (`webhooks/ssrf.rs`): http(s) only; loopback, RFC 1918, ULA,
  link-local (metadata), CGNAT, multicast, reserved, NAT64 and v4-mapped
  internal addresses are refused at creation (literal hosts) and at
  delivery (resolved, then the client is pinned to the checked addresses).
  Allow-list: `BGH_WEBHOOK_ALLOWED_HOSTS` and `site_settings
  ['webhooks.allowed_hosts']` (JSON array): hosts, `*.suffix`, IPs, CIDRs, `*`.
* Payloads (`payloads/`, reusable `pub` builders): push (+ create/delete,
  commits via one `git log` per ref), issues, issue_comment, pull_request,
  pull_request_review, pull_request_review_comment, release, star, watch,
  fork, member, repository, label, milestone, status, check_run,
  check_suite, workflow_run, organization (member_added), ping, test push.

## Email

* `mail.send` job (any crate: `bgh_core::mail::enqueue`) → SMTP via lettre
  (`BGH_SMTP_URL`, pooled) or the dev transport (log + `{data_dir}/mail/*.eml`).
* `notify.email`: per recipient, honouring settings and read access;
  GitHub-style subjects (`[o/r] Title (Issue #1)`, `Re:` for follow-ups),
  `Message-ID`/`In-Reply-To`/`References` threading, `List-ID`,
  `List-Unsubscribe(-Post)`, `X-GitHub-Reason/Sender/Recipient`; text +
  HTML (rendered markdown) bodies.

## Shared-code changes (additive)

* `bgh_core::events::Event`: new variants consumed by notifications and
  webhooks — please emit them from the owning crate:
  * issues (B3): `IssueAssigned`, `IssueUnassigned`, `IssueLabeled`,
    `IssueUnlabeled`, `IssueMilestoned`, `IssueDemilestoned`, `IssueLocked`,
    `IssueUnlocked`, `IssueDeleted { issue: <REST JSON> }`, `LabelCreated`,
    `LabelEdited { changes }`, `LabelDeleted { label: <REST JSON> }`,
    `MilestoneCreated/Edited/Closed/Opened/Deleted`. Assign/label events
    apply to PRs too (`issue_id` = the PR's issue id).
  * pulls (B4): `PullRequestEdited { changes }`, `PullRequestReadyForReview`,
    `PullRequestConvertedToDraft`, `PullRequestReviewRequested` /
    `PullRequestReviewRequestRemoved` (`reviewer_id` xor `team_id`),
    `PullRequestReviewDismissed`, `PullRequestReviewComment{Created,Edited,Deleted}`,
    `CommitStatusCreated`, `CheckRunUpdated { action }`,
    `CheckSuiteUpdated { action }` (`completed` → `ci_activity` notification).
  * repos (B2): `RepositoryRenamed { old_name }`, `RepositoryArchived`,
    `RepositoryUnarchived`, `RepositoryPublicized`, `RepositoryPrivatized`,
    `RepositoryForked { fork_id }`, `StarCreated`, `StarDeleted`,
    `CollaboratorAdded/Edited/Removed`.
  * releases (B6): `ReleaseCreated`, `ReleaseEdited { changes }`,
    `ReleaseDeleted { release: <REST JSON> }` (plus existing `ReleasePublished`).
  * actions (B10): `WorkflowRunUpdated { action, workflow_run, workflow }`.
* `bgh_core::markdown::mentions(text) -> Mentions { users, teams }`.
* `bgh_core::perms::users_repo_permissions(db, repo, user_ids)` — batched
  "which of these users can read this repo".
* `bgh_core::mail`: `Email`, `SendEmail` job (`mail.send`, handled by
  bgh-notify), `mail::enqueue`, account templates (`verify_email`,
  `password_reset`, `password_changed`, `org_invitation`, `repo_invitation`).
* `Config`: `smtp_url` (`BGH_SMTP_URL`), `mail_from` (`BGH_MAIL_FROM`),
  `webhook_allowed_hosts` (`BGH_WEBHOOK_ALLOWED_HOSTS`),
  `webhook_timeout_secs` (`BGH_WEBHOOK_TIMEOUT_SECS`).

## Jobs and listeners

Jobs `mail.send`, `notify.email`, `notify.deliver_webhook`; listeners
`notify.notifications`, `notify.webhooks`. Also additive:
`Pagination::with_default_per_page` in bgh-core.

## Known gaps / TODO

* Reply-by-email (optional) not implemented: creating comments belongs to
  bgh-issues; an inbound endpoint could call a pub service fn from there.
* Email digests not implemented (one email per activity).
* No GHES global hooks API (`/admin/hooks`); dispatch already delivers to
  site-wide rows (`repo_id` and `org_id` NULL) if B7 adds the endpoints.
* No `meta` (hook deleted) event; no automatic pruning of old
  `webhook_deliveries` (index on `created_at` exists for a cleanup job).
* Delete events (`IssueCommentDeleted`, review comment deleted) send a
  minimal comment object (row is gone); `changes` for comment edits is `{}`.
* Listeners are in-process/best-effort (per architecture); a crash between
  commit and fan-out loses that notification (deliveries are durable once
  queued).

## Migrations

* `0500_notify.sql`: notification columns (`latest_comment_type`,
  `subject_key`, `last_actor_id`) + indexes, `notification_settings`,
  webhook `creator_id`, delivery columns (`url`, `payload_raw`,
  `content_type`, `attempts`, `error`, `throttled_at`).
