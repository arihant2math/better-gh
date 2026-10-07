//! GitHub `commit` shape (as returned by `GET /pulls/{n}/commits`).

use std::collections::HashMap;

use bgh_core::models::api::SimpleUser;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_git::Commit;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct GitActor {
    pub name: String,
    pub email: String,
    pub date: Timestamp,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShaUrl {
    pub sha: String,
    pub url: String,
}

pub use bgh_repos::gitjson::Verification;

#[derive(Debug, Clone, Serialize)]
pub struct CommitDetail {
    pub author: GitActor,
    pub committer: GitActor,
    pub message: String,
    pub tree: ShaUrl,
    pub url: String,
    pub comment_count: i64,
    pub verification: Verification,
}

#[derive(Debug, Clone, Serialize)]
pub struct Parent {
    pub sha: String,
    pub url: String,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitJson {
    pub sha: String,
    pub node_id: String,
    pub commit: CommitDetail,
    pub url: String,
    pub html_url: String,
    pub comments_url: String,
    pub author: Option<SimpleUser>,
    pub committer: Option<SimpleUser>,
    pub parents: Vec<Parent>,
}

/// Map commit emails to users (verified emails only), one query.
pub async fn users_by_email(
    state: &AppState,
    emails: impl IntoIterator<Item = String>,
) -> ApiResult<HashMap<String, db::User>> {
    let mut emails: Vec<String> = emails.into_iter().map(|e| e.to_lowercase()).collect();
    emails.sort();
    emails.dedup();
    if emails.is_empty() {
        return Ok(HashMap::new());
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        // Not `email`: `users.email` (nullable public email) is in User::COLUMNS.
        matched_email: String,
        #[sqlx(flatten)]
        user: db::User,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT lower(e.email) AS matched_email, {} FROM user_emails e JOIN users u ON u.id = e.user_id
          WHERE lower(e.email) = ANY($1) AND e.verified",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&emails)
    .fetch_all(&state.db)
    .await?;
    let mut map: HashMap<String, db::User> = rows
        .into_iter()
        .map(|r| (r.matched_email, r.user))
        .collect();
    // noreply addresses: {id}+{login}@users.noreply.{host}
    let suffix = format!("@users.noreply.{}", state.config.hostname());
    let mut ids = Vec::new();
    for e in &emails {
        if map.contains_key(e) {
            continue;
        }
        if let Some(local) = e.strip_suffix(&suffix)
            && let Some((id, _)) = local.split_once('+')
            && let Ok(id) = id.parse::<i64>()
        {
            ids.push((e.clone(), id));
        }
    }
    if !ids.is_empty() {
        let users = bgh_core::views::users_by_id(state, ids.iter().map(|(_, i)| Some(*i))).await?;
        for (e, id) in ids {
            if let Some(u) = users.get(&id) {
                map.insert(e, u.clone());
            }
        }
    }
    Ok(map)
}

pub fn render(
    state: &AppState,
    repo_id: i64,
    owner: &str,
    repo: &str,
    c: &Commit,
    users: &HashMap<String, db::User>,
) -> CommitJson {
    let urls = &state.urls;
    let actor = |s: &bgh_git::Signature| GitActor {
        name: s.name.clone(),
        email: s.email.clone(),
        date: s.when.into(),
    };
    let user = |s: &bgh_git::Signature| {
        users
            .get(&s.email.to_lowercase())
            .map(|u| SimpleUser::new(urls, u))
    };
    CommitJson {
        sha: c.sha.clone(),
        node_id: node_id::encode_str(NodeType::Commit, &format!("{repo_id}:{}", c.sha)),
        commit: CommitDetail {
            author: actor(&c.author),
            committer: actor(&c.committer),
            message: c.message.trim_end_matches('\n').to_string(),
            tree: ShaUrl {
                sha: c.tree.clone(),
                url: urls.api(&format!("/repos/{owner}/{repo}/git/trees/{}", c.tree)),
            },
            url: urls.api(&format!("/repos/{owner}/{repo}/git/commits/{}", c.sha)),
            comment_count: 0,
            verification: Verification::for_signature(c.signature.as_deref()),
        },
        url: urls.commit(owner, repo, &c.sha),
        html_url: urls.commit_html(owner, repo, &c.sha),
        comments_url: format!("{}/comments", urls.commit(owner, repo, &c.sha)),
        author: user(&c.author),
        committer: user(&c.committer),
        parents: c
            .parents
            .iter()
            .map(|p| Parent {
                sha: p.clone(),
                url: urls.commit(owner, repo, p),
                html_url: urls.commit_html(owner, repo, p),
            })
            .collect(),
    }
}

/// Render commits with batched author lookup.
pub async fn render_many(
    state: &AppState,
    repo_id: i64,
    owner: &str,
    repo: &str,
    commits: &[Commit],
) -> ApiResult<Vec<CommitJson>> {
    let users = users_by_email(
        state,
        commits
            .iter()
            .flat_map(|c| [c.author.email.clone(), c.committer.email.clone()]),
    )
    .await?;
    let mut verifications = bgh_repos::signatures::verify_commits(state, commits).await?;
    Ok(commits
        .iter()
        .map(|c| {
            let mut json = render(state, repo_id, owner, repo, c, &users);
            if let Some(v) = verifications.remove(&c.sha) {
                json.commit.verification = v;
            }
            json
        })
        .collect())
}
