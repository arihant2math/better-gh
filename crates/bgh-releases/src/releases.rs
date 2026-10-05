//! Release CRUD: list, get, latest, by tag, create, update, delete.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::events::PushEvent;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::git;
use crate::model::{BodyFormat, Release, ReleaseRow, Renderer, release_sync_json};
use crate::notes;

/// Load a release of `access.repo`; drafts are only visible to writers.
pub(crate) async fn load(state: &AppState, access: &RepoAccess, id: i64) -> ApiResult<ReleaseRow> {
    let row: ReleaseRow = sqlx::query_as(&format!(
        "SELECT {} FROM releases WHERE id = $1 AND repo_id = $2",
        ReleaseRow::COLUMNS
    ))
    .bind(id)
    .bind(access.repo.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if row.draft && access.permission < Permission::Write {
        return Err(ApiError::NotFound);
    }
    Ok(row)
}

pub(crate) fn renderer<'a>(
    state: &'a AppState,
    access: &'a RepoAccess,
    headers: &HeaderMap,
) -> Renderer<'a> {
    Renderer {
        state,
        owner: &access.owner.login,
        repo: &access.repo.name,
        format: BodyFormat::from_headers(headers),
    }
}

/// `GET /repos/{owner}/{repo}/releases`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Release>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<ReleaseRow> = sqlx::query_as(&format!(
        "SELECT {} FROM releases WHERE repo_id = $1 AND (NOT draft OR $2)
          ORDER BY created_at DESC, id DESC LIMIT $3 OFFSET $4",
        ReleaseRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(access.permission >= Permission::Write)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = renderer(&state, &access, &headers)
        .render(page.items)
        .await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

/// `GET /repos/{owner}/{repo}/releases/{release_id}`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<Release>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = load(&state, &access, id).await?;
    Ok(Json(
        renderer(&state, &access, &headers).render_one(row).await?,
    ))
}

/// The latest release: explicitly marked, else the newest published
/// non-prerelease (excluding releases marked `make_latest: false`).
pub(crate) async fn latest_row(state: &AppState, repo_id: i64) -> ApiResult<Option<ReleaseRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM releases
          WHERE repo_id = $1 AND NOT draft AND NOT prerelease AND make_latest IS NOT FALSE
          ORDER BY (make_latest IS TRUE) DESC, created_at DESC, id DESC LIMIT 1",
        ReleaseRow::COLUMNS
    ))
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await?)
}

/// `GET /repos/{owner}/{repo}/releases/latest`
pub async fn latest(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Release>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row = latest_row(&state, access.repo.id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(
        renderer(&state, &access, &headers).render_one(row).await?,
    ))
}

/// `GET /repos/{owner}/{repo}/releases/tags/{tag}` (published releases only).
pub async fn by_tag(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    Path((owner, repo, tag)): Path<(String, String, String)>,
) -> ApiResult<Json<Release>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row: ReleaseRow = sqlx::query_as(&format!(
        "SELECT {} FROM releases WHERE repo_id = $1 AND tag_name = $2 AND NOT draft",
        ReleaseRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&tag)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok(Json(
        renderer(&state, &access, &headers).render_one(row).await?,
    ))
}

#[derive(Debug, Default, Deserialize)]
pub struct ReleaseBody {
    pub tag_name: Option<String>,
    pub target_commitish: Option<String>,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: Option<bool>,
    pub prerelease: Option<bool>,
    #[serde(default)]
    pub generate_release_notes: bool,
    /// `"true"` | `"false"` | `"legacy"` (booleans accepted too).
    pub make_latest: Option<serde_json::Value>,
    pub discussion_category_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MakeLatest {
    True,
    False,
    Legacy,
}

fn parse_make_latest(v: &Option<serde_json::Value>) -> ApiResult<Option<MakeLatest>> {
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Bool(true)) => Some(MakeLatest::True),
        Some(serde_json::Value::Bool(false)) => Some(MakeLatest::False),
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "true" => Some(MakeLatest::True),
            "false" => Some(MakeLatest::False),
            "legacy" => Some(MakeLatest::Legacy),
            _ => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Release",
                    "make_latest",
                )));
            }
        },
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Release",
                "make_latest",
            )));
        }
    })
}

fn tag_conflict(e: sqlx::Error) -> ApiError {
    match bgh_core::db::unique_violation(&e).as_deref() {
        Some("releases_repo_tag_published_key") => {
            ApiError::invalid_field(FieldError::already_exists("Release", "tag_name"))
        }
        _ => e.into(),
    }
}

/// Apply "latest" semantics for a (newly) published release inside `tx`:
/// `true` (the default on publish) clears other explicit marks.
async fn apply_latest(
    tx: &mut Tx,
    row: &ReleaseRow,
    choice: Option<MakeLatest>,
    publishing: bool,
) -> ApiResult<Option<bool>> {
    let value = match choice {
        Some(MakeLatest::False) => Some(false),
        Some(MakeLatest::Legacy) => None,
        Some(MakeLatest::True) => Some(true),
        None if publishing && !row.prerelease => Some(true),
        None => return Ok(row.make_latest),
    };
    if value == Some(true) && !row.draft {
        sqlx::query(
            "UPDATE releases SET make_latest = NULL
              WHERE repo_id = $1 AND id <> $2 AND make_latest IS TRUE",
        )
        .bind(row.repo_id)
        .bind(row.id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(value)
}

fn location(state: &AppState, access: &RepoAccess, id: i64) -> HeaderValue {
    HeaderValue::from_str(&state.urls.api(&format!(
        "/repos/{}/{}/releases/{id}",
        access.owner.login, access.repo.name
    )))
    .expect("valid url")
}

/// `POST /repos/{owner}/{repo}/releases`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<ReleaseBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let tag = body
        .tag_name
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Release", "tag_name")))?;
    git::validate_tag_name(&tag)?;
    let make_latest = parse_make_latest(&body.make_latest)?;
    let draft = body.draft.unwrap_or(false);
    let prerelease = body.prerelease.unwrap_or(false);
    let target = body
        .target_commitish
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());

    if !draft {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM releases WHERE repo_id = $1 AND tag_name = $2 AND NOT draft)",
        )
        .bind(access.repo.id)
        .bind(&tag)
        .fetch_one(&state.db)
        .await?;
        if exists {
            return Err(ApiError::invalid_field(FieldError::already_exists(
                "Release", "tag_name",
            )));
        }
    }

    let (mut name, mut text) = (body.name.clone(), body.body.clone());
    if body.generate_release_notes {
        let generated = notes::generate(&state, &access, &tag, Some(&target), None).await?;
        text = Some(match text.filter(|t| !t.is_empty()) {
            Some(t) => format!("{t}\n\n{}", generated.body),
            None => generated.body,
        });
        if name.as_deref().is_none_or(str::is_empty) {
            name = Some(generated.name);
        }
    }

    // Published releases need their tag; drafts create it on publish.
    let created_tag = if draft {
        None
    } else {
        git::ensure_tag(&state, &access.repo, &tag, &target).await?
    };

    let mut tx = Tx::begin(&state).await?;
    let mut row: ReleaseRow = sqlx::query_as(&format!(
        "INSERT INTO releases (repo_id, tag_name, target_commitish, name, body, draft,
                               prerelease, author_id, published_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CASE WHEN $6 THEN NULL ELSE now() END)
         RETURNING {}",
        ReleaseRow::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(&tag)
    .bind(&target)
    .bind(&name)
    .bind(&text)
    .bind(draft)
    .bind(prerelease)
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(tag_conflict)?;
    let latest = match (draft, make_latest) {
        // Drafts remember an explicit choice for publish time.
        (true, Some(MakeLatest::False)) => Some(false),
        (true, Some(MakeLatest::True)) => Some(true),
        (true, _) => None,
        (false, choice) => apply_latest(&mut tx, &row, choice, true).await?,
    };
    if latest != row.make_latest {
        row = sqlx::query_as(&format!(
            "UPDATE releases SET make_latest = $2 WHERE id = $1 RETURNING {}",
            ReleaseRow::COLUMNS
        ))
        .bind(row.id)
        .bind(latest)
        .fetch_one(&mut *tx)
        .await?;
    }
    tx.sync(
        &access.scope(),
        "release",
        row.id,
        SyncAction::Insert,
        &release_sync_json(&row),
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "release.create",
        audit::Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({ "release_id": row.id, "tag_name": row.tag_name }),
    )
    .await?;
    if let Some(update) = created_tag {
        tx.emit(Event::Push(PushEvent {
            repo_id: access.repo.id,
            pusher_id: Some(auth.user.id),
            updates: vec![update],
            origin: None,
        }));
    }
    tx.emit(Event::ReleaseCreated {
        repo_id: access.repo.id,
        release_id: row.id,
        actor_id: auth.user.id,
    });
    if !row.draft {
        tx.emit(Event::ReleasePublished {
            repo_id: access.repo.id,
            release_id: row.id,
            actor_id: auth.user.id,
        });
    }
    tx.commit().await?;

    let id = row.id;
    let json = renderer(&state, &access, &headers).render_one(row).await?;
    let mut resp = (StatusCode::CREATED, Json(json)).into_response();
    resp.headers_mut()
        .insert(header::LOCATION, location(&state, &access, id));
    Ok(resp)
}

/// `PATCH /repos/{owner}/{repo}/releases/{release_id}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<ReleaseBody>,
) -> ApiResult<Json<Release>> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let current = load(&state, &access, id).await?;
    let make_latest = parse_make_latest(&body.make_latest)?;

    let tag = match body.tag_name.as_deref() {
        Some("") => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "Release", "tag_name",
            )));
        }
        Some(t) => {
            git::validate_tag_name(t)?;
            t.to_string()
        }
        None => current.tag_name.clone(),
    };
    let target = body
        .target_commitish
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| current.target_commitish.clone());
    let draft = body.draft.unwrap_or(current.draft);
    let publishing = current.draft && !draft;

    let created_tag = if draft {
        None
    } else {
        git::ensure_tag(&state, &access.repo, &tag, &target).await?
    };

    let mut tx = Tx::begin(&state).await?;
    let mut row: ReleaseRow = sqlx::query_as(&format!(
        "UPDATE releases
            SET tag_name = $2, target_commitish = $3, name = coalesce($4, name),
                body = coalesce($5, body), draft = $6, prerelease = coalesce($7, prerelease),
                published_at = CASE WHEN $6 THEN NULL ELSE coalesce(published_at, now()) END,
                updated_at = now()
          WHERE id = $1 RETURNING {}",
        ReleaseRow::COLUMNS
    ))
    .bind(current.id)
    .bind(&tag)
    .bind(&target)
    .bind(&body.name)
    .bind(&body.body)
    .bind(draft)
    .bind(body.prerelease)
    .fetch_one(&mut *tx)
    .await
    .map_err(tag_conflict)?;
    let choice = make_latest.or(if publishing {
        match current.make_latest {
            Some(true) => Some(MakeLatest::True),
            Some(false) => Some(MakeLatest::False),
            None => None,
        }
    } else {
        None
    });
    if !row.draft && (choice.is_some() || publishing) {
        let latest = apply_latest(&mut tx, &row, choice, publishing).await?;
        if latest != row.make_latest {
            row = sqlx::query_as(&format!(
                "UPDATE releases SET make_latest = $2 WHERE id = $1 RETURNING {}",
                ReleaseRow::COLUMNS
            ))
            .bind(row.id)
            .bind(latest)
            .fetch_one(&mut *tx)
            .await?;
        }
    }
    tx.sync(
        &access.scope(),
        "release",
        row.id,
        SyncAction::Update,
        &release_sync_json(&row),
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "release.update",
        audit::Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({ "release_id": row.id, "tag_name": row.tag_name }),
    )
    .await?;
    if let Some(update) = created_tag {
        tx.emit(Event::Push(PushEvent {
            repo_id: access.repo.id,
            pusher_id: Some(auth.user.id),
            updates: vec![update],
            origin: None,
        }));
    }
    tx.emit(Event::ReleaseUpdated {
        repo_id: access.repo.id,
        release_id: row.id,
        actor_id: auth.user.id,
    });
    // Webhook-facing events: `edited` with GitHub's `changes`, plus the
    // state transitions (`published` is ReleasePublished below).
    let mut changes = serde_json::Map::new();
    if current.name != row.name {
        changes.insert("name".into(), json!({ "from": current.name }));
    }
    if current.body != row.body {
        changes.insert("body".into(), json!({ "from": current.body }));
    }
    if current.tag_name != row.tag_name {
        changes.insert("tag_name".into(), json!({ "from": current.tag_name }));
    }
    if make_latest.is_some() && current.make_latest != row.make_latest {
        changes.insert(
            "make_latest".into(),
            json!({ "to": row.make_latest == Some(true) }),
        );
    }
    if !changes.is_empty() {
        tx.emit(Event::ReleaseEdited {
            repo_id: access.repo.id,
            release_id: row.id,
            actor_id: auth.user.id,
            changes: serde_json::Value::Object(changes),
        });
    }
    let state_change = if !current.draft && row.draft {
        Some("unpublished")
    } else if !current.draft && current.prerelease != row.prerelease {
        Some(if row.prerelease {
            "prereleased"
        } else {
            "released"
        })
    } else {
        None
    };
    if let Some(action) = state_change {
        tx.emit(Event::ReleaseStateChanged {
            repo_id: access.repo.id,
            release_id: row.id,
            actor_id: auth.user.id,
            action: action.into(),
        });
    }
    if publishing {
        tx.emit(Event::ReleasePublished {
            repo_id: access.repo.id,
            release_id: row.id,
            actor_id: auth.user.id,
        });
    }
    tx.commit().await?;
    Ok(Json(
        renderer(&state, &access, &headers).render_one(row).await?,
    ))
}

/// `DELETE /repos/{owner}/{repo}/releases/{release_id}` (the tag is kept).
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    access.require(Permission::Write)?;
    access.require_not_archived()?;
    let row = load(&state, &access, id).await?;
    // GitHub REST JSON as it was before deletion (webhook payloads).
    let release = serde_json::to_value(
        renderer(&state, &access, &HeaderMap::new())
            .render_one(row.clone())
            .await?,
    )?;

    let mut tx = Tx::begin(&state).await?;
    let digests: Vec<Option<String>> =
        sqlx::query_scalar("DELETE FROM release_assets WHERE release_id = $1 RETURNING sha256")
            .bind(row.id)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM reactions WHERE subject_type = 'release' AND subject_id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM releases WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    tx.sync(
        &access.scope(),
        "release",
        row.id,
        SyncAction::Delete,
        &json!({ "id": row.id }),
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "release.destroy",
        audit::Target::Repo {
            id: access.repo.id,
            org_id: access.owner.is_org().then_some(access.owner.id),
        },
        json!({ "release_id": row.id, "tag_name": row.tag_name }),
    )
    .await?;
    tx.emit(Event::ReleaseDeleted {
        repo_id: access.repo.id,
        release_id: row.id,
        tag_name: row.tag_name.clone(),
        actor_id: auth.user.id,
        release,
    });
    tx.commit().await?;
    crate::assets::gc_blobs(&state, digests.into_iter().flatten().collect()).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_latest_values() {
        use serde_json::Value;
        assert_eq!(
            parse_make_latest(&Some(Value::String("legacy".into()))).unwrap(),
            Some(MakeLatest::Legacy)
        );
        assert_eq!(
            parse_make_latest(&Some(Value::Bool(false))).unwrap(),
            Some(MakeLatest::False)
        );
        assert_eq!(parse_make_latest(&None).unwrap(), None);
        assert!(parse_make_latest(&Some(Value::String("x".into()))).is_err());
    }
}
