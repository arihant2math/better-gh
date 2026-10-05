//! Pull request reviews: create (pending or submitted, with comments),
//! list / get, update body, delete pending, submit, dismiss, and the
//! comments of a review.

use axum::extract::State;
use bgh_core::models::api::{AuthorAssociation, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::comments::{self, LocationInput, ReviewCommentJson};
use crate::json::{Href, associations};
use crate::model::{PENDING, Pull, Review, ReviewComment};
use crate::pulls::load_pull;
use crate::timeline;

#[derive(Debug, Clone, Serialize)]
pub struct ReviewLinks {
    pub html: Href,
    pub pull_request: Href,
}

/// `pull-request-review`.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewJson {
    pub id: i64,
    pub node_id: String,
    pub user: SimpleUser,
    pub body: String,
    pub state: String,
    pub html_url: String,
    pub pull_request_url: String,
    pub author_association: AuthorAssociation,
    #[serde(rename = "_links")]
    pub links: ReviewLinks,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<Timestamp>,
    pub commit_id: Option<String>,
}

pub async fn render(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    rows: &[Review],
) -> ApiResult<Vec<ReviewJson>> {
    let users = bgh_core::views::users_by_id(state, rows.iter().map(|r| r.user_id)).await?;
    let authors: Vec<i64> = rows.iter().filter_map(|r| r.user_id).collect();
    let assoc = associations(state, &access.repo, &authors).await?;
    let owner = access.owner.login.as_str();
    let name = access.repo.name.as_str();
    let pr_url = state.urls.pull(owner, name, pull.number());
    let pr_html = state.urls.pull_html(owner, name, pull.number());
    Ok(rows
        .iter()
        .map(|r| {
            let html = format!("{pr_html}#pullrequestreview-{}", r.id);
            ReviewJson {
                id: r.id,
                node_id: node_id::encode(NodeType::PullRequestReview, r.id),
                user: SimpleUser::or_ghost(&state.urls, r.user_id.and_then(|u| users.get(&u))),
                body: r.body.clone(),
                state: r.state.clone(),
                links: ReviewLinks {
                    html: Href { href: html.clone() },
                    pull_request: Href {
                        href: pr_url.clone(),
                    },
                },
                html_url: html,
                pull_request_url: pr_url.clone(),
                author_association: r
                    .user_id
                    .and_then(|u| assoc.get(&u).copied())
                    .unwrap_or(AuthorAssociation::None),
                submitted_at: ts(r.submitted_at),
                commit_id: r.commit_id.clone(),
            }
        })
        .collect())
}
async fn find(state: &AppState, pull: &Pull, id: i64, viewer: Option<i64>) -> ApiResult<Review> {
    let r: Review = sqlx::query_as(&format!(
        "SELECT {} FROM pr_reviews WHERE id = $1 AND pull_id = $2",
        Review::COLUMNS
    ))
    .bind(id)
    .bind(pull.id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if r.state == PENDING && r.user_id != viewer {
        return Err(ApiError::NotFound);
    }
    Ok(r)
}

fn parse_event(e: &str) -> ApiResult<&'static str> {
    match e {
        "APPROVE" => Ok("APPROVED"),
        "REQUEST_CHANGES" => Ok("CHANGES_REQUESTED"),
        "COMMENT" => Ok("COMMENTED"),
        "PENDING" => Ok(PENDING),
        _ => Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequestReview",
            "event",
        ))),
    }
}

/// Validate a submission (own PR, body requirements).
fn check_submission(
    pull: &Pull,
    user_id: i64,
    state: &str,
    body: &str,
    has_comments: bool,
) -> ApiResult<()> {
    let own = pull.issue.author_id == Some(user_id);
    if own && state == "APPROVED" {
        return Err(ApiError::unprocessable(
            "Can not approve your own pull request",
        ));
    }
    if own && state == "CHANGES_REQUESTED" {
        return Err(ApiError::unprocessable(
            "Can not request changes on your own pull request",
        ));
    }
    if (state == "CHANGES_REQUESTED" || state == "COMMENTED")
        && body.trim().is_empty()
        && !has_comments
    {
        return Err(ApiError::unprocessable(format!(
            "Body is required when event is {}",
            if state == "COMMENTED" {
                "COMMENT"
            } else {
                "REQUEST_CHANGES"
            }
        )));
    }
    if state != PENDING && state != "COMMENTED" && !pull.is_open() {
        return Err(ApiError::unprocessable("Pull request is closed"));
    }
    Ok(())
}

/// Side effects of a review becoming visible: comment counts, the
/// reviewer's request removal, sync, events, mergeability refresh.
async fn on_submitted(
    tx: &mut Tx,
    access: &RepoAccess,
    pull: &Pull,
    review: &Review,
    user_id: i64,
) -> ApiResult<()> {
    let comments: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments WHERE review_id = $1 ORDER BY id",
        ReviewComment::COLUMNS
    ))
    .bind(review.id)
    .fetch_all(&mut **tx)
    .await?;
    if !comments.is_empty() {
        sqlx::query(
            "UPDATE pull_requests SET review_comments_count = review_comments_count + $2
              WHERE issue_id = $1",
        )
        .bind(pull.id())
        .bind(comments.len() as i64)
        .execute(&mut **tx)
        .await?;
    }
    for c in &comments {
        tx.sync_model(SyncModel::ReviewComment, c.id, SyncAction::Insert)
            .await?;
        tx.emit(Event::PullRequestReviewCommentCreated {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            comment_id: c.id,
            actor_id: user_id,
        });
    }
    sqlx::query("DELETE FROM pr_requested_reviewers WHERE pull_id = $1 AND user_id = $2")
        .bind(pull.id())
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE issues SET updated_at = now() WHERE id = $1")
        .bind(pull.id())
        .execute(&mut **tx)
        .await?;
    tx.sync_model(SyncModel::Review, review.id, SyncAction::Update)
        .await?;
    crate::json::sync_pull(tx, &access.scope(), pull.id()).await?;
    tx.enqueue(&crate::jobs::Refresh {
        pull_id: pull.id(),
        codeowners: false,
    })
    .await?;
    tx.emit(Event::PullRequestReviewSubmitted {
        repo_id: access.repo.id,
        pull_id: pull.id(),
        review_id: review.id,
        actor_id: user_id,
    });
    Ok(())
}

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Page<ReviewJson>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let rows: Vec<Review> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_reviews
          WHERE pull_id = $1 AND (state <> 'PENDING' OR user_id IS NOT DISTINCT FROM $2)
          ORDER BY id LIMIT $3 OFFSET $4",
        Review::COLUMNS
    ))
    .bind(pull.id())
    .bind(auth.user_id())
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = render(&state, &access, &pull, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<axum::Json<ReviewJson>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    let r = find(&state, &pull, id, auth.user_id()).await?;
    Ok(axum::Json(
        render(&state, &access, &pull, &[r]).await?.remove(0),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DraftComment {
    pub body: Option<String>,
    #[serde(flatten)]
    pub location: LocationInput,
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub commit_id: Option<String>,
    pub body: Option<String>,
    pub event: Option<String>,
    #[serde(default)]
    pub comments: Vec<DraftComment>,
}

pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<CreateBody>,
) -> ApiResult<axum::Json<ReviewJson>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let st = match body.event.as_deref() {
        None | Some("") => PENDING,
        Some(e) => parse_event(e)?,
    };
    let text = body.body.clone().unwrap_or_default();
    check_submission(&pull, auth.user.id, st, &text, !body.comments.is_empty())?;
    let commit_id = body
        .commit_id
        .clone()
        .unwrap_or_else(|| pull.pr.head_sha.clone());
    if !bgh_git::is_sha(&commit_id) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequestReview",
            "commit_id",
        )));
    }
    // Resolve every comment location before writing anything.
    let mut locations = Vec::with_capacity(body.comments.len());
    for c in &body.comments {
        if c.body.as_deref().is_none_or(|b| b.trim().is_empty()) {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "PullRequestReviewComment",
                "body",
            )));
        }
        locations.push(comments::locate(&state, &pull, &commit_id, &c.location).await?);
    }
    let mut tx = Tx::begin(&state).await?;
    let review: Review = sqlx::query_as(&format!(
        "INSERT INTO pr_reviews (pull_id, repo_id, user_id, body, state, commit_id, submitted_at)
         VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $5 = 'PENDING' THEN NULL ELSE now() END)
         RETURNING {}",
        Review::COLUMNS
    ))
    .bind(pull.id())
    .bind(access.repo.id)
    .bind(auth.user.id)
    .bind(&text)
    .bind(st)
    .bind(&commit_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some("pr_reviews_one_pending_key") => {
            ApiError::unprocessable("User can only have one pending review per pull request")
        }
        _ => e.into(),
    })?;
    for (c, loc) in body.comments.iter().zip(&locations) {
        comments::insert(
            &mut tx,
            &pull,
            review.id,
            auth.user.id,
            c.body.as_deref().unwrap_or(""),
            loc,
            None,
            false,
        )
        .await?;
    }
    if st == PENDING {
        // Pending reviews are private: no sync broadcast to the repo scope.
    } else {
        tx.sync_model(SyncModel::Review, review.id, SyncAction::Insert)
            .await?;
        on_submitted(&mut tx, &access, &pull, &review, auth.user.id).await?;
    }
    tx.commit().await?;
    Ok(axum::Json(
        render(&state, &access, &pull, &[review]).await?.remove(0),
    ))
}

#[derive(Debug, Deserialize)]
pub struct UpdateBody {
    pub body: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<axum::Json<ReviewJson>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let r = find(&state, &pull, id, Some(auth.user.id)).await?;
    if r.user_id != Some(auth.user.id) {
        return Err(ApiError::forbidden(
            "Resource not accessible by integration",
        ));
    }
    let text = body.body.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("PullRequestReview", "body"))
    })?;
    let mut tx = Tx::begin(&state).await?;
    let review: Review = sqlx::query_as(&format!(
        "UPDATE pr_reviews SET body = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        Review::COLUMNS
    ))
    .bind(id)
    .bind(&text)
    .fetch_one(&mut *tx)
    .await?;
    if review.state != PENDING {
        tx.sync_model(SyncModel::Review, id, SyncAction::Update)
            .await?;
        tx.emit(Event::PullRequestReviewEdited {
            repo_id: access.repo.id,
            pull_id: pull.id(),
            review_id: id,
            actor_id: auth.user.id,
        });
    }
    tx.commit().await?;
    Ok(axum::Json(
        render(&state, &access, &pull, &[review]).await?.remove(0),
    ))
}

pub async fn delete_pending(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<axum::Json<ReviewJson>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let r = find(&state, &pull, id, Some(auth.user.id)).await?;
    if r.state != PENDING {
        return Err(ApiError::unprocessable(
            "Can not delete a non-pending pull request review",
        ));
    }
    if r.user_id != Some(auth.user.id) {
        return Err(ApiError::NotFound);
    }
    sqlx::query("DELETE FROM pr_reviews WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(axum::Json(
        render(&state, &access, &pull, &[r]).await?.remove(0),
    ))
}

#[derive(Debug, Deserialize)]
pub struct SubmitBody {
    pub body: Option<String>,
    pub event: Option<String>,
}

pub async fn submit(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
    Json(body): Json<SubmitBody>,
) -> ApiResult<axum::Json<ReviewJson>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let r = find(&state, &pull, id, Some(auth.user.id)).await?;
    if r.user_id != Some(auth.user.id) {
        return Err(ApiError::NotFound);
    }
    if r.state != PENDING {
        return Err(ApiError::unprocessable(
            "Can not submit a non-pending pull request review",
        ));
    }
    let st = parse_event(body.event.as_deref().ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("PullRequestReview", "event"))
    })?)?;
    if st == PENDING {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequestReview",
            "event",
        )));
    }
    let text = body.body.clone().unwrap_or_else(|| r.body.clone());
    let n_comments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pr_review_comments WHERE review_id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    check_submission(&pull, auth.user.id, st, &text, n_comments > 0)?;
    let mut tx = Tx::begin(&state).await?;
    let review: Review = sqlx::query_as(&format!(
        "UPDATE pr_reviews SET body = $2, state = $3, submitted_at = now(), updated_at = now()
          WHERE id = $1 AND state = 'PENDING' RETURNING {}",
        Review::COLUMNS
    ))
    .bind(id)
    .bind(&text)
    .bind(st)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::unprocessable("Can not submit a non-pending pull request review"))?;
    on_submitted(&mut tx, &access, &pull, &review, auth.user.id).await?;
    tx.commit().await?;
    Ok(axum::Json(
        render(&state, &access, &pull, &[review]).await?.remove(0),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DismissBody {
    pub message: Option<String>,
    pub event: Option<String>,
}

pub async fn dismiss(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
    Json(body): Json<DismissBody>,
) -> ApiResult<axum::Json<ReviewJson>> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    access.require(Permission::Write)?;
    let rules = crate::protection::rules_for(&state.db, access.repo.id, &pull.pr.base_ref).await?;
    if rules.dismissal_restrictions.is_some() {
        let actor = bgh_repos::protection::Actor::load(&state, &access, &auth.user).await?;
        if !rules.may_dismiss(&actor) {
            return Err(ApiError::forbidden(
                "You are not allowed to dismiss reviews on this branch.",
            ));
        }
    }
    let message = body
        .message
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("PullRequestReview", "message"))
        })?;
    if let Some(e) = &body.event
        && e != "DISMISS"
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "PullRequestReview",
            "event",
        )));
    }
    let r = find(&state, &pull, id, Some(auth.user.id)).await?;
    match r.state.as_str() {
        "APPROVED" | "CHANGES_REQUESTED" => {}
        "COMMENTED" => {
            return Err(ApiError::unprocessable(
                "Can not dismiss a commented pull request review",
            ));
        }
        PENDING => {
            return Err(ApiError::unprocessable(
                "Can not dismiss a pending pull request review",
            ));
        }
        _ => {
            return Err(ApiError::unprocessable(
                "Can not dismiss a dismissed pull request review",
            ));
        }
    }
    let mut tx = Tx::begin(&state).await?;
    let review: Review = sqlx::query_as(&format!(
        "UPDATE pr_reviews SET state = 'DISMISSED', dismissed_at = now(), dismissal_message = $2,
                updated_at = now() WHERE id = $1 RETURNING {}",
        Review::COLUMNS
    ))
    .bind(id)
    .bind(&message)
    .fetch_one(&mut *tx)
    .await?;
    timeline::record(
        &mut tx,
        access.repo.id,
        pull.id(),
        Some(auth.user.id),
        "review_dismissed",
        None,
        json!({"dismissed_review": {"review_id": id, "state": r.state.to_lowercase(),
                "dismissal_message": message}}),
    )
    .await?;
    tx.sync_model(SyncModel::Review, id, SyncAction::Update)
        .await?;
    tx.enqueue(&crate::jobs::Refresh {
        pull_id: pull.id(),
        codeowners: false,
    })
    .await?;
    tx.emit(Event::PullRequestReviewDismissed {
        repo_id: access.repo.id,
        pull_id: pull.id(),
        review_id: id,
        actor_id: Some(auth.user.id),
    });
    tx.commit().await?;
    Ok(axum::Json(
        render(&state, &access, &pull, &[review]).await?.remove(0),
    ))
}

pub async fn comments(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo, number, id)): Path<(String, String, i64, i64)>,
) -> ApiResult<Page<ReviewCommentJson>> {
    let (access, pull) = load_pull(&state, auth.as_ref(), &owner, &repo, number).await?;
    find(&state, &pull, id, auth.user_id()).await?;
    let rows: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments WHERE review_id = $1 ORDER BY id LIMIT $2 OFFSET $3",
        ReviewComment::COLUMNS
    ))
    .bind(id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let items = comments::render(&state, &access, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}
