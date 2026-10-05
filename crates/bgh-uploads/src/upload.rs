//! `POST /_bgh/uploads?repository_id=&owner_id=&name=`: store one
//! attachment. The body is `multipart/form-data` (a `file` part, or the
//! first part with a file name) or the raw content with `?name=`.

use axum::body::Body;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use serde::Deserialize;

use crate::model::{Attachment, AttachmentRow};
use crate::policy;
use crate::storage::{self, SpoolError};

#[derive(Debug, Default, Deserialize)]
pub struct UploadParams {
    pub repository_id: Option<i64>,
    pub owner_id: Option<i64>,
    pub name: Option<String>,
}

const RESOURCE: &str = "Attachment";

fn rejected(message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom(RESOURCE, "file", message))
}

/// Who the upload belongs to: `(owner_id, repo_id)`.
async fn resolve_target(
    state: &AppState,
    auth: &AuthContext,
    params: &UploadParams,
) -> ApiResult<(i64, Option<i64>)> {
    if let Some(repo_id) = params.repository_id {
        let repo = db::Repository::find(&state.db, repo_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let owner = db::User::find(&state.db, repo.owner_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let access = RepoAccess::for_repo(state, Some(auth), repo, owner).await?;
        access.require_not_archived()?;
        return Ok((access.owner.id, Some(access.repo.id)));
    }
    match params.owner_id {
        Some(id) if id != auth.user.id => {
            // An organization the caller belongs to.
            let role = bgh_core::perms::org_role(&state.db, id, auth.user.id).await?;
            if role.is_none() && !auth.user.site_admin {
                return Err(ApiError::NotFound);
            }
            db::User::find(&state.db, id)
                .await?
                .ok_or(ApiError::NotFound)?;
            Ok((id, None))
        }
        _ => Ok((auth.user.id, None)),
    }
}

pub async fn upload(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(params): Query<UploadParams>,
    req: Request,
) -> ApiResult<Response> {
    let (owner_id, repo_id) = resolve_target(&state, &auth, &params).await?;
    let is_multipart = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("multipart/form-data"));

    let (name, spooled) = if is_multipart {
        let mut form = Multipart::from_request(req, &state)
            .await
            .map_err(|e| ApiError::unprocessable(e.body_text()))?;
        let mut found = None;
        while let Some(field) = form
            .next_field()
            .await
            .map_err(|e| ApiError::unprocessable(e.body_text()))?
        {
            let file_name = field.file_name().map(str::to_string);
            if field.name() == Some("file") || file_name.is_some() {
                let raw = file_name
                    .or_else(|| params.name.clone())
                    .unwrap_or_default();
                let (name, max) = check_name(&raw)?;
                found = Some((name, spool(&state, field, max).await?));
                break;
            }
        }
        found.ok_or_else(|| ApiError::invalid_field(FieldError::missing_field(RESOURCE, "file")))?
    } else {
        let raw = params.name.clone().unwrap_or_default();
        let (name, max) = check_name(&raw)?;
        let body = Body::new(req.into_body()).into_data_stream();
        (name, spool(&state, body, max).await?)
    };

    let (_, content_type) = policy::classify(&name).expect("checked");
    let cleanup = |path| async move {
        let _ = tokio::fs::remove_file(path).await;
    };
    if spooled.size == 0 {
        cleanup(spooled.path).await;
        return Err(rejected("File is empty."));
    }
    if !policy::content_matches(content_type, &spooled.head) {
        cleanup(spooled.path).await;
        return Err(rejected("File contents do not match its extension."));
    }
    if let Err(e) = bgh_core::settings::check_upload_quota(&state, owner_id, spooled.size).await {
        cleanup(spooled.path).await;
        return Err(e);
    }
    storage::put(&state, &spooled.sha256, &spooled.path)
        .await
        .map_err(ApiError::internal)?;

    let row: AttachmentRow = sqlx::query_as(&format!(
        "INSERT INTO attachments (uuid, uploader_id, owner_id, repo_id, name, content_type,
                                  size, sha256)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {}",
        AttachmentRow::COLUMNS
    ))
    .bind(uuid::Uuid::new_v4())
    .bind(auth.user.id)
    .bind(owner_id)
    .bind(repo_id)
    .bind(&name)
    .bind(content_type)
    .bind(spooled.size as i64)
    .bind(&spooled.sha256)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(Attachment::new(&state, &row))).into_response())
}

/// Validate the file name: `(clean name, size limit)`, or 422.
fn check_name(raw: &str) -> ApiResult<(String, u64)> {
    let name = policy::clean_name(raw);
    if name.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            RESOURCE, "name",
        )));
    }
    let Some((kind, _)) = policy::classify(&name) else {
        return Err(rejected(format!(
            "We don't support that file type ({name}). Try again with a GIF, JPEG, JPG, MOV, MP4, PNG, SVG, WEBM, a ZIP, a PDF, a text, log or code file, or an Office document."
        )));
    };
    Ok((name, kind.max_size()))
}

async fn spool<S, E>(state: &AppState, body: S, max: u64) -> ApiResult<storage::Spooled>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>>,
    E: std::fmt::Display,
{
    storage::spool(state, body, max).await.map_err(|e| match e {
        SpoolError::TooLarge => rejected(format!(
            "Yowza, that's a big file. Try again with a file smaller than {} MB.",
            max / (1024 * 1024)
        )),
        SpoolError::Io(e) => ApiError::internal(e),
    })
}
