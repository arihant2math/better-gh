//! Comment moderation and edit history (P42): the `Minimizable` interface,
//! `UserContentEdit` (+ connection), a minimal `CommitComment`, and the
//! batch loaders behind `isMinimized` / `minimizedReason` /
//! `userContentEdits` / `lastEditedAt` / `editor` on every comment kind.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dataloader::Loader;
use async_graphql::{Context, ID, Interface, Object};
use bgh_core::commit_comments::CommitCommentRow;
use bgh_core::moderation::{self, ContentEdit, ContentKind};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;

use super::issue::IssueComment;
use super::pull::{PullRequestReview, PullRequestReviewComment};
use super::repo::{self, Repository};
use super::{Actor, actor, nid};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, gql};
use crate::loaders::{Loaders, one};
use crate::scalars::{DateTime, HTML, URI, dt, odt};

type LResult<V> = Result<HashMap<(ContentKind, i64), V>, Arc<ApiError>>;

fn group(keys: &[(ContentKind, i64)]) -> HashMap<ContentKind, Vec<i64>> {
    let mut by_kind: HashMap<ContentKind, Vec<i64>> = HashMap::new();
    for (k, id) in keys {
        by_kind.entry(*k).or_default().push(*id);
    }
    by_kind
}

/// `(kind, id)` → minimized reason (only minimized targets are present).
pub struct MinimizedLoader(pub AppState);

impl Loader<(ContentKind, i64)> for MinimizedLoader {
    type Value = String;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(ContentKind, i64)]) -> LResult<String> {
        let mut out = HashMap::new();
        for (kind, ids) in group(keys) {
            let rows = moderation::minimized_reasons(&self.0.db, kind, &ids)
                .await
                .map_err(|e| Arc::new(ApiError::from(e)))?;
            out.extend(rows.into_iter().map(|(id, r)| ((kind, id), r)));
        }
        Ok(out)
    }
}

/// `(kind, id)` → edit history, oldest first.
pub struct ContentEditsLoader(pub AppState);

impl Loader<(ContentKind, i64)> for ContentEditsLoader {
    type Value = Vec<ContentEdit>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[(ContentKind, i64)]) -> LResult<Vec<ContentEdit>> {
        let mut out: HashMap<(ContentKind, i64), Vec<ContentEdit>> = HashMap::new();
        for (kind, ids) in group(keys) {
            let rows = moderation::edits_for(&self.0.db, kind, &ids)
                .await
                .map_err(|e| Arc::new(ApiError::from(e)))?;
            for e in rows {
                out.entry((kind, e.target_id)).or_default().push(e);
            }
        }
        Ok(out)
    }
}

/// `minimizedReason` of a target.
pub async fn minimized_reason(
    ctx: &Context<'_>,
    kind: ContentKind,
    id: i64,
) -> GResult<Option<String>> {
    let l = ctx.data_unchecked::<Loaders>();
    one(&l.minimized, (kind, id)).await
}

/// `viewerCanMinimize`: triage access to the repository.
pub async fn viewer_can_minimize(ctx: &Context<'_>, repo_id: i64) -> GResult<bool> {
    if gql(ctx).viewer_id().is_none() {
        return Ok(false);
    }
    let repo = repo::load_unchecked(ctx, repo_id).await?;
    Ok(repo.row().perm >= Permission::Triage && !repo.row().repo.archived)
}

/// Edit history of a target, oldest first.
pub async fn edits(ctx: &Context<'_>, kind: ContentKind, id: i64) -> GResult<Vec<ContentEdit>> {
    let l = ctx.data_unchecked::<Loaders>();
    Ok(one(&l.content_edits, (kind, id)).await?.unwrap_or_default())
}

/// `lastEditedAt` (from the history; falls back to `updated_at` changes
/// made before history was recorded).
pub async fn last_edited_at(
    ctx: &Context<'_>,
    kind: ContentKind,
    id: i64,
) -> GResult<Option<DateTime>> {
    Ok(edits(ctx, kind, id).await?.last().map(|e| dt(e.created_at)))
}

/// `editor`: who made the latest edit.
pub async fn editor(ctx: &Context<'_>, kind: ContentKind, id: i64) -> GResult<Option<Actor>> {
    match edits(ctx, kind, id).await?.last() {
        Some(e) => actor::actor(ctx, e.editor_id).await,
        None => Ok(None),
    }
}

/// `includesCreatedEdit`: the history starts from the created text.
pub async fn includes_created_edit(ctx: &Context<'_>, kind: ContentKind, id: i64) -> GResult<bool> {
    Ok(!edits(ctx, kind, id).await?.is_empty())
}

/// `userContentEdits(first, last, after, before)`, newest first.
pub async fn user_content_edits(
    ctx: &Context<'_>,
    kind: ContentKind,
    id: i64,
    args: ConnArgs,
) -> GResult<UserContentEditConnection> {
    let mut all = edits(ctx, kind, id).await?;
    all.reverse();
    let nodes = all
        .into_iter()
        .map(|e| UserContentEdit(Arc::new(e)))
        .collect();
    Ok(Page::from_vec(nodes, &args)?.into())
}

/// An edit on user content.
#[derive(Clone)]
pub struct UserContentEdit(pub Arc<ContentEdit>);

#[Object]
impl UserContentEdit {
    pub async fn id(&self) -> ID {
        nid(NodeType::UserContentEdit, self.0.id)
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.deleted_at.unwrap_or(self.0.created_at))
    }
    pub async fn edited_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn editor(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.editor_id).await
    }
    pub async fn deleted_at(&self) -> Option<DateTime> {
        odt(self.0.deleted_at)
    }
    pub async fn deleted_by(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.deleted_by_id).await
    }
    /// The text after this edit (GitHub returns the edited body here).
    pub async fn diff(&self) -> Option<String> {
        self.0.body.clone()
    }
}

connection!(
    UserContentEditConnection,
    UserContentEditEdge,
    UserContentEdit
);

/// Entities that can be minimized.
#[derive(Interface, Clone)]
#[graphql(
    field(name = "is_minimized", ty = "GResult<bool>"),
    field(name = "minimized_reason", ty = "GResult<Option<String>>"),
    field(name = "viewer_can_minimize", ty = "GResult<bool>")
)]
pub enum Minimizable {
    IssueComment(IssueComment),
    PullRequestReviewComment(PullRequestReviewComment),
    PullRequestReview(PullRequestReview),
    CommitComment(CommitComment),
}

/// A comment on a commit (minimal: moderation and edit history).
#[derive(Clone)]
pub struct CommitComment(pub Arc<CommitCommentRow>);

#[Object]
impl CommitComment {
    pub async fn id(&self) -> ID {
        nid(NodeType::CommitComment, self.0.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.0.id)
    }
    pub async fn author(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.0.user_id).await
    }
    pub async fn body(&self) -> String {
        self.0.body.clone()
    }
    #[graphql(name = "bodyHTML")]
    pub async fn body_html(&self, ctx: &Context<'_>) -> GResult<HTML> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        Ok(super::issue::render_html(ctx, &repo, &self.0.body))
    }
    pub async fn path(&self) -> Option<String> {
        self.0.path.clone()
    }
    pub async fn position(&self) -> Option<i32> {
        self.0.position
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.0.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.0.updated_at)
    }
    pub async fn last_edited_at(&self, ctx: &Context<'_>) -> GResult<Option<DateTime>> {
        last_edited_at(ctx, ContentKind::CommitComment, self.0.id).await
    }
    pub async fn editor(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        editor(ctx, ContentKind::CommitComment, self.0.id).await
    }
    pub async fn url(&self, ctx: &Context<'_>) -> GResult<URI> {
        let repo = repo::load_unchecked(ctx, self.0.repo_id).await?;
        let r = repo.row();
        Ok(URI(gql(ctx).state.urls.html(&format!(
            "/{}/{}/commit/{}#commitcomment-{}",
            r.owner.login, r.repo.name, self.0.commit_id, self.0.id
        ))))
    }
    pub async fn repository(&self, ctx: &Context<'_>) -> GResult<Repository> {
        repo::load_unchecked(ctx, self.0.repo_id).await
    }
    pub async fn is_minimized(&self, ctx: &Context<'_>) -> GResult<bool> {
        Ok(minimized_reason(ctx, ContentKind::CommitComment, self.0.id)
            .await?
            .is_some())
    }
    pub async fn minimized_reason(&self, ctx: &Context<'_>) -> GResult<Option<String>> {
        minimized_reason(ctx, ContentKind::CommitComment, self.0.id).await
    }
    pub async fn viewer_can_minimize(&self, ctx: &Context<'_>) -> GResult<bool> {
        viewer_can_minimize(ctx, self.0.repo_id).await
    }
    pub async fn user_content_edits(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
    ) -> GResult<UserContentEditConnection> {
        user_content_edits(
            ctx,
            ContentKind::CommitComment,
            self.0.id,
            ConnArgs::new(first, last, after, before),
        )
        .await
    }
}
