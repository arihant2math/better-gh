# P21 — Notification privacy, retention and polling

Status: **done**, self-integrated into `claude/sleepy-cray-9jj0t3` (branch
`bgh/p21-notification-privacy`).

## What exists

### Privacy

* SQL function `bgh_can_read_repo(uid, rid)` (migrations/3300): the SQL twin
  of `perms::repo_permissions` ≥ read (public, owner, site admin,
  collaborator, org admin / member with a base permission ≠ none, team
  grants incl. parent teams). Internal repositories count as private, like
  the rest of perms.
* `notification` sync shape (`bgh_core::sync::shapes`) loads only rows the
  holder can read: bootstrap, partial sync and deltas. A delta for an
  unreadable row records nothing.
* `fanout::retitle` updates (and syncs) only rows whose holders can read.
* Listener `notify.access` (`bgh-notify/src/privacy.rs`) deletes the
  unreadable threads and records `D` sync actions (done rows excepted, they
  are already gone client-side) on: `AccessChanged`, `OrgMemberRemoved`,
  `TeamMemberRemoved`, `TeamRepoRemoved`, `TeamDeleted`, `TeamEdited`,
  `OrganizationChanged` (base permission), `RepositoryPrivatized`,
  `RepositoryTransferred`, `UserAccountChanged`. Batched (1000 rows / tx).
* The retention pass runs a full sweep of threads in non-public repos as a
  backstop for missed events.

### Retention (`notify.retention` service, hourly, pg advisory-lock leader)

Site settings section `retention` (`bgh_core::settings::RetentionSettings`,
validated in bgh-admin, edited on the admin Site settings page → "Data
retention"); window 0 = keep forever:

| Setting | Default | Effect |
|---|---|---|
| `enabled` | true | run hourly |
| `notifications_days` | 150 | delete threads not updated since (sync `D`) |
| `webhook_payload_days` | 30 | `webhook_deliveries` lose `payload_raw`, `request_payload`, `response_body`, `response_headers` |
| `webhook_delivery_days` | 90 | delete delivery rows |
| `activity_days` | 90 | delete `activity_events` |
| — | — | expired `sessions` are always deleted |

All deletes batch 1000 rows per statement. `POST /_bgh/admin/retention/run`
(site admin) runs a pass and returns the report
`{ran, notifications, unreadable_notifications, webhook_payloads,
webhook_deliveries, activity_events, sessions}`. Redelivering a delivery
whose payload was dropped → 422.

### Polling headers

`bgh_core::polling` (new, additive): `http_date`, `not_modified`,
`with_headers`, `Polled<T>` and the `conditional` route layer.

* `GET /notifications`, `GET /repos/{o}/{r}/notifications`: `Last-Modified`
  = newest `notifications.changed_at` of the user's threads (any state;
  column maintained by a `BEFORE UPDATE` trigger), `If-Modified-Since` → 304
  before building the list, `X-Poll-Interval: 60` on 200 and 304.
* Events API timelines (`/events`, `/repos/{o}/{r}/events`,
  `/networks/...`, `/orgs/{org}/events`, `/users/{u}/events[...]`,
  `/users/{u}/received_events[...]`): `X-Poll-Interval: 60`,
  `Last-Modified` = newest event on the page, `If-Modified-Since` → 304 via
  `polling::conditional`.

## Migrations

`3300_notification_privacy.sql`: `bgh_can_read_repo`, `notifications.changed_at`
+ trigger, indexes `notifications (user_id, changed_at DESC)`,
`notifications (repo_id, user_id)`, `notifications (updated_at)`,
partial `webhook_deliveries (created_at) WHERE payload_raw <> ''`.

## Shared-code changes (additive)

* `bgh-core`: new `polling.rs`; `settings.rs` new `retention` section;
  `sync/shapes.rs` notification readability filter.
* `bgh-admin/src/settings.rs`: retention bounds (≤ 36500 days).
* `bgh-search`: Events API handlers return `Polled<Page<_>>`; events routes
  get the `conditional` layer.
* Web: admin settings "Data retention" section (`settingsForm.ts`,
  `settingsSections.tsx`, `api.ts`), vitest in `settingsForm.test.ts`.
  No mock change: the admin pages have no mock backend.
* docs/SYNC_PROTOCOL.md §scopes note on notification readability.

## Tests

* `crates/bgh-notify/tests/it/privacy.rs`: collaborator removal via REST
  (rows + bootstrap + REST + `D` action, later retitle reaches only
  readers); access lost without an event (shape hides, retitle skips,
  retention sweep deletes); team (child-team) removal and
  visibility → private; event → target mapping; retention windows incl.
  stripped-delivery 422 and the admin trigger; notifications polling.
* `crates/bgh-search/tests/it/activity.rs` `timelines_support_polling`.
* `bgh_core::polling` unit tests.
* Web: vitest for the retention form; Playwright check of the Site settings
  "Data retention" section against a real server (render + save).
* Gate: fmt, clippy, `cargo test --workspace`, web typecheck/lint/test/build,
  `api-smoke.sh` (45/45) and `gh-compat.sh` (55/55) green.

## Known gaps

* HTTP dates have one-second resolution: a change in the same second as a
  poll's response is only seen on the next change (same as GitHub).
* Deleting a thread for access loss doesn't move `Last-Modified` of the
  remaining list (the row is gone); the next change does.
* Thread subscriptions of repositories the user lost access to are kept
  (they generate nothing: fan-out checks read access).
* Global (site) hook deliveries are not covered; they get their own table
  with P63.
