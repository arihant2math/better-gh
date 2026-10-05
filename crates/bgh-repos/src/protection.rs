//! Branch protection enforcement for pushes.
//!
//! Checked before the pack reaches git (see `bgh_git::smart_http`), so only
//! rules decidable from the ref update commands are enforced here: locked
//! branches, deletions, "require a pull request", and push restrictions.
//! Force-push detection needs the pushed objects and is not yet enforced
//! (TODO: quarantine-based pre-receive check).

use bgh_core::events::RefUpdate;
use bgh_core::perms::{Permission, RepoAccess};
use bgh_core::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProtectionRule {
    pub pattern: String,
    pub required_pull_request_reviews: Option<serde_json::Value>,
    pub restrictions: Option<serde_json::Value>,
    pub enforce_admins: bool,
    pub allow_force_pushes: bool,
    pub allow_deletions: bool,
    pub lock_branch: bool,
}

/// fnmatch-style branch pattern: `*` (no `/`), `**` (anything), `?`.
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                (0..=n.len()).any(|i| go(rest, &n[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                for i in 0..=n.len() {
                    if go(rest, &n[i..]) {
                        return true;
                    }
                    if i < n.len() && n[i] == b'/' {
                        break;
                    }
                }
                false
            }
            Some(b'?') => !n.is_empty() && n[0] != b'/' && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

pub async fn load_rules(state: &AppState, repo_id: i64) -> ApiResult<Vec<ProtectionRule>> {
    Ok(sqlx::query_as(
        "SELECT pattern, required_pull_request_reviews, restrictions, enforce_admins,
                allow_force_pushes, allow_deletions, lock_branch
           FROM branch_protections WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_all(&state.db)
    .await?)
}

/// Decide whether `pusher` may apply `updates`. `Err(reason)` rejects.
pub fn check_push(
    rules: &[ProtectionRule],
    access: &RepoAccess,
    pusher: &AuthContext,
    updates: &[RefUpdate],
) -> Result<(), String> {
    for u in updates {
        let Some(branch) = u.branch() else { continue };
        for rule in rules.iter().filter(|r| pattern_matches(&r.pattern, branch)) {
            if rule.lock_branch {
                return Err(format!(
                    "protected branch hook declined: {branch} is locked"
                ));
            }
            if u.is_delete() && !rule.allow_deletions {
                return Err(format!(
                    "protected branch hook declined: cannot delete {branch}"
                ));
            }
            let admin_bypass = access.permission >= Permission::Admin && !rule.enforce_admins;
            if admin_bypass {
                continue;
            }
            if let Some(r) = &rule.restrictions {
                let allowed = r
                    .get("users")
                    .and_then(|v| v.as_array())
                    .is_some_and(|users| users.iter().any(|v| v.as_i64() == Some(pusher.user.id)));
                if !allowed {
                    return Err(format!(
                        "protected branch hook declined: you're not authorized to push to {branch}"
                    ));
                }
            }
            if rule.required_pull_request_reviews.is_some() && !u.is_delete() {
                return Err(format!(
                    "protected branch hook declined: changes to {branch} must be made through a pull request"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pattern_matches as m;

    #[test]
    fn patterns() {
        assert!(m("main", "main"));
        assert!(!m("main", "mainline"));
        assert!(m("release/*", "release/1.0"));
        assert!(!m("release/*", "release/1.0/hotfix"));
        assert!(m("release/**", "release/1.0/hotfix"));
        assert!(m("v?", "v1"));
        assert!(m("*", "feature"));
        assert!(!m("*", "feature/x"));
    }
}
