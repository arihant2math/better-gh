//! Event matching for the `on:` section ("Events that trigger workflows").

use super::filters::filter_matches;
use super::model::{EventTrigger, Triggers};

/// Event names accepted in `on:` (lowercase).
pub const KNOWN_EVENTS: &[&str] = &[
    "branch_protection_rule",
    "check_run",
    "check_suite",
    "create",
    "delete",
    "deployment",
    "deployment_status",
    "discussion",
    "discussion_comment",
    "fork",
    "gollum",
    "image_version",
    "issue_comment",
    "issues",
    "label",
    "merge_group",
    "milestone",
    "page_build",
    "project",
    "project_card",
    "project_column",
    "public",
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
    "pull_request_target",
    "push",
    "registry_package",
    "release",
    "repository_dispatch",
    "schedule",
    "status",
    "watch",
    "workflow_call",
    "workflow_dispatch",
    "workflow_run",
];

/// Default activity types of `pull_request` / `pull_request_target`.
pub const DEFAULT_PULL_REQUEST_TYPES: &[&str] = &["opened", "synchronize", "reopened"];

fn short_branch(name: &str) -> &str {
    name.strip_prefix("refs/heads/").unwrap_or(name)
}

impl EventTrigger {
    /// Positive/negative ref filter pair. `None` when neither is set.
    fn ref_filter(
        include: &Option<Vec<String>>,
        ignore: &Option<Vec<String>>,
        name: &str,
    ) -> Option<bool> {
        match (include, ignore) {
            (None, None) => None,
            (Some(inc), _) => Some(filter_matches(inc, name)),
            (None, Some(ign)) => Some(!filter_matches(ign, name)),
        }
    }

    fn has_branch_filter(&self) -> bool {
        self.branches.is_some() || self.branches_ignore.is_some()
    }

    fn has_tag_filter(&self) -> bool {
        self.tags.is_some() || self.tags_ignore.is_some()
    }

    /// Branch filters (`branches` / `branches-ignore`) against a short
    /// branch name; true when no branch filter is set.
    pub fn branch_matches(&self, branch: &str) -> bool {
        Self::ref_filter(&self.branches, &self.branches_ignore, branch).unwrap_or(true)
    }

    /// Tag filters (`tags` / `tags-ignore`) against a short tag name; true
    /// when no tag filter is set.
    pub fn tag_matches(&self, tag: &str) -> bool {
        Self::ref_filter(&self.tags, &self.tags_ignore, tag).unwrap_or(true)
    }

    /// Path filters. `None` (unknown changes) passes. With `paths`, at least
    /// one changed file must match; with `paths-ignore`, the event triggers
    /// unless every changed file is ignored. An empty change list therefore
    /// fails any path filter.
    pub fn paths_match(&self, changed_paths: Option<&[String]>) -> bool {
        let Some(files) = changed_paths else {
            return true;
        };
        if let Some(paths) = &self.paths {
            return files.iter().any(|f| filter_matches(paths, f));
        }
        if let Some(ignore) = &self.paths_ignore {
            return files.iter().any(|f| !filter_matches(ignore, f));
        }
        true
    }

    /// Whether `action` is accepted by `types` (any action when unset).
    pub fn type_matches(&self, action: &str) -> bool {
        self.types.is_empty() || self.types.iter().any(|t| t == action)
    }
}

impl Triggers {
    /// The filters for `event` (case-insensitive), if the workflow listens to it.
    pub fn get(&self, event: &str) -> Option<&EventTrigger> {
        self.events
            .get(event)
            .or_else(|| self.events.get(&event.to_ascii_lowercase()))
    }

    /// Whether the workflow listens to `event` at all (no filters applied).
    pub fn has(&self, event: &str) -> bool {
        self.get(event).is_some()
    }

    /// `push`. `refname` is the full ref (`refs/heads/x`, `refs/tags/v1`).
    ///
    /// * only branch filters -> tag pushes don't trigger;
    /// * only tag filters -> branch pushes don't trigger;
    /// * neither -> every ref triggers;
    /// * path filters apply to branch pushes only (GitHub does not evaluate
    ///   path filters for tag pushes); `changed_paths: None` passes them.
    pub fn matches_push(&self, refname: &str, changed_paths: Option<&[String]>) -> bool {
        let Some(t) = self.get("push") else {
            return false;
        };
        let (has_branch, has_tag) = (t.has_branch_filter(), t.has_tag_filter());
        if let Some(branch) = refname.strip_prefix("refs/heads/") {
            let ref_ok = if has_branch {
                t.branch_matches(branch)
            } else {
                !has_tag
            };
            ref_ok && t.paths_match(changed_paths)
        } else if let Some(tag) = refname.strip_prefix("refs/tags/") {
            if has_tag {
                t.tag_matches(tag)
            } else {
                !has_branch
            }
        } else {
            !has_branch && !has_tag && t.paths_match(changed_paths)
        }
    }

    /// `pull_request` / `pull_request_target`. Activity types default to
    /// opened, synchronize, reopened; branch filters match the base branch's
    /// short name; path filters as for push.
    pub fn matches_pull_request(
        &self,
        event: &str,
        action: &str,
        base_branch: &str,
        changed_paths: Option<&[String]>,
    ) -> bool {
        let Some(t) = self.get(event) else {
            return false;
        };
        let type_ok = if t.types.is_empty() {
            DEFAULT_PULL_REQUEST_TYPES.contains(&action)
        } else {
            t.types.iter().any(|x| x == action)
        };
        type_ok && t.branch_matches(short_branch(base_branch)) && t.paths_match(changed_paths)
    }

    /// Generic activity-type events (`release`, `issues`, `issue_comment`,
    /// ...): when `types` is set, `action` must be listed.
    pub fn matches_activity(&self, event: &str, action: &str) -> bool {
        self.get(event).is_some_and(|t| t.type_matches(action))
    }

    /// `workflow_run`: the triggering workflow name must be listed in
    /// `workflows`, the action in `types` (default: any), and the branch must
    /// pass `branches` / `branches-ignore`.
    pub fn matches_workflow_run(
        &self,
        workflow_name: &str,
        action: &str,
        head_branch: &str,
    ) -> bool {
        self.get("workflow_run").is_some_and(|t| {
            t.workflows.iter().any(|w| w == workflow_name)
                && t.type_matches(action)
                && t.branch_matches(short_branch(head_branch))
        })
    }

    /// The `schedule` cron expressions.
    pub fn schedules(&self) -> Vec<String> {
        self.get("schedule")
            .map(|t| t.crons.clone())
            .unwrap_or_default()
    }
}
