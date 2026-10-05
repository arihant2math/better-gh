//! Reactions on releases (`/repos/{o}/{r}/releases/{id}/reactions`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bgh_core::models::api::{REACTION_CONTENTS, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::model::Reaction;
use crate::releases;

#[derive(sqlx::FromRow)]
struct ReactionRow {
    id: i64,
    user_id: i64,
    content: String,
    created_at: DateTime<Utc>,
}

async fn render(state: &AppState, rows: Vec<ReactionRow>) -> ApiResult<Vec<Reaction>> {
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|r| Some(r.user_id))).await?;
    Ok(rows
        .into_iter()
        .map(|r| Reaction {
            id: r.id,
            node_id: node_id::encode(NodeType::Reaction, r.id),
            user: users
                .get(&r.user_id)
                .map(|u| SimpleUser::new(&state.urls, u)),
            content: r.content,
            created_at: r.created_at.into(),
        })
        .collect())
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub content: Option<String>,
}

fn validate_content(c: &str) -> ApiResult<()> {
    if REACTION_CONTENTS.contains(&c) {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "Reaction", "content",
        )))
    }
}

/// `GET /repos/{owner}/{repo}/releases/{release_id}/reactions`
pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Query(params): Query<ListParams>,
) -> ApiResult<Page<Reaction>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let release = releases::load(&state, &access, id).await?;
    if let Some(c) = params.content.as_deref() {
        validate_content(c)?;
    }
    let rows: Vec<ReactionRow> = sqlx::query_as(
        "SELECT id, user_id, content, created_at FROM reactions
          WHERE subject_type = 'release' AND subject_id = $1 AND ($2::text IS NULL OR content = $2)
          ORDER BY id LIMIT $3 OFFSET $4",
    )
    .bind(release.id)
    .bind(&params.content)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct CreateBody {
    pub content: Option<String>,
}

/// `POST /repos/{owner}/{repo}/releases/{release_id}/reactions`: 201 when
/// created, 200 with the existing reaction otherwise. Releases accept
/// `+1`, `laugh`, `heart`, `hooray`, `rocket` and `eyes`.
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let release = releases::load(&state, &access, id).await?;
    let content = body
        .content
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Reaction", "content")))?;
    validate_content(&content)?;
    if matches!(content.as_str(), "-1" | "confused") {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Reaction", "content",
        )));
    }
    let inserted: Option<ReactionRow> = sqlx::query_as(
        "INSERT INTO reactions (subject_type, subject_id, user_id, content)
         VALUES ('release', $1, $2, $3)
         ON CONFLICT (subject_type, subject_id, user_id, content) DO NOTHING
         RETURNING id, user_id, content, created_at",
    )
    .bind(release.id)
    .bind(auth.user.id)
    .bind(&content)
    .fetch_optional(&state.db)
    .await?;
    let (status, row) = match inserted {
        Some(row) => (StatusCode::CREATED, row),
        None => (
            StatusCode::OK,
            sqlx::query_as(
                "SELECT id, user_id, content, created_at FROM reactions
                  WHERE subject_type = 'release' AND subject_id = $1 AND user_id = $2 AND content = $3",
            )
            .bind(release.id)
            .bind(auth.user.id)
            .bind(&content)
            .fetch_one(&state.db)
            .await?,
        ),
    };
    let json = render(&state, vec![row])
        .await?
        .pop()
        .expect("one reaction");
    Ok((status, Json(json)).into_response())
}

/// `DELETE /repos/{owner}/{repo}/releases/{release_id}/reactions/{reaction_id}`
/// (own reactions; repository admins may delete any).
pub async fn delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id, reaction_id)): Path<(String, String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    let release = releases::load(&state, &access, id).await?;
    let deleted = sqlx::query(
        "DELETE FROM reactions
          WHERE id = $1 AND subject_type = 'release' AND subject_id = $2
            AND (user_id = $3 OR $4)",
    )
    .bind(reaction_id)
    .bind(release.id)
    .bind(auth.user.id)
    .bind(access.permission >= Permission::Admin)
    .execute(&state.db)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
