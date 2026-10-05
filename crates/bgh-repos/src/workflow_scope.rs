//! GitHub's `workflow` scope: tokens may only create, update or delete
//! workflow files (`.github/workflows/**`) when they carry the `workflow`
//! scope. Checked for git pushes (in the `pre-receive` hook, see
//! [`bgh_git::smart_http::PushPolicy::workflow_denied`]) and contents API
//! writes. Actions job tokens are always refused; browser sessions,
//! passwords and SSH keys (full user credentials) are always allowed.

use bgh_core::perms::job_token_repo;
use bgh_core::prelude::*;
use bgh_git::smart_http::WORKFLOW_PATH_PLACEHOLDER;

/// Whether `path` (repository-relative) is a workflow file.
pub fn is_workflow_path(path: &str) -> bool {
    path.trim_start_matches('/')
        .starts_with(".github/workflows/")
}

/// The rejection message template (with
/// [`WORKFLOW_PATH_PLACEHOLDER`]) when `auth` may not touch workflow
/// files, `None` when it may.
pub async fn denial(state: &AppState, auth: &AuthContext) -> ApiResult<Option<String>> {
    let who = match auth.method {
        bgh_core::auth::AuthMethod::Token { token_id } => {
            if job_token_repo(auth).is_some() {
                return Ok(Some(format!(
                    "refusing to allow a GitHub App to create or update workflow \
                     `{WORKFLOW_PATH_PLACEHOLDER}` without `workflows` permission"
                )));
            }
            if auth.has_scope("workflow") {
                return Ok(None);
            }
            let kind: Option<String> =
                sqlx::query_scalar("SELECT kind FROM access_tokens WHERE id = $1")
                    .bind(token_id)
                    .fetch_optional(&state.db)
                    .await?;
            match kind.as_deref() {
                Some("oauth") => "an OAuth App",
                _ => "a Personal Access Token",
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(format!(
        "refusing to allow {who} to create or update workflow \
         `{WORKFLOW_PATH_PLACEHOLDER}` without `workflow` scope"
    )))
}

/// `template` with the offending `path` filled in.
pub fn message(template: &str, path: &str) -> String {
    template.replace(WORKFLOW_PATH_PLACEHOLDER, path)
}

/// 403 when `auth` may not write the workflow file `path` (contents API).
pub async fn check_path(state: &AppState, auth: &AuthContext, path: &str) -> ApiResult<()> {
    if !is_workflow_path(path) {
        return Ok(());
    }
    match denial(state, auth).await? {
        Some(t) => Err(ApiError::forbidden(message(
            &t,
            path.trim_start_matches('/'),
        ))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_paths() {
        assert!(is_workflow_path(".github/workflows/ci.yml"));
        assert!(is_workflow_path("/.github/workflows/sub/x.yaml"));
        assert!(!is_workflow_path(".github/dependabot.yml"));
        assert!(!is_workflow_path("docs/.github/workflows/ci.yml"));
        assert_eq!(
            message("x `@PATH@` y", ".github/workflows/a.yml"),
            "x `.github/workflows/a.yml` y"
        );
    }
}
