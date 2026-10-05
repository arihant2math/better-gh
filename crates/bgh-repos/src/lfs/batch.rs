//! `POST /{owner}/{repo}/info/lfs/objects/batch`
//! (https://github.com/git-lfs/git-lfs/blob/main/docs/api/batch.md)

use std::collections::{BTreeMap, HashMap};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_git::lfs::is_valid_oid;
use serde::{Deserialize, Serialize};

use super::{LfsAccess, LfsResult, TOKEN_TTL_SECS, authorize, lfs_json};

/// Maximum objects per batch request.
pub const MAX_OBJECTS: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    pub operation: String,
    #[serde(default)]
    pub transfers: Vec<String>,
    #[serde(default)]
    pub objects: Vec<ObjectSpec>,
    pub hash_algo: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjectSpec {
    pub oid: String,
    pub size: i64,
}

#[derive(Debug, Serialize)]
pub struct Action {
    pub href: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub header: BTreeMap<String, String>,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct ObjectError {
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ObjectResponse {
    pub oid: String,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<BTreeMap<&'static str, Action>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ObjectError>,
}

#[derive(Debug, Serialize)]
pub struct BatchResponse {
    pub transfer: &'static str,
    pub objects: Vec<ObjectResponse>,
    pub hash_algo: &'static str,
}

/// Base URL for object actions: `{base}/{owner}/{repo}.git/info/lfs/objects`.
pub fn objects_url(state: &AppState, access: &RepoAccess) -> String {
    state.urls.html(&format!(
        "/{}/{}.git/info/lfs/objects",
        access.owner.login, access.repo.name
    ))
}

fn action(lfs: &LfsAccess, href: String) -> Action {
    let mut header = BTreeMap::new();
    if let Some(a) = &lfs.authorization {
        header.insert("Authorization".to_string(), a.clone());
    }
    Action {
        href,
        header,
        expires_in: TOKEN_TTL_SECS,
    }
}

fn unprocessable(msg: &str) -> super::LfsError {
    ApiError::unprocessable(msg).into()
}

pub async fn batch(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> LfsResult<Response> {
    let req: BatchRequest = serde_json::from_slice(&body)
        .map_err(|_| ApiError::bad_request("Problems parsing JSON"))?;
    let upload = match req.operation.as_str() {
        "upload" => true,
        "download" => false,
        _ => return Err(unprocessable("Invalid operation")),
    };
    if req.hash_algo.as_deref().is_some_and(|h| h != "sha256") {
        return Err(
            ApiError::Status(StatusCode::CONFLICT, "Unsupported hash algorithm".into()).into(),
        );
    }
    if !req.transfers.is_empty() && !req.transfers.iter().any(|t| t == "basic") {
        return Err(unprocessable(
            "Only the basic transfer adapter is supported",
        ));
    }
    if req.objects.len() > MAX_OBJECTS {
        return Err(unprocessable("Too many objects in one batch request"));
    }
    let lfs = authorize(&state, &headers, &owner, &repo, upload).await?;
    let repo_id = lfs.access.repo.id;

    let oids: Vec<String> = req
        .objects
        .iter()
        .filter(|o| is_valid_oid(&o.oid))
        .map(|o| o.oid.clone())
        .collect();
    let present: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
        "SELECT oid, size FROM lfs_objects WHERE repo_id = $1 AND oid = ANY($2)",
    )
    .bind(repo_id)
    .bind(&oids)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let base = objects_url(&state, &lfs.access);
    let objects = req
        .objects
        .into_iter()
        .map(|o| {
            if !is_valid_oid(&o.oid) || o.size < 0 {
                return ObjectResponse {
                    oid: o.oid,
                    size: o.size,
                    authenticated: None,
                    actions: None,
                    error: Some(ObjectError {
                        code: 422,
                        message: "Invalid object".into(),
                    }),
                };
            }
            let stored = present.get(&o.oid).copied();
            let mut actions = BTreeMap::new();
            let mut error = None;
            if upload {
                if stored != Some(o.size) {
                    actions.insert("upload", action(&lfs, format!("{base}/{}", o.oid)));
                    actions.insert("verify", action(&lfs, format!("{base}/{}/verify", o.oid)));
                }
            } else if stored.is_some() {
                actions.insert("download", action(&lfs, format!("{base}/{}", o.oid)));
            } else {
                error = Some(ObjectError {
                    code: 404,
                    message: "Object does not exist".into(),
                });
            }
            ObjectResponse {
                oid: o.oid,
                size: stored.unwrap_or(o.size),
                authenticated: Some(true),
                actions: (!actions.is_empty()).then_some(actions),
                error,
            }
        })
        .collect();
    Ok(lfs_json(
        StatusCode::OK,
        &BatchResponse {
            transfer: "basic",
            objects,
            hash_algo: "sha256",
        },
    ))
}
