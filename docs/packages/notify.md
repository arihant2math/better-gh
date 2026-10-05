# B5 notify — status

Branch `bgh/notify`, crate `bgh-notify`, migrations `0500-0599`.

## Status

In progress. Shared additions landed first so other packages can emit the
events notify consumes.

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

## Migrations

* `0500_notify.sql`: notification columns (`latest_comment_type`,
  `subject_key`, `last_actor_id`) + indexes, `notification_settings`,
  webhook `creator_id`, delivery columns (`url`, `payload_raw`,
  `content_type`, `attempts`, `error`, `throttled_at`).
