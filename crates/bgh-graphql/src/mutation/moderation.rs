//! Moderation mutations (P42): `minimizeComment`, `unminimizeComment`,
//! `deleteIssue`. They call the bgh-issues moderation handlers.

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, Object, SimpleObject};
use axum::extract::State;
use bgh_core::auth::RequireUser;
use bgh_core::commit_comments::CommitCommentRow;
use bgh_core::moderation::ContentKind;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_issues::moderation::MinimizeBody;

use super::{guard, into_json, issue_by_node, owner_repo, repo_by_id};
use crate::ctx::{GResult, api_err, gql, not_found};
use crate::loaders::{ReviewCommentRow, ReviewRow};
use crate::model::Repository;
use crate::model::issue::IssueComment;
use crate::model::moderation::{CommitComment, Minimizable};
use crate::model::pull::{PullRequestReview, PullRequestReviewComment};

/// The reasons a piece of content can be reported or minimized.
#[derive(Enum, Copy, Clone, Eq, PartialEq)]
pub enum ReportedContentClassifiers {
    Spam,
    Abuse,
    OffTopic,
    Outdated,
    Duplicate,
    Resolved,
}

impl ReportedContentClassifiers {
    fn reason(self) -> &'static str {
        match self {
            Self::Spam => "spam",
            Self::Abuse => "abuse",
            Self::OffTopic => "off-topic",
            Self::Outdated => "outdated",
            Self::Duplicate => "duplicate",
            Self::Resolved => "resolved",
        }
    }
}

#[derive(InputObject)]
pub struct MinimizeCommentInput {
    pub subject_id: ID,
    pub classifier: ReportedContentClassifiers,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct MinimizeCommentPayload {
    pub client_mutation_id: Option<String>,
    pub minimized_comment: Option<Minimizable>,
}

#[derive(InputObject)]
pub struct UnminimizeCommentInput {
    pub subject_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct UnminimizeCommentPayload {
    pub client_mutation_id: Option<String>,
    pub unminimized_comment: Option<Minimizable>,
}

#[derive(InputObject)]
pub struct DeleteIssueInput {
    pub issue_id: ID,
    pub client_mutation_id: Option<String>,
}

#[derive(SimpleObject)]
pub struct DeleteIssuePayload {
    pub client_mutation_id: Option<String>,
    pub repository: Option<Repository>,
}

fn st(ctx: &Context<'_>) -> State<AppState> {
    State(gql(ctx).state.clone())
}

fn user(a: &bgh_core::auth::AuthContext) -> RequireUser {
    RequireUser(a.clone())
}

/// A minimizable subject (by node id): its kind, id and repository id.
async fn subject(ctx: &Context<'_>, id: &ID) -> GResult<(ContentKind, i64, i64)> {
    let unresolved = || {
        not_found(format!(
            "Could not resolve to a node with the global id of '{}'.",
            id.0
        ))
    };
    let (kind, n) = match node_id::decode(&id.0) {
        Some((NodeType::IssueComment, n)) => (ContentKind::Comment, n),
        Some((NodeType::PullRequestReview, n)) => (ContentKind::Review, n),
        Some((NodeType::PullRequestReviewComment, n)) => (ContentKind::ReviewComment, n),
        Some((NodeType::CommitComment, n)) => (ContentKind::CommitComment, n),
        _ => return Err(unresolved()),
    };
    let repo_id: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT repo_id FROM {} WHERE id = $1",
        kind.table()
    ))
    .bind(n)
    .fetch_optional(&gql(ctx).state.db)
    .await
    .map_err(|e| api_err(e.into()))?;
    let repo_id = repo_id.ok_or_else(unresolved)?;
    repo_by_id(ctx, repo_id).await?;
    Ok((kind, n, repo_id))
}

/// Reload a subject as its GraphQL object.
async fn minimizable(ctx: &Context<'_>, kind: ContentKind, id: i64) -> GResult<Minimizable> {
    let db = &gql(ctx).state.db;
    let map = |e: sqlx::Error| api_err(e.into());
    Ok(match kind {
        ContentKind::Comment => {
            let row: db::Comment = sqlx::query_as(&format!(
                "SELECT {} FROM comments WHERE id = $1",
                db::Comment::COLUMNS
            ))
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(map)?;
            Minimizable::IssueComment(IssueComment(Arc::new(row)))
        }
        ContentKind::Review => {
            let row: ReviewRow = sqlx::query_as(&format!(
                "SELECT {} FROM pr_reviews WHERE id = $1",
                ReviewRow::COLUMNS
            ))
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(map)?;
            Minimizable::PullRequestReview(PullRequestReview(Arc::new(row)))
        }
        ContentKind::ReviewComment => {
            let row: ReviewCommentRow = sqlx::query_as(&format!(
                "SELECT {} FROM pr_review_comments WHERE id = $1",
                ReviewCommentRow::COLUMNS
            ))
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(map)?;
            Minimizable::PullRequestReviewComment(PullRequestReviewComment(Arc::new(row)))
        }
        ContentKind::CommitComment | ContentKind::Issue => {
            let row: CommitCommentRow = sqlx::query_as(&format!(
                "SELECT {} FROM commit_comments WHERE id = $1",
                CommitCommentRow::COLUMNS
            ))
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(map)?;
            Minimizable::CommitComment(CommitComment(Arc::new(row)))
        }
    })
}

#[derive(Default)]
pub struct ModerationMutations;

#[Object]
impl ModerationMutations {
    /// Minimizes a comment on an Issue, Commit, Pull Request, or Gist.
    pub async fn minimize_comment(
        &self,
        ctx: &Context<'_>,
        input: MinimizeCommentInput,
    ) -> GResult<MinimizeCommentPayload> {
        let a = guard(ctx)?;
        let (kind, id, repo_id) = subject(ctx, &input.subject_id).await?;
        let (o, r) = owner_repo(&*repo_by_id(ctx, repo_id).await?);
        into_json(
            bgh_issues::moderation::minimize(
                st(ctx),
                user(a),
                Path((o, r, kind.as_str().to_string(), id)),
                Json(MinimizeBody {
                    reason: Some(input.classifier.reason().to_string()),
                }),
            )
            .await,
        )
        .await?;
        Ok(MinimizeCommentPayload {
            client_mutation_id: input.client_mutation_id,
            minimized_comment: Some(minimizable(ctx, kind, id).await?),
        })
    }

    /// Unminimizes a comment on an Issue, Commit, Pull Request, or Gist.
    pub async fn unminimize_comment(
        &self,
        ctx: &Context<'_>,
        input: UnminimizeCommentInput,
    ) -> GResult<UnminimizeCommentPayload> {
        let a = guard(ctx)?;
        let (kind, id, repo_id) = subject(ctx, &input.subject_id).await?;
        let (o, r) = owner_repo(&*repo_by_id(ctx, repo_id).await?);
        into_json(
            bgh_issues::moderation::unminimize(
                st(ctx),
                user(a),
                Path((o, r, kind.as_str().to_string(), id)),
            )
            .await,
        )
        .await?;
        Ok(UnminimizeCommentPayload {
            client_mutation_id: input.client_mutation_id,
            unminimized_comment: Some(minimizable(ctx, kind, id).await?),
        })
    }

    /// Deletes an Issue object.
    pub async fn delete_issue(
        &self,
        ctx: &Context<'_>,
        input: DeleteIssueInput,
    ) -> GResult<DeleteIssuePayload> {
        let a = guard(ctx)?;
        let (issue, repo) = issue_by_node(ctx, &input.issue_id).await?;
        if issue.is_pull_request {
            return Err(not_found(format!(
                "Could not resolve to an Issue with the global id of '{}'.",
                input.issue_id.0
            )));
        }
        let (o, r) = owner_repo(&repo);
        into_json(
            bgh_issues::moderation::delete_issue(st(ctx), user(a), Path((o, r, issue.number)))
                .await,
        )
        .await?;
        Ok(DeleteIssuePayload {
            client_mutation_id: input.client_mutation_id,
            repository: Some(crate::model::repo::load_unchecked(ctx, repo.repo.id).await?),
        })
    }
}
