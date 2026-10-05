//! Short-lived download tokens (`?token=`) for raw files and archives of
//! private repositories, used in redirects and `download_url`s where the
//! client cannot send credentials (browsers, `curl -L`).

use bgh_core::prelude::*;
use redis::AsyncCommands;

/// Lifetime of a download token.
pub const TTL_SECS: u64 = 300;

fn key(state: &AppState, token: &str) -> String {
    state.redis_key(&format!("dl:{}", bgh_core::crypto::sha256_hex(token)))
}

/// Issue a token granting read access to `repo_id` for [`TTL_SECS`].
pub async fn issue(state: &AppState, repo_id: i64) -> ApiResult<String> {
    let token = bgh_core::crypto::random_token(32);
    let mut redis = state.redis.clone();
    let _: () = redis
        .set_ex(key(state, &token), repo_id, TTL_SECS)
        .await
        .map_err(ApiError::internal)?;
    Ok(token)
}

/// Whether `token` grants read access to `repo_id`.
pub async fn check(state: &AppState, token: &str, repo_id: i64) -> bool {
    if token.is_empty() || token.len() > 128 {
        return false;
    }
    let mut redis = state.redis.clone();
    let v: Option<i64> = redis.get(key(state, token)).await.unwrap_or(None);
    v == Some(repo_id)
}
