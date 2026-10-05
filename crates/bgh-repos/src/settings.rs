//! Repository settings.
//!
//! * `PATCH /repos/{owner}/{repo}`: name (rename keeps a redirect from the
//!   old name), description, homepage, visibility, default branch,
//!   archive, feature flags, merge options.
//! * `POST /repos/{owner}/{repo}/transfer`
//! * `GET`/`PUT /repos/{owner}/{repo}/topics`

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::models::api::Repository;
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use crate::create::is_valid_repo_name;
use crate::json::full_repo;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/transfer", post(transfer))
        .route(
            "/repos/{owner}/{repo}/topics",
            get(get_topics).put(put_topics),
        )
}

/// Distinguish an explicit `null` (`Some(None)`) from a missing field (`None`).
pub fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// `PATCH /repos/{owner}/{repo}` body.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateRepoBody {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub homepage: Option<Option<String>>,
    pub private: Option<bool>,
    pub visibility: Option<String>,
    pub has_issues: Option<bool>,
    pub has_projects: Option<bool>,
    pub has_wiki: Option<bool>,
    pub has_discussions: Option<bool>,
    pub is_template: Option<bool>,
    pub default_branch: Option<String>,
    pub allow_squash_merge: Option<bool>,
    pub allow_merge_commit: Option<bool>,
    pub allow_rebase_merge: Option<bool>,
    pub allow_auto_merge: Option<bool>,
    pub delete_branch_on_merge: Option<bool>,
    pub allow_update_branch: Option<bool>,
    pub use_squash_pr_title_as_default: Option<bool>,
    pub squash_merge_commit_title: Option<String>,
    pub squash_merge_commit_message: Option<String>,
    pub merge_commit_title: Option<String>,
    pub merge_commit_message: Option<String>,
    pub archived: Option<bool>,
    pub allow_forking: Option<bool>,
    pub web_commit_signoff_required: Option<bool>,
    /// Secret scanning toggles (`bgh_security::settings::apply`); other
    /// features are accepted and ignored.
    pub security_and_analysis: Option<serde_json::Value>,
}

fn invalid(field: &str) -> ApiError {
    ApiError::invalid_field(FieldError::invalid("Repository", field))
}

fn one_of(field: &str, v: &Option<String>, allowed: &[&str]) -> ApiResult<()> {
    match v {
        Some(v) if !allowed.contains(&v.as_str()) => Err(invalid(field)),
        _ => Ok(()),
    }
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Record that `owner_login/name` now refers to `repo_id`, and drop any
/// redirect shadowing the repository's current name.
pub async fn record_redirect(
    conn: &mut sqlx::PgConnection,
    repo_id: i64,
    old_owner: &str,
    old_name: &str,
    new_owner: &str,
    new_name: &str,
) -> ApiResult<()> {
    sqlx::query(
        "DELETE FROM repo_redirects
          WHERE lower(owner_login) = lower($1) AND lower(name) = lower($2)",
    )
    .bind(new_owner)
    .bind(new_name)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO repo_redirects (owner_login, name, repo_id) VALUES ($1, $2, $3)
         ON CONFLICT ((lower(owner_login)), (lower(name)))
         DO UPDATE SET repo_id = EXCLUDED.repo_id, created_at = now()",
    )
    .bind(old_owner)
    .bind(old_name)
    .bind(repo_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn name_taken(e: sqlx::Error) -> ApiError {
    match unique_violation(&e).as_deref() {
        Some("repositories_owner_name_key") => ApiError::invalid_field(FieldError::custom(
            "Repository",
            "name",
            "name already exists on this account",
        )),
        _ => e.into(),
    }
}

/// Persist every mutable settings column of `r`.
async fn save(conn: &mut sqlx::PgConnection, r: &db::Repository) -> ApiResult<db::Repository> {
    sqlx::query_as(&format!(
        "UPDATE repositories SET
            name = $2, description = $3, homepage = $4, visibility = $5, default_branch = $6,
            archived = $7, is_template = $8, allow_forking = $9, has_issues = $10,
            has_projects = $11, has_wiki = $12, has_discussions = $13,
            allow_merge_commit = $14, allow_squash_merge = $15, allow_rebase_merge = $16,
            allow_auto_merge = $17, allow_update_branch = $18, delete_branch_on_merge = $19,
            use_squash_pr_title_as_default = $20, squash_merge_commit_title = $21,
            squash_merge_commit_message = $22, merge_commit_title = $23,
            merge_commit_message = $24, web_commit_signoff_required = $25, updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(r.id)
    .bind(&r.name)
    .bind(&r.description)
    .bind(&r.homepage)
    .bind(&r.visibility)
    .bind(&r.default_branch)
    .bind(r.archived)
    .bind(r.is_template)
    .bind(r.allow_forking)
    .bind(r.has_issues)
    .bind(r.has_projects)
    .bind(r.has_wiki)
    .bind(r.has_discussions)
    .bind(r.allow_merge_commit)
    .bind(r.allow_squash_merge)
    .bind(r.allow_rebase_merge)
    .bind(r.allow_auto_merge)
    .bind(r.allow_update_branch)
    .bind(r.delete_branch_on_merge)
    .bind(r.use_squash_pr_title_as_default)
    .bind(&r.squash_merge_commit_title)
    .bind(&r.squash_merge_commit_message)
    .bind(&r.merge_commit_title)
    .bind(&r.merge_commit_message)
    .bind(r.web_commit_signoff_required)
    .fetch_one(&mut *conn)
    .await
    .map_err(name_taken)
}

/// `PATCH /repos/{owner}/{repo}`
pub async fn update_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<UpdateRepoBody>,
) -> ApiResult<Json<Repository>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    let old = access.repo.clone();
    if old.archived && body.archived != Some(false) {
        return Err(ApiError::forbidden(
            "Repository was archived so is read-only.",
        ));
    }

    let mut r = old.clone();
    if let Some(name) = &body.name {
        let name = name.trim();
        if !is_valid_repo_name(name) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Repository",
                "name",
                "name may only contain alphanumeric characters, '.', '-' and '_'",
            )));
        }
        r.name = name.to_string();
    }
    if let Some(d) = body.description.clone() {
        r.description = non_empty(d);
    }
    if let Some(h) = body.homepage.clone() {
        r.homepage = non_empty(h);
    }
    match (body.visibility.as_deref(), body.private) {
        (Some(v @ ("public" | "private")), _) => r.visibility = v.to_string(),
        (Some("internal"), _) if access.owner.is_org() => r.visibility = "internal".into(),
        (Some(_), _) => return Err(invalid("visibility")),
        (None, Some(true)) if r.visibility == "public" => r.visibility = "private".into(),
        (None, Some(false)) => r.visibility = "public".into(),
        (None, _) => {}
    }
    if r.visibility != old.visibility {
        if r.is_private() {
            auth.require_scope("repo")?;
        }
        bgh_core::settings::load(&state)
            .await?
            .privacy
            .check_visibility(&r.visibility)?;
    }
    one_of(
        "squash_merge_commit_title",
        &body.squash_merge_commit_title,
        &["PR_TITLE", "COMMIT_OR_PR_TITLE"],
    )?;
    one_of(
        "squash_merge_commit_message",
        &body.squash_merge_commit_message,
        &["PR_BODY", "COMMIT_MESSAGES", "BLANK"],
    )?;
    one_of(
        "merge_commit_title",
        &body.merge_commit_title,
        &["PR_TITLE", "MERGE_MESSAGE"],
    )?;
    one_of(
        "merge_commit_message",
        &body.merge_commit_message,
        &["PR_BODY", "PR_TITLE", "BLANK"],
    )?;
    macro_rules! set {
        ($($f:ident),*) => { $( if let Some(v) = body.$f.clone() { r.$f = v; } )* };
    }
    set!(
        has_issues,
        has_projects,
        has_wiki,
        has_discussions,
        is_template,
        allow_squash_merge,
        allow_merge_commit,
        allow_rebase_merge,
        allow_auto_merge,
        delete_branch_on_merge,
        allow_update_branch,
        use_squash_pr_title_as_default,
        squash_merge_commit_title,
        squash_merge_commit_message,
        merge_commit_title,
        merge_commit_message,
        archived,
        allow_forking,
        web_commit_signoff_required
    );

    let store = crate::store(&state);
    let mut set_head = None;
    if let Some(branch) = body.default_branch.as_deref().map(str::trim)
        && branch != old.default_branch
    {
        let exists = store
            .read(old.id, {
                let b = format!("refs/heads/{branch}");
                move |g| Ok(g.find_ref(&b)?.is_some())
            })
            .await?;
        if !exists {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Repository",
                "default_branch",
                format!(
                    "The branch {branch} was not found. Please push that ref first or create it via the Git Data API."
                ),
            )));
        }
        r.default_branch = branch.to_string();
        set_head = Some(branch.to_string());
    }

    let mut tx = Tx::begin(&state).await?;
    let updated = save(&mut tx, &r).await?;
    if let Some(sa) = &body.security_and_analysis {
        let change = bgh_security::settings::apply(&mut tx, updated.id, sa).await?;
        if change.changed() {
            audit::log(
                &mut *tx,
                Some(&auth.user),
                "repo.security_and_analysis",
                audit::Target::Repo {
                    id: updated.id,
                    org_id: access.owner.is_org().then_some(access.owner.id),
                },
                json!({"security_and_analysis": sa}),
            )
            .await?;
        }
        if change.scanning_enabled() {
            bgh_security::jobs::enqueue_history_scan(&mut tx, updated.id, "backfill").await?;
        }
    }
    let owner_login = access.owner.login.clone();
    let org_id = access.owner.is_org().then_some(access.owner.id);
    let target = audit::Target::Repo {
        id: updated.id,
        org_id,
    };
    if updated.name != old.name {
        record_redirect(
            &mut tx,
            updated.id,
            &owner_login,
            &old.name,
            &owner_login,
            &updated.name,
        )
        .await?;
        audit::log(
            &mut *tx,
            Some(&auth.user),
            "repo.rename",
            target,
            json!({"old_name": old.name, "name": updated.name}),
        )
        .await?;
        tx.emit(Event::RepositoryRenamed {
            repo_id: updated.id,
            actor_id: auth.user.id,
            old_name: old.name.clone(),
        });
    }
    if updated.visibility != old.visibility {
        audit::log(
            &mut *tx,
            Some(&auth.user),
            "repo.access",
            target,
            json!({"visibility": updated.visibility, "previous_visibility": old.visibility}),
        )
        .await?;
    }
    if updated.archived != old.archived {
        let action = if updated.archived {
            "repo.archived"
        } else {
            "repo.unarchived"
        };
        audit::log(&mut *tx, Some(&auth.user), action, target, json!({})).await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.update",
        target,
        json!({"name": updated.name}),
    )
    .await?;
    tx.sync_model(SyncModel::Repo, updated.id, SyncAction::Update)
        .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: updated.id,
        actor_id: auth.user.id,
    });
    for event in repository_webhook_events(&old, &updated, auth.user.id) {
        tx.emit(event);
    }
    if let Some(branch) = &set_head {
        // HEAD follows the default branch; done inside the transaction
        // window so a failure rolls the row back.
        bgh_git::write::set_head(&store, updated.id, branch).await?;
        crate::stats::enqueue_languages(&mut tx, updated.id).await?;
        crate::licenses::enqueue_detect(&mut tx, updated.id).await?;
    }
    tx.commit().await?;

    let access = RepoAccess {
        repo: updated,
        ..access
    };
    Ok(Json(full_repo(&state, Some(&auth), &access).await?))
}

// ----- transfer ------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct TransferBody {
    pub new_owner: Option<String>,
    pub new_name: Option<String>,
    #[serde(default)]
    pub team_ids: Vec<i64>,
}

/// `POST /repos/{owner}/{repo}/transfer`: immediate transfer to an
/// organization the caller can create repositories in, or to the caller.
/// The old `owner/name` keeps redirecting.
pub async fn transfer(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<TransferBody>,
) -> ApiResult<(StatusCode, Json<Repository>)> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Admin)?;
    bgh_core::sudo::require(&state, &auth).await?;
    let new_owner_login = body
        .new_owner
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("Repository", "new_owner"))
        })?;
    let new_owner = db::User::find_by_login(&state.db, new_owner_login)
        .await?
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::custom(
                "Repository",
                "new_owner",
                format!("{new_owner_login} does not exist"),
            ))
        })?;
    let new_name = match body.new_name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => {
            if !is_valid_repo_name(n) {
                return Err(invalid("new_name"));
            }
            n.to_string()
        }
        _ => access.repo.name.clone(),
    };
    if new_owner.id == access.owner.id && new_name == access.repo.name {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "new_owner",
            "Repository is already owned by the new owner",
        )));
    }
    // Transfers to another user wait for that user's acceptance
    // (`crate::lifecycle`); to an organization or to yourself they are
    // immediate.
    if !new_owner.is_org() && new_owner.id != auth.user.id {
        crate::lifecycle::request_transfer(&state, &auth, &access, &new_owner, &new_name).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(full_repo(&state, Some(&auth), &access).await?),
        ));
    }
    let access =
        apply_transfer(&state, &auth, &access, new_owner, new_name, &body.team_ids).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(full_repo(&state, Some(&auth), &access).await?),
    ))
}

/// Move `access`'s repository to `new_owner` as `new_name` now, on behalf
/// of `auth` (who must be able to create repositories there). The old
/// `owner/name` keeps redirecting.
pub(crate) async fn apply_transfer(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    new_owner: db::User,
    new_name: String,
    team_ids: &[i64],
) -> ApiResult<RepoAccess> {
    let visibility = crate::forks::visibility_for_owner(&access.repo.visibility, &new_owner);
    if !crate::forks::check_new_repo(state, &auth.user, &new_owner, &visibility).await? {
        return Err(ApiError::forbidden(format!(
            "You don't have the permission to create repositories on {}",
            new_owner.login
        )));
    }
    let old = access.repo.clone();
    let old_owner = access.owner.clone();

    let mut tx = Tx::begin(state).await?;
    let updated: db::Repository = sqlx::query_as(&format!(
        "UPDATE repositories SET owner_id = $2, name = $3, visibility = $4, updated_at = now()
          WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(old.id)
    .bind(new_owner.id)
    .bind(&new_name)
    .bind(&visibility)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_taken)?;
    record_redirect(
        &mut tx,
        old.id,
        &old_owner.login,
        &old.name,
        &new_owner.login,
        &new_name,
    )
    .await?;
    // The new owner no longer needs a collaborator grant.
    sqlx::query("DELETE FROM collaborators WHERE repo_id = $1 AND user_id = $2")
        .bind(old.id)
        .bind(new_owner.id)
        .execute(&mut *tx)
        .await?;
    // Team grants only make sense within the owning organization.
    sqlx::query(
        "DELETE FROM team_repos tr USING teams t
          WHERE tr.repo_id = $1 AND t.id = tr.team_id AND t.org_id <> $2",
    )
    .bind(old.id)
    .bind(new_owner.id)
    .execute(&mut *tx)
    .await?;
    if new_owner.is_org() && !team_ids.is_empty() {
        let added = sqlx::query(
            "INSERT INTO team_repos (team_id, repo_id, permission)
             SELECT t.id, $1, t.permission FROM teams t WHERE t.id = ANY($2) AND t.org_id = $3
             ON CONFLICT (team_id, repo_id) DO NOTHING",
        )
        .bind(old.id)
        .bind(team_ids)
        .bind(new_owner.id)
        .execute(&mut *tx)
        .await?;
        if added.rows_affected() as usize != team_ids.len() {
            return Err(invalid("team_ids"));
        }
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "repo.transfer",
        audit::Target::Repo {
            id: old.id,
            org_id: new_owner.is_org().then_some(new_owner.id),
        },
        json!({"from": format!("{}/{}", old_owner.login, old.name),
               "to": format!("{}/{}", new_owner.login, new_name)}),
    )
    .await?;
    tx.sync_model(SyncModel::Repo, old.id, SyncAction::Update)
        .await?;
    tx.emit(Event::RepositoryTransferred {
        repo_id: old.id,
        actor_id: auth.user.id,
        old_owner_id: old_owner.id,
    });
    tx.commit().await?;

    RepoAccess::for_repo(state, Some(auth), updated, new_owner).await
}

/// Webhook-facing events for a settings change: `archived`/`unarchived`,
/// `publicized`/`privatized`, and `edited` (with GitHub's `changes` for
/// description, homepage and default branch) when any other setting
/// changed. Renames are emitted separately.
pub fn repository_webhook_events(
    old: &db::Repository,
    new: &db::Repository,
    actor_id: i64,
) -> Vec<Event> {
    let repo_id = new.id;
    let mut out = Vec::new();
    let mut changes = serde_json::Map::new();
    if old.description != new.description {
        changes.insert("description".into(), json!({ "from": old.description }));
    }
    if old.homepage != new.homepage {
        changes.insert("homepage".into(), json!({ "from": old.homepage }));
    }
    if old.default_branch != new.default_branch {
        changes.insert(
            "default_branch".into(),
            json!({ "from": old.default_branch }),
        );
    }
    // Anything else (merge settings, features, ...) is an `edited` with
    // only the documented keys in `changes`.
    const SETTINGS: &[&str] = &[
        "is_template",
        "allow_forking",
        "has_issues",
        "has_projects",
        "has_wiki",
        "has_discussions",
        "allow_merge_commit",
        "allow_squash_merge",
        "allow_rebase_merge",
        "allow_auto_merge",
        "allow_update_branch",
        "delete_branch_on_merge",
        "use_squash_pr_title_as_default",
        "squash_merge_commit_title",
        "squash_merge_commit_message",
        "merge_commit_title",
        "merge_commit_message",
        "web_commit_signoff_required",
    ];
    let comparable = |r: &db::Repository| {
        let v = serde_json::to_value(r).unwrap_or_default();
        SETTINGS.iter().map(|k| v[*k].clone()).collect::<Vec<_>>()
    };
    if !changes.is_empty() || comparable(old) != comparable(new) {
        out.push(Event::RepositoryEdited {
            repo_id,
            actor_id,
            changes: serde_json::Value::Object(changes),
        });
    }
    if old.archived != new.archived {
        out.push(if new.archived {
            Event::RepositoryArchived { repo_id, actor_id }
        } else {
            Event::RepositoryUnarchived { repo_id, actor_id }
        });
    }
    if old.visibility != new.visibility {
        if new.visibility == "public" {
            out.push(Event::RepositoryPublicized { repo_id, actor_id });
        } else if old.visibility == "public" {
            out.push(Event::RepositoryPrivatized { repo_id, actor_id });
        }
    }
    out
}

// ----- topics --------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct Topics {
    pub names: Vec<String>,
}

/// `GET /repos/{owner}/{repo}/topics`
async fn get_topics(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Topics>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    Ok(Json(Topics {
        names: access.repo.topics,
    }))
}

/// GitHub topic rules: lowercase letters, digits and hyphens, starting with
/// a letter or digit, at most 50 characters.
fn valid_topic(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 50
        && t.as_bytes()[0].is_ascii_alphanumeric()
        && t.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `PUT /repos/{owner}/{repo}/topics` `{"names": [...]}` (replaces all).
async fn put_topics(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<Topics>,
) -> ApiResult<Json<Topics>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Maintain)?;
    access.require_not_archived()?;
    let mut names: Vec<String> = Vec::new();
    for n in &body.names {
        let t = n.trim().to_ascii_lowercase();
        if !valid_topic(&t) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Repository",
                "topics",
                format!(
                    "{n:?} is not a valid topic. Topics must start with a lowercase letter or number, consist of 50 characters or less, and can include hyphens."
                ),
            )));
        }
        if !names.contains(&t) {
            names.push(t);
        }
    }
    if names.len() > 20 {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Repository",
            "topics",
            "Repositories can have at most 20 topics.",
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let updated: db::Repository = sqlx::query_as(&format!(
        "UPDATE repositories SET topics = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        db::Repository::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&names)
    .fetch_one(&mut *tx)
    .await?;
    tx.sync_model(SyncModel::Repo, updated.id, SyncAction::Update)
        .await?;
    tx.emit(Event::RepositoryUpdated {
        repo_id: updated.id,
        actor_id: auth.user.id,
    });
    if updated.topics != access.repo.topics {
        tx.emit(Event::RepositoryEdited {
            repo_id: updated.id,
            actor_id: auth.user.id,
            changes: json!({ "topics": { "from": access.repo.topics } }),
        });
    }
    tx.commit().await?;
    Ok(Json(Topics {
        names: updated.topics,
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn topics() {
        use super::valid_topic as ok;
        assert!(ok("rust"));
        assert!(ok("web-3"));
        assert!(!ok("-x"));
        assert!(!ok("Rust"));
        assert!(!ok(&"a".repeat(51)));
    }
}
