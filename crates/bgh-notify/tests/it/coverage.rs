//! Webhook coverage: every event name the hook validator accepts is either
//! produced (a domain event maps to it via `payloads::event_names` *and*
//! some crate emits that domain event) or explicitly listed as not
//! producible yet. New webhook-able actions must be added here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use bgh_core::events::{Event, PushEvent, RefUpdate, ZERO_SHA};
use bgh_notify::payloads::event_names;
use bgh_notify::webhooks::EVENTS;
use serde_json::json;

/// Accepted by the validator, no producer yet (feature missing).
const NOT_PRODUCIBLE_YET: &[&str] = &[
    "branch_protection_configuration",
    "code_scanning_alert",
    "custom_property",
    "custom_property_values",
    "dependabot_alert",
    "deployment_protection_rule",
    "deployment_review",
    "discussion",
    "discussion_comment",
    "github_app_authorization",
    "installation",
    "installation_repositories",
    "installation_target",
    "issue_dependencies",
    "marketplace_purchase",
    "merge_group",
    "org_block",
    "page_build",
    "personal_access_token_request",
    "project",
    "project_card",
    "project_column",
    "projects_v2",
    "projects_v2_item",
    "registry_package",
    "repository_advisory",
    "repository_import",
    "repository_vulnerability_alert",
    "secret_scanning_alert",
    "secret_scanning_alert_location",
    "security_advisory",
    "security_and_analysis",
    "sponsorship",
    "workflow_dispatch",
];

/// Produced by bgh-notify itself, not from a domain event: `ping` on hook
/// creation / `POST .../pings`, `meta` when a hook is deleted.
const HOOK_LIFECYCLE: &[&str] = &["ping", "meta"];

/// One instance of every domain event variant that maps to a webhook.
fn samples() -> Vec<Event> {
    let sha = "a".repeat(40);
    let v = json!({});
    vec![
        Event::Push(PushEvent {
            repo_id: 1,
            pusher_id: Some(1),
            updates: vec![
                RefUpdate {
                    old: ZERO_SHA.into(),
                    new: sha.clone(),
                    refname: "refs/heads/a".into(),
                },
                RefUpdate {
                    old: sha.clone(),
                    new: ZERO_SHA.into(),
                    refname: "refs/heads/b".into(),
                },
            ],
            origin: None,
        }),
        Event::RepositoryCreated {
            repo_id: 1,
            actor_id: 1,
        },
        Event::RepositoryDeleted {
            repo_id: 1,
            owner_id: 1,
            full_name: "a/b".into(),
            actor_id: 1,
        },
        Event::RepositoryEdited {
            repo_id: 1,
            actor_id: 1,
            changes: v.clone(),
        },
        Event::RepositoryRenamed {
            repo_id: 1,
            actor_id: 1,
            old_name: "x".into(),
        },
        Event::RepositoryTransferred {
            repo_id: 1,
            actor_id: 1,
            old_owner_id: 2,
        },
        Event::RepositoryArchived {
            repo_id: 1,
            actor_id: 1,
        },
        Event::RepositoryUnarchived {
            repo_id: 1,
            actor_id: 1,
        },
        Event::RepositoryPublicized {
            repo_id: 1,
            actor_id: 1,
        },
        Event::RepositoryPrivatized {
            repo_id: 1,
            actor_id: 1,
        },
        Event::RepositoryStarred {
            repo_id: 1,
            actor_id: 1,
            starred: true,
        },
        Event::RepositoryStarred {
            repo_id: 1,
            actor_id: 1,
            starred: false,
        },
        Event::RepositoryForked {
            repo_id: 1,
            fork_id: 2,
            actor_id: 1,
        },
        Event::CollaboratorAdded {
            repo_id: 1,
            user_id: 2,
            actor_id: 1,
            permission: "write".into(),
        },
        Event::CollaboratorEdited {
            repo_id: 1,
            user_id: 2,
            actor_id: 1,
            old_permission: "write".into(),
            permission: "admin".into(),
        },
        Event::CollaboratorRemoved {
            repo_id: 1,
            user_id: 2,
            actor_id: 1,
        },
        Event::IssueOpened {
            repo_id: 1,
            issue_id: 1,
            actor_id: 1,
        },
        Event::IssuePinned {
            repo_id: 1,
            issue_id: 1,
            actor_id: 1,
        },
        Event::IssueUnpinned {
            repo_id: 1,
            issue_id: 1,
            actor_id: 1,
        },
        Event::IssueTransferred {
            repo_id: 1,
            issue_id: 1,
            old_repo_id: 2,
            old_number: 1,
            actor_id: 1,
        },
        Event::IssueLabeled {
            repo_id: 1,
            issue_id: 1,
            label_id: 1,
            actor_id: 1,
        },
        Event::SubIssueAdded {
            repo_id: 1,
            parent_id: 1,
            sub_issue_id: 2,
            actor_id: 1,
        },
        Event::SubIssueRemoved {
            repo_id: 1,
            parent_id: 1,
            sub_issue_id: 2,
            actor_id: 1,
        },
        Event::IssueCommentEdited {
            repo_id: 1,
            issue_id: 1,
            comment_id: 1,
            actor_id: 1,
            changes: v.clone(),
        },
        Event::PullRequestOpened {
            repo_id: 1,
            pull_id: 1,
            actor_id: 1,
        },
        Event::PullRequestAutoMergeEnabled {
            repo_id: 1,
            pull_id: 1,
            actor_id: 1,
        },
        Event::PullRequestAutoMergeDisabled {
            repo_id: 1,
            pull_id: 1,
            actor_id: Some(1),
        },
        Event::PullRequestReviewSubmitted {
            repo_id: 1,
            pull_id: 1,
            review_id: 1,
            actor_id: 1,
        },
        Event::PullRequestReviewEdited {
            repo_id: 1,
            pull_id: 1,
            review_id: 1,
            actor_id: 1,
            changes: v.clone(),
        },
        Event::PullRequestReviewCommentCreated {
            repo_id: 1,
            pull_id: 1,
            comment_id: 1,
            actor_id: 1,
        },
        Event::PullRequestReviewThreadResolved {
            repo_id: 1,
            pull_id: 1,
            comment_id: 1,
            actor_id: 1,
        },
        Event::PullRequestReviewThreadUnresolved {
            repo_id: 1,
            pull_id: 1,
            comment_id: 1,
            actor_id: 1,
        },
        Event::ReleaseCreated {
            repo_id: 1,
            release_id: 1,
            actor_id: 1,
        },
        Event::ReleaseEdited {
            repo_id: 1,
            release_id: 1,
            actor_id: 1,
            changes: v.clone(),
        },
        Event::ReleaseStateChanged {
            repo_id: 1,
            release_id: 1,
            actor_id: 1,
            action: "unpublished".into(),
        },
        Event::PackagePublished {
            repo_id: 1,
            package_id: 1,
            version_id: 1,
            actor_id: 1,
            tag: None,
        },
        Event::PackageUpdated {
            repo_id: 1,
            package_id: 1,
            version_id: 1,
            actor_id: 1,
            tag: None,
        },
        Event::LabelCreated {
            repo_id: 1,
            label_id: 1,
            actor_id: 1,
        },
        Event::MilestoneCreated {
            repo_id: 1,
            milestone_id: 1,
            actor_id: 1,
        },
        Event::CommitStatusCreated {
            repo_id: 1,
            status_id: 1,
            sha: sha.clone(),
            actor_id: None,
        },
        Event::CheckRunCreated {
            repo_id: 1,
            check_run_id: 1,
            actor_id: None,
        },
        Event::CheckRunCompleted {
            repo_id: 1,
            check_run_id: 1,
            actor_id: None,
        },
        Event::CheckRunRerequested {
            repo_id: 1,
            check_run_id: 1,
            actor_id: 1,
        },
        Event::CheckRunActionRequested {
            repo_id: 1,
            check_run_id: 1,
            actor_id: 1,
            identifier: "x".into(),
        },
        Event::CheckSuiteRequested {
            repo_id: 1,
            check_suite_id: 1,
            actor_id: None,
        },
        Event::CheckSuiteRerequested {
            repo_id: 1,
            check_suite_id: 1,
            actor_id: 1,
        },
        Event::CheckSuiteCompleted {
            repo_id: 1,
            check_suite_id: 1,
        },
        Event::CheckRunUpdated {
            repo_id: 1,
            check_run_id: 1,
            action: "completed".into(),
            actor_id: None,
        },
        Event::WorkflowRunUpdated {
            repo_id: 1,
            run_id: 1,
            action: "completed".into(),
            actor_id: None,
            workflow_run: v.clone(),
            workflow: None,
        },
        Event::WorkflowJobUpdated {
            repo_id: 1,
            run_id: 1,
            job_id: 1,
            action: "completed".into(),
            workflow_job: v.clone(),
        },
        Event::DeploymentCreated {
            repo_id: 1,
            deployment_id: 1,
            actor_id: Some(1),
        },
        Event::DeploymentStatusCreated {
            repo_id: 1,
            deployment_id: 1,
            status_id: 1,
            state: "success".into(),
            actor_id: Some(1),
        },
        Event::CommitCommentCreated {
            repo_id: 1,
            comment_id: 1,
            actor_id: 1,
            commit_author_id: Some(1),
        },
        Event::DeployKeyCreated {
            repo_id: 1,
            key_id: 1,
            actor_id: 1,
            key: v.clone(),
        },
        Event::DeployKeyDeleted {
            repo_id: 1,
            key_id: 1,
            actor_id: 1,
            key: v.clone(),
        },
        Event::BranchProtectionRuleChanged {
            repo_id: 1,
            actor_id: 1,
            action: "created".into(),
            rule: v.clone(),
            changes: v.clone(),
        },
        Event::RepositoryRulesetChanged {
            repo_id: 1,
            actor_id: 1,
            action: "created".into(),
            ruleset: v.clone(),
            changes: v.clone(),
        },
        Event::WikiPagesUpdated {
            repo_id: 1,
            actor_id: 1,
            pages: json!([]),
        },
        Event::RepositoryDispatch {
            repo_id: 1,
            actor_id: 1,
            event_type: "deploy".into(),
            client_payload: json!({}),
            branch: "main".into(),
        },
        Event::OrgMemberAdded {
            org_id: 1,
            user_id: 2,
            actor_id: 1,
        },
        Event::OrgMemberRemoved {
            org_id: 1,
            user_id: 2,
            actor_id: 1,
        },
        Event::OrgMemberInvited {
            org_id: 1,
            invitation_id: 1,
            actor_id: 1,
        },
        Event::TeamCreated {
            org_id: 1,
            team_id: 1,
            actor_id: 1,
        },
        Event::TeamEdited {
            org_id: 1,
            team_id: 1,
            actor_id: 1,
            changes: v.clone(),
        },
        Event::TeamDeleted {
            org_id: 1,
            team_id: 1,
            slug: "t".into(),
            actor_id: 1,
            team: v.clone(),
        },
        Event::TeamRepoAdded {
            org_id: 1,
            team_id: 1,
            repo_id: 1,
            actor_id: 1,
        },
        Event::TeamRepoRemoved {
            org_id: 1,
            team_id: 1,
            repo_id: 1,
            actor_id: 1,
        },
        Event::TeamMemberAdded {
            org_id: 1,
            team_id: 1,
            user_id: 2,
            actor_id: 1,
        },
        Event::TeamMemberRemoved {
            org_id: 1,
            team_id: 1,
            user_id: 2,
            actor_id: 1,
        },
    ]
}

/// The variant name of an event (`Event::IssueOpened {..}` → `IssueOpened`).
fn variant(e: &Event) -> String {
    let dbg = format!("{e:?}");
    dbg.split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_string()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Production source of every crate except the event definitions and
/// bgh-notify's payload mapping (which only *matches* variants).
fn emitter_sources() -> String {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    let mut all = String::new();
    for f in files {
        let path = f.to_string_lossy().replace('\\', "/");
        if path.ends_with("bgh-core/src/events.rs")
            || path.contains("bgh-notify/src/payloads/")
            || path.contains("bgh-notify/src/fanout.rs")
            || path.contains("bgh-notify/src/webhooks/dispatch.rs")
        {
            continue;
        }
        all.push_str(&std::fs::read_to_string(&f).unwrap());
        all.push('\n');
    }
    all
}

/// Whether some crate constructs `Event::<variant>` (a struct literal or a
/// tuple variant), as opposed to matching on it: patterns use `..` or are
/// followed by `=>`, `|` or `if`.
fn has_emitter(src: &str, variant: &str) -> bool {
    for open in ['{', '('] {
        let close = if open == '{' { '}' } else { ')' };
        let pat = format!("Event::{variant} {open}");
        let alt = format!("Event::{variant}{open}");
        for needle in [pat, alt] {
            let mut from = 0;
            while let Some(i) = src[from..].find(&needle) {
                let start = from + i + needle.len();
                from = start;
                // Matching close bracket.
                let mut depth = 1;
                let mut end = start;
                for (j, c) in src[start..].char_indices() {
                    if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            end = start + j;
                            break;
                        }
                    }
                }
                let body = &src[start..end];
                let after = src[end + 1..].trim_start();
                let pattern = body.contains("..")
                    || after.starts_with("=>")
                    || after.starts_with('|')
                    || after.starts_with("if ");
                if !pattern {
                    return true;
                }
            }
        }
    }
    false
}

#[test]
fn every_accepted_webhook_event_has_a_producer_or_is_listed() {
    let src = emitter_sources();
    // webhook name → emitted domain variants mapping to it.
    let mut producers: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut unmapped = Vec::new();
    for e in samples() {
        let names = event_names(&e);
        let v = variant(&e);
        if names.is_empty() {
            unmapped.push(v.clone());
        }
        if !has_emitter(&src, &v) {
            continue;
        }
        for n in names {
            producers.entry(n).or_default().insert(v.clone());
        }
    }
    assert!(
        unmapped.is_empty(),
        "samples without a webhook: {unmapped:?}"
    );

    let mut missing = Vec::new();
    for name in EVENTS.iter().copied().filter(|n| *n != "*") {
        let listed = NOT_PRODUCIBLE_YET.contains(&name) || HOOK_LIFECYCLE.contains(&name);
        let produced = producers.contains_key(name);
        assert!(
            !(listed && produced),
            "`{name}` is produced (by {:?}) but listed as not producible: remove it from the list",
            producers.get(name)
        );
        if !listed && !produced {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "webhook events accepted by the validator with no emitted domain event: {missing:?} \
         (wire an emitter or add them to NOT_PRODUCIBLE_YET)"
    );
    // Every producible name maps to something the validator accepts.
    for name in producers.keys() {
        assert!(EVENTS.contains(name), "`{name}` is not in webhooks::EVENTS");
    }
}
