//! Applying ```suggestion blocks of review comments (P38): one or a batch
//! of suggestions become **one commit** on the head branch.
//!
//! `POST /_bgh/repos/{o}/{r}/pulls/{n}/suggestions/apply`
//! `{comment_ids, message?, description?, expected_head_sha?}` → `201
//! {commit_sha, resolved_thread_ids}`.
//!
//! The server reads each file at the PR head (no size limit beyond git's),
//! replaces the commented lines keeping the file's line endings (CRLF
//! stays CRLF, a missing final newline stays missing), writes one commit
//! authored by the applier with a `Co-authored-by: Name <email>` trailer
//! per suggestion author, moves the branch through
//! `bgh_repos::refs::write_ref` (protection, rulesets, post-receive →
//! synchronize) and resolves the applied threads.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::prelude::*;
use bgh_git::{GitError, PathLookup, TreeEdit, TreeEntryKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::git;
use crate::jobs::Refresh;
use crate::model::{Pull, ReviewComment};
use crate::pulls::load_pull;

/// Most suggestions applied in one commit.
pub const MAX_BATCH: usize = 100;

/// One replacement: lines `start..=end` (1-based) become `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Why a batch can't be applied to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// A suggestion points past the end of the file.
    OutOfRange,
    /// Two suggestions touch the same lines.
    Overlap,
}

/// The first ```suggestion block of a comment body (`None` without one).
pub fn extract(body: &str) -> Option<String> {
    let start = body.find("```suggestion")?;
    let rest = &body[start..];
    let nl = rest.find('\n')?;
    let content = &rest[nl + 1..];
    let end = content.find("```")?;
    Some(content[..end].to_string())
}

/// Replacement lines of a suggestion block's content (each line ends with
/// `\n`, CRLF tolerated); empty content deletes the lines.
fn suggestion_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let t = text.strip_suffix('\n').unwrap_or(text);
    t.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// Apply `edits` to `content`, keeping line endings: new lines take the
/// terminator of the first replaced line, the last one that of the last
/// replaced line (so a missing final newline stays missing).
pub fn apply(content: &[u8], edits: &[Edit]) -> Result<Vec<u8>, ApplyError> {
    // Lines with their terminators.
    let mut lines: Vec<(&[u8], &[u8])> = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        match rest.iter().position(|&b| b == b'\n') {
            Some(i) => {
                let (body, eol) = if i > 0 && rest[i - 1] == b'\r' {
                    (&rest[..i - 1], &rest[i - 1..=i])
                } else {
                    (&rest[..i], &rest[i..=i])
                };
                lines.push((body, eol));
                rest = &rest[i + 1..];
            }
            None => {
                lines.push((rest, b""));
                rest = &[];
            }
        }
    }
    let default_eol: &[u8] = lines
        .iter()
        .map(|(_, e)| *e)
        .find(|e| !e.is_empty())
        .unwrap_or(b"\n");
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.start, e.end));
    for e in &sorted {
        if e.start == 0 || e.start > e.end || e.end > lines.len() {
            return Err(ApplyError::OutOfRange);
        }
    }
    if sorted.windows(2).any(|w| w[1].start <= w[0].end) {
        return Err(ApplyError::Overlap);
    }
    let mut out = Vec::with_capacity(content.len());
    let mut next = 0usize; // index into `lines`
    for e in sorted {
        for (body, eol) in &lines[next..e.start - 1] {
            out.extend_from_slice(body);
            out.extend_from_slice(eol);
        }
        let first_eol = lines[e.start - 1].1;
        let eol: &[u8] = if first_eol.is_empty() {
            default_eol
        } else {
            first_eol
        };
        let last_eol = lines[e.end - 1].1;
        let new = suggestion_lines(&e.text);
        for (i, l) in new.iter().enumerate() {
            out.extend_from_slice(l.as_bytes());
            out.extend_from_slice(if i + 1 == new.len() { last_eol } else { eol });
        }
        next = e.end;
    }
    for (body, eol) in &lines[next..] {
        out.extend_from_slice(body);
        out.extend_from_slice(eol);
    }
    Ok(out)
}

/// Commit message: headline, optional description, one trailer per
/// co-author (deduplicated, in the given order).
pub fn commit_message(headline: &str, description: &str, coauthors: &[(String, String)]) -> String {
    let mut msg = headline.trim().to_string();
    let description = description.trim();
    if !description.is_empty() {
        msg.push_str("\n\n");
        msg.push_str(description);
    }
    let mut seen = BTreeSet::new();
    let trailers: Vec<String> = coauthors
        .iter()
        .filter(|(_, email)| seen.insert(email.to_ascii_lowercase()))
        .map(|(name, email)| {
            format!(
                "Co-authored-by: {} <{}>",
                name.replace(['<', '>', '\n'], ""),
                email.replace(['<', '>', '\n'], "")
            )
        })
        .collect();
    if !trailers.is_empty() {
        msg.push_str("\n\n");
        msg.push_str(&trailers.join("\n"));
    }
    msg.push('\n');
    msg
}

#[derive(Debug, Deserialize)]
pub struct ApplyBody {
    #[serde(default)]
    pub comment_ids: Vec<i64>,
    pub message: Option<String>,
    pub description: Option<String>,
    pub expected_head_sha: Option<String>,
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::unprocessable(msg.into())
}

/// Apply the suggestions of `comment_ids` as one commit by `auth`; returns
/// the commit SHA and the resolved thread roots.
pub async fn apply_batch(
    state: &AppState,
    auth: &AuthContext,
    access: &RepoAccess,
    pull: &Pull,
    body: &ApplyBody,
) -> ApiResult<(String, Vec<i64>)> {
    access.require_not_archived()?;
    if !pull.is_open() {
        return Err(invalid("Pull request is closed"));
    }
    let ids: Vec<i64> = body
        .comment_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "PullRequest",
            "comment_ids",
        )));
    }
    if ids.len() > MAX_BATCH {
        return Err(invalid(format!(
            "At most {MAX_BATCH} suggestions can be applied at once"
        )));
    }
    // Write access to the head repository, or maintainer edits + write on
    // base (like update-branch).
    let head_repo_id = pull
        .pr
        .head_repo_id
        .ok_or_else(|| invalid("The head repository does not exist"))?;
    let head_access = if head_repo_id == access.repo.id {
        access.clone()
    } else {
        let head_repo = db::Repository::find(&state.db, head_repo_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let head_owner = db::User::find(&state.db, head_repo.owner_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        RepoAccess::for_repo(state, Some(auth), head_repo, head_owner)
            .await
            .map_err(|_| ApiError::forbidden("Resource not accessible by integration"))?
    };
    let maintainer = pull.pr.maintainer_can_modify && access.permission >= Permission::Write;
    if head_access.permission < Permission::Write && !maintainer {
        return Err(ApiError::forbidden(
            "Resource not accessible by integration",
        ));
    }
    if let Some(expected) = body.expected_head_sha.as_deref()
        && expected != pull.pr.head_sha
    {
        return Err(invalid("expected head sha didn't match current head ref."));
    }

    let comments: Vec<ReviewComment> = sqlx::query_as(&format!(
        "SELECT {} FROM pr_review_comments c
          WHERE c.pull_id = $1 AND c.id = ANY($2)
            AND NOT EXISTS (SELECT 1 FROM pr_reviews r
                             WHERE r.id = c.review_id AND r.state = 'PENDING')
          ORDER BY c.id",
        db::prefixed("c", ReviewComment::COLUMNS)
    ))
    .bind(pull.id())
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    if comments.len() != ids.len() {
        return Err(invalid("Suggestion not found (pending or deleted)"));
    }
    let mut by_path: BTreeMap<String, Vec<Edit>> = BTreeMap::new();
    let mut author_ids: Vec<i64> = Vec::new();
    for c in &comments {
        let text = extract(&c.body)
            .ok_or_else(|| invalid(format!("Comment {} has no suggestion", c.id)))?;
        let line = match (c.subject_type.as_str(), c.side.as_deref(), c.line) {
            ("line", None | Some("RIGHT"), Some(line)) if line > 0 => line as usize,
            ("line", _, None) => {
                return Err(invalid(
                    "This suggestion is outdated and can no longer be applied",
                ));
            }
            _ => return Err(invalid("Suggestions can only be applied to changed lines")),
        };
        let start = c
            .start_line
            .filter(|s| *s > 0 && (*s as usize) <= line)
            .map_or(line, |s| s as usize);
        by_path.entry(c.path.clone()).or_default().push(Edit {
            start,
            end: line,
            text,
        });
        if let Some(a) = c.user_id
            && a != auth.user.id
            && !author_ids.contains(&a)
        {
            author_ids.push(a);
        }
    }
    for path in by_path.keys() {
        bgh_repos::workflow_scope::check_path(state, auth, path).await?;
    }

    // The branch must still be at the PR head.
    let store = git::store(state);
    let tip = git::branch_tip(&store, head_repo_id, &pull.pr.head_ref)
        .await?
        .ok_or_else(|| invalid("The head branch does not exist"))?;
    if tip != pull.pr.head_sha {
        return Err(invalid(
            "The head branch was updated; reload the pull request and try again",
        ));
    }
    let head_sha = pull.pr.head_sha.clone();
    let paths: Vec<String> = by_path.keys().cloned().collect();
    let (tree, files) = store
        .read(head_repo_id, move |r| {
            let commit = r.commit(&r.resolve_commit(&head_sha)?)?;
            let mut files = Vec::new();
            for p in paths {
                match r.lookup_path(&commit.sha, &p) {
                    Ok(PathLookup::Entry(e))
                        if matches!(e.kind, TreeEntryKind::Blob | TreeEntryKind::Executable) =>
                    {
                        let blob = r.blob_with_limit(&e.sha, u64::MAX)?;
                        files.push((p, e.mode, blob.is_binary(), blob.data));
                    }
                    Ok(_) | Err(GitError::NotFound(_)) => {
                        return Err(GitError::InvalidInput(format!(
                            "{p} no longer exists on the head branch"
                        )));
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok((commit.tree, files))
        })
        .await
        .map_err(|e| match e {
            GitError::InvalidInput(m) => invalid(m),
            e => e.into(),
        })?;
    let mut edits = Vec::new();
    for (path, mode, binary, data) in files {
        if binary {
            return Err(invalid(format!("{path} is a binary file")));
        }
        let new = apply(&data, &by_path[&path]).map_err(|e| match e {
            ApplyError::OutOfRange => invalid(format!(
                "A suggestion no longer applies to {path} (the lines changed)"
            )),
            ApplyError::Overlap => invalid(format!(
                "Suggestions on {path} overlap; apply them separately"
            )),
        })?;
        edits.push(TreeEdit::Content {
            path,
            mode,
            content: new,
        });
    }

    // Message and identities.
    let n = comments.len();
    let headline = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if n == 1 {
                "Apply suggestion from code review".to_string()
            } else {
                "Apply suggestions from code review".to_string()
            }
        });
    let mut coauthors = Vec::new();
    if !author_ids.is_empty() {
        let users =
            bgh_core::views::users_by_id(state, author_ids.iter().map(|id| Some(*id))).await?;
        for id in &author_ids {
            if let Some(u) = users.get(id) {
                let ident = git::identity(state, u).await?;
                coauthors.push((ident.name, ident.email));
            }
        }
    }
    let message = commit_message(
        &headline,
        body.description.as_deref().unwrap_or(""),
        &coauthors,
    );
    let author = git::identity(state, &auth.user).await?;
    let committer = git::site_committer(state);
    let cli = store.cli(head_repo_id)?;
    let new_tree = cli.build_tree(Some(&tree), &edits).await?;
    let commit = cli
        .commit_tree(
            &new_tree,
            std::slice::from_ref(&pull.pr.head_sha),
            &message,
            &author,
            &committer,
        )
        .await?;
    bgh_repos::refs::write_ref(
        state,
        &head_access,
        &auth.user,
        &format!("refs/heads/{}", pull.pr.head_ref),
        Some(&pull.pr.head_sha),
        Some(&commit),
        false,
    )
    .await?;

    let resolved = resolve_threads(state, access, pull, auth.user.id, &comments).await?;
    Ok((commit, resolved))
}

/// Resolve the threads of the applied comments (those not yet resolved).
async fn resolve_threads(
    state: &AppState,
    access: &RepoAccess,
    pull: &Pull,
    actor_id: i64,
    comments: &[ReviewComment],
) -> ApiResult<Vec<i64>> {
    let roots: Vec<i64> = comments
        .iter()
        .map(ReviewComment::thread_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut tx = Tx::begin(state).await?;
    let resolved: Vec<i64> = sqlx::query_scalar(
        "UPDATE pr_review_comments SET resolved_at = now(), resolved_by_id = $3
          WHERE pull_id = $1 AND id = ANY($2) AND resolved_at IS NULL
          RETURNING id",
    )
    .bind(pull.id())
    .bind(&roots)
    .bind(actor_id)
    .fetch_all(&mut *tx)
    .await?;
    if !resolved.is_empty() {
        tx.sync_models(SyncModel::ReviewComment, &resolved, SyncAction::Update)
            .await?;
        tx.enqueue(&Refresh {
            pull_id: pull.id(),
            codeowners: false,
        })
        .await?;
        for id in &resolved {
            tx.emit(Event::PullRequestReviewThreadResolved {
                repo_id: access.repo.id,
                pull_id: pull.id(),
                comment_id: *id,
                actor_id,
            });
        }
    }
    tx.commit().await?;
    Ok(resolved)
}

/// `POST /_bgh/repos/{o}/{r}/pulls/{n}/suggestions/apply`.
pub async fn apply_handler(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<ApplyBody>,
) -> ApiResult<(StatusCode, axum::Json<Value>)> {
    let (access, pull) = load_pull(&state, Some(&auth), &owner, &repo, number).await?;
    let (commit, resolved) = apply_batch(&state, &auth, &access, &pull, &body).await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(json!({
            "commit_sha": commit,
            "resolved_thread_ids": resolved,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(start: usize, end: usize, text: &str) -> Edit {
        Edit {
            start,
            end,
            text: text.to_string(),
        }
    }

    #[test]
    fn extracts_first_block() {
        assert_eq!(
            extract("Try:\n```suggestion\nlet x = 1;\n```\nok").as_deref(),
            Some("let x = 1;\n")
        );
        assert_eq!(extract("```suggestion\n```").as_deref(), Some(""));
        assert_eq!(extract("no block"), None);
    }

    #[test]
    fn keeps_crlf() {
        let src = b"a\r\nb\r\nc\r\n";
        let out = apply(src, &[e(2, 2, "B1\nB2\n")]).unwrap();
        assert_eq!(out, b"a\r\nB1\r\nB2\r\nc\r\n");
    }

    #[test]
    fn keeps_missing_final_newline() {
        let out = apply(b"a\nb", &[e(2, 2, "x\ny\n")]).unwrap();
        assert_eq!(out, b"a\nx\ny");
    }

    #[test]
    fn deletes_and_multiple() {
        let out = apply(b"1\n2\n3\n4\n5\n", &[e(4, 5, "four\n"), e(1, 2, "")]).unwrap();
        assert_eq!(out, b"3\nfour\n");
        assert_eq!(
            apply(b"1\n2\n", &[e(1, 2, "x\n"), e(2, 2, "y\n")]),
            Err(ApplyError::Overlap)
        );
        assert_eq!(
            apply(b"1\n", &[e(2, 2, "x\n")]),
            Err(ApplyError::OutOfRange)
        );
    }

    #[test]
    fn blank_line_suggestion() {
        // "```suggestion\n\n```" replaces the line with an empty one.
        let out = apply(b"a\nb\nc\n", &[e(2, 2, "\n")]).unwrap();
        assert_eq!(out, b"a\n\nc\n");
    }

    #[test]
    fn trailers() {
        let msg = commit_message(
            "Apply suggestions from code review",
            "Fix typos",
            &[
                ("Ada".into(), "ada@x.io".into()),
                ("Bob".into(), "bob@x.io".into()),
                ("Ada L".into(), "ADA@x.io".into()),
            ],
        );
        assert_eq!(
            msg,
            "Apply suggestions from code review\n\nFix typos\n\nCo-authored-by: Ada <ada@x.io>\nCo-authored-by: Bob <bob@x.io>\n"
        );
    }
}
