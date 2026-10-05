//! Derived repository data: languages (background job, cached per default
//! branch commit), contributors, tags and teams.
//!
//! * `GET /repos/{o}/{r}/languages`
//! * `GET /repos/{o}/{r}/contributors`
//! * `GET /repos/{o}/{r}/tags`
//! * `GET /repos/{o}/{r}/teams`

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::jobs::JobPayload;
use bgh_core::models::api::{SimpleUser, Team, TeamSimple};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::cache;
use crate::gitjson::{RepoRef, ShaUrl, short_commit};
use crate::identity::users_by_email;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/languages", get(languages))
        .route("/repos/{owner}/{repo}/contributors", get(contributors))
        .route("/repos/{owner}/{repo}/tags", get(tags))
        .route("/repos/{owner}/{repo}/teams", get(teams))
}

// ----- languages -------------------------------------------------------------

/// Recompute `repo_languages` for the default branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeLanguages {
    pub repo_id: i64,
}

impl JobPayload for ComputeLanguages {
    const KIND: &'static str = "repos.compute_languages";
    const MAX_ATTEMPTS: i32 = 3;
}

/// `{"Rust": 1234, ...}` in descending byte order.
struct OrderedLanguages(Vec<(String, u64)>);

impl Serialize for OrderedLanguages {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

#[derive(sqlx::FromRow)]
struct LanguagesRow {
    commit_sha: String,
    languages: Value,
}

fn parse_languages(v: &Value) -> Vec<(String, u64)> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|pair| Some((pair[0].as_str()?.to_string(), pair[1].as_u64()?)))
        .collect()
}

/// Enqueue a languages computation unless one is already pending.
pub async fn enqueue_languages(conn: &mut sqlx::PgConnection, repo_id: i64) -> ApiResult<()> {
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND failed_at IS NULL
                          AND (payload->>'repo_id')::bigint = $2)",
    )
    .bind(ComputeLanguages::KIND)
    .bind(repo_id)
    .fetch_one(&mut *conn)
    .await?;
    if !pending {
        bgh_core::jobs::enqueue_job(&mut *conn, &ComputeLanguages { repo_id }).await?;
    }
    Ok(())
}

/// `GET /repos/{owner}/{repo}/languages`: last computed statistics; a
/// recomputation is queued when the default branch moved.
async fn languages(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let row: Option<LanguagesRow> =
        sqlx::query_as("SELECT commit_sha, languages FROM repo_languages WHERE repo_id = $1")
            .bind(access.repo.id)
            .fetch_optional(&state.db)
            .await?;
    let head = crate::store(&state)
        .cli(access.repo.id)?
        .resolve_commit(&access.repo.default_branch)
        .await?;
    let fresh = match (&row, &head) {
        (Some(r), Some(h)) => &r.commit_sha == h,
        (_, None) => true,
        (None, Some(_)) => false,
    };
    if !fresh {
        let mut conn = state.db.acquire().await?;
        enqueue_languages(&mut conn, access.repo.id).await?;
    }
    let langs = match (row, head) {
        (Some(r), Some(_)) => parse_languages(&r.languages),
        _ => vec![],
    };
    Ok(Json(serde_json::to_value(OrderedLanguages(langs))?))
}

/// Job: tally languages of the default branch tree (cached by tree SHA)
/// and update `repositories.language`.
pub async fn compute_languages(state: AppState, job: ComputeLanguages) -> anyhow::Result<()> {
    let Some(repo) = db::Repository::find(&state.db, job.repo_id).await? else {
        return Ok(());
    };
    let store = crate::store(&state);
    let Ok(git) = store.cli(repo.id) else {
        return Ok(());
    };
    let Some(head) = git.resolve_commit(&repo.default_branch).await? else {
        sqlx::query("DELETE FROM repo_languages WHERE repo_id = $1")
            .bind(repo.id)
            .execute(&state.db)
            .await?;
        return Ok(());
    };
    let current: Option<String> =
        sqlx::query_scalar("SELECT commit_sha FROM repo_languages WHERE repo_id = $1")
            .bind(repo.id)
            .fetch_optional(&state.db)
            .await?;
    if current.as_deref() == Some(head.as_str()) {
        return Ok(());
    }
    let tree = git.commit(&head).await?.tree;
    let langs: Vec<(String, u64)> = cache::cached(&state, &format!("languages:{tree}"), || async {
        let entries = git.ls_tree(&tree, true).await?;
        Ok(bgh_git::languages::tally(&entries))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let primary = langs.first().map(|(l, _)| l.clone());
    let json = Value::Array(
        langs
            .iter()
            .map(|(l, n)| serde_json::json!([l, n]))
            .collect(),
    );

    let mut tx = Tx::begin(&state).await?;
    sqlx::query(
        "INSERT INTO repo_languages (repo_id, commit_sha, languages, computed_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (repo_id) DO UPDATE
            SET commit_sha = EXCLUDED.commit_sha, languages = EXCLUDED.languages,
                computed_at = now()",
    )
    .bind(repo.id)
    .bind(&head)
    .bind(&json)
    .execute(&mut *tx)
    .await?;
    if primary != repo.language {
        sqlx::query("UPDATE repositories SET language = $2 WHERE id = $1")
            .bind(repo.id)
            .bind(&primary)
            .execute(&mut *tx)
            .await?;
        tx.sync_model(SyncModel::Repo, repo.id, SyncAction::Update)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    tx.commit().await?;
    Ok(())
}

// ----- contributors ------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ContributorsQuery {
    anon: Option<String>,
}

#[derive(Serialize)]
struct UserContributor {
    #[serde(flatten)]
    user: SimpleUser,
    contributions: u64,
}

#[derive(Serialize)]
struct AnonContributor {
    email: String,
    name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    contributions: u64,
}

/// `GET /repos/{owner}/{repo}/contributors` (`anon=1|true` includes commit
/// authors without an account). 204 for empty repositories.
async fn contributors(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ContributorsQuery>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let git = crate::store(&state).cli(access.repo.id)?;
    let Some(head) = git.resolve_commit(&access.repo.default_branch).await? else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let anon = matches!(q.anon.as_deref(), Some("1" | "true"));
    let shortlog: Vec<bgh_git::ops::Contributor> = cache::cached(
        &state,
        &format!("shortlog:{}:{head}", access.repo.id),
        || async { Ok(git.shortlog(&head).await?) },
    )
    .await?;
    let users = users_by_email(&state, shortlog.iter().map(|c| c.email.as_str())).await?;

    // Aggregate per account (several emails may map to one user).
    let mut per_user: HashMap<i64, (db::User, u64)> = HashMap::new();
    let mut anonymous: Vec<&bgh_git::ops::Contributor> = Vec::new();
    for c in &shortlog {
        match users.get(&c.email.to_ascii_lowercase()) {
            Some(u) => per_user.entry(u.id).or_insert_with(|| (u.clone(), 0)).1 += c.commits,
            None => anonymous.push(c),
        }
    }
    let mut items: Vec<(u64, Value)> = per_user
        .into_values()
        .map(|(u, n)| {
            let v = serde_json::to_value(UserContributor {
                user: SimpleUser::new(&state.urls, &u),
                contributions: n,
            });
            (n, v.unwrap_or(Value::Null))
        })
        .collect();
    if anon {
        for c in anonymous {
            items.push((
                c.commits,
                serde_json::to_value(AnonContributor {
                    email: c.email.clone(),
                    name: c.name.clone(),
                    kind: "Anonymous",
                    contributions: c.commits,
                })?,
            ));
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.0));
    let total = items.len() as i64;
    let page: Vec<Value> = items
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|(_, v)| v)
        .collect();
    Ok(p.page_with_total(page, total).into_response())
}

// ----- tags -------------------------------------------------------------------

#[derive(Serialize)]
struct TagJson {
    name: String,
    commit: ShaUrl,
    zipball_url: String,
    tarball_url: String,
    node_id: String,
}

/// Natural ordering key: digit runs compare numerically (`v10` > `v9`).
fn natural_key(s: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                n.push(d);
                chars.next();
            }
            out.push((1, n.parse().unwrap_or(u64::MAX), String::new()));
        } else {
            let mut t = String::new();
            while let Some(&d) = chars.peek().filter(|d| !d.is_ascii_digit()) {
                t.push(d);
                chars.next();
            }
            out.push((0, 0, t));
        }
    }
    out
}

/// `GET /repos/{owner}/{repo}/tags`: newest version first.
async fn tags(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<TagJson>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let mut refs = crate::store(&state)
        .read(access.repo.id, |r| r.tags())
        .await?;
    refs.sort_by_key(|r| std::cmp::Reverse(natural_key(r.short_name())));
    let total = refs.len() as i64;
    let r = RepoRef::new(&state.urls, &access);
    let items = refs
        .into_iter()
        .skip(p.offset() as usize)
        .take(p.limit() as usize)
        .map(|t| {
            let name = t.short_name().to_string();
            TagJson {
                commit: short_commit(&r, &t.peeled),
                zipball_url: r.api(&format!("/zipball/refs/tags/{}", encode_path(&name))),
                tarball_url: r.api(&format!("/tarball/refs/tags/{}", encode_path(&name))),
                node_id: node_id::encode_str(
                    NodeType::Ref,
                    &format!("{}:refs/tags/{name}", access.repo.id),
                ),
                name,
            }
        })
        .collect();
    Ok(p.page_with_total(items, total))
}

// ----- teams --------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TeamGrant {
    #[sqlx(flatten)]
    team: db::Team,
    grant: String,
}

/// `GET /repos/{owner}/{repo}/teams`: teams with access, with the
/// permission granted on this repository.
async fn teams(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Page<Team>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let rows: Vec<TeamGrant> = sqlx::query_as(&format!(
        "SELECT {}, tr.permission AS grant FROM team_repos tr JOIN teams t ON t.id = tr.team_id
          WHERE tr.repo_id = $1 ORDER BY lower(t.name), t.id LIMIT $2 OFFSET $3",
        db::prefixed("t", db::Team::COLUMNS)
    ))
    .bind(access.repo.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let page = p.page(rows);
    let parent_ids: Vec<i64> = page.items.iter().filter_map(|r| r.team.parent_id).collect();
    let parents: HashMap<i64, db::Team> = if parent_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, db::Team>(&format!(
            "SELECT {} FROM teams WHERE id = ANY($1)",
            db::Team::COLUMNS
        ))
        .bind(&parent_ids)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect()
    };
    let org = access.owner.login.clone();
    Ok(page.map(|row| {
        let mut team = TeamSimple::new(&state.urls, &org, &row.team);
        team.permission = Permission::parse(&row.grant)
            .unwrap_or(Permission::Read)
            .legacy_name()
            .to_string();
        Team {
            parent: row
                .team
                .parent_id
                .and_then(|id| parents.get(&id))
                .map(|t| TeamSimple::new(&state.urls, &org, t)),
            team,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::natural_key;

    #[test]
    fn natural_order() {
        let mut v = vec!["v1.9.0", "v1.10.0", "v1.2.0", "alpha"];
        v.sort_by_key(|s| std::cmp::Reverse(natural_key(s)));
        assert_eq!(v, vec!["v1.10.0", "v1.9.0", "v1.2.0", "alpha"]);
    }
}
