//! `commit_comment` objects (shape shared with the REST API in
//! `bgh_core::commit_comments`).

use bgh_core::AppState;
use bgh_core::commit_comments::{self as cc, BodyFormat, CommitCommentRow};
use serde_json::Value;

use super::common::RepoCtx;

/// Webhook `comment` object by id (`None` if deleted).
pub async fn commit_comment(
    state: &AppState,
    ctx: &RepoCtx,
    comment_id: i64,
) -> anyhow::Result<Option<Value>> {
    let Some(c) = CommitCommentRow::find(&state.db, ctx.repo.id, comment_id).await? else {
        return Ok(None);
    };
    let mut out = cc::render(
        state,
        ctx.owner_login(),
        &ctx.repo,
        std::slice::from_ref(&c),
        BodyFormat::RAW,
    )
    .await
    .map_err(|e| anyhow::anyhow!("rendering commit comment {comment_id}: {e:?}"))?;
    Ok(out.pop())
}
