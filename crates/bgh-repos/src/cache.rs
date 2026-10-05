//! Best-effort Redis cache for git-derived data.
//!
//! Keys must identify immutable inputs (object SHAs, resolved commit SHAs
//! plus parameters), so entries never need invalidation; they expire after
//! [`TTL_SECS`] to bound memory. Redis failures degrade to recomputation.

use std::future::Future;

use bgh_core::prelude::*;
use redis::AsyncCommands;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Lifetime of cached entries.
pub const TTL_SECS: u64 = 7 * 24 * 3600;
/// Values larger than this are not cached.
const MAX_BYTES: usize = 8 * 1024 * 1024;

fn key(state: &AppState, key: &str) -> String {
    state.redis_key(&format!("gitcache:{key}"))
}

pub async fn get<T: DeserializeOwned>(state: &AppState, k: &str) -> Option<T> {
    let mut redis = state.redis.clone();
    let raw: Option<Vec<u8>> = match redis.get(key(state, k)).await {
        Ok(v) => v,
        Err(err) => {
            tracing::debug!(?err, "git cache read failed");
            return None;
        }
    };
    serde_json::from_slice(&raw?).ok()
}

pub async fn put<T: Serialize>(state: &AppState, k: &str, value: &T) {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    if bytes.len() > MAX_BYTES {
        return;
    }
    let mut redis = state.redis.clone();
    let res: Result<(), _> = redis.set_ex(key(state, k), bytes, TTL_SECS).await;
    if let Err(err) = res {
        tracing::debug!(?err, "git cache write failed");
    }
}

/// Return the cached value for `k`, or compute, store and return it.
pub async fn cached<T, F, Fut>(state: &AppState, k: &str, compute: F) -> ApiResult<T>
where
    T: Serialize + DeserializeOwned,
    F: FnOnce() -> Fut,
    Fut: Future<Output = ApiResult<T>>,
{
    if let Some(v) = get(state, k).await {
        return Ok(v);
    }
    let v = compute().await?;
    put(state, k, &v).await;
    Ok(v)
}
