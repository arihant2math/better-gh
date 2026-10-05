//! Commit statistics of the default branch (package P31):
//!
//! * `GET /repos/{o}/{r}/stats/contributors`
//! * `GET /repos/{o}/{r}/stats/commit_activity`
//! * `GET /repos/{o}/{r}/stats/code_frequency`
//! * `GET /repos/{o}/{r}/stats/participation`
//! * `GET /repos/{o}/{r}/stats/punch_card`
//!
//! One `git log` pass (the `repos.compute_stats` job) aggregates per-author
//! weekly additions/deletions/commits, daily commit counts and the punch
//! card into `repo_stats`, keyed by the default-branch head. Like GitHub,
//! a request whose head has no cached statistics queues the job and
//! answers `202 Accepted` with `{}`; clients retry until they get `200`.
//! Repositories with 10,000+ commits skip line counts: additions and
//! deletions are `0` and `code_frequency` answers 422.

use std::collections::{BTreeMap, HashMap};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::jobs::JobPayload;
use bgh_core::models::api::SimpleUser;
use bgh_core::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::identity::users_by_email;

/// Above this many commits, line statistics are not computed (GitHub's limit).
pub const LARGE_REPO_COMMITS: u64 = 10_000;
const WEEK: i64 = 7 * 86_400;
const DAY: i64 = 86_400;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/repos/{owner}/{repo}/stats/contributors",
            get(contributors),
        )
        .route(
            "/repos/{owner}/{repo}/stats/commit_activity",
            get(commit_activity),
        )
        .route(
            "/repos/{owner}/{repo}/stats/code_frequency",
            get(code_frequency),
        )
        .route(
            "/repos/{owner}/{repo}/stats/participation",
            get(participation),
        )
        .route("/repos/{owner}/{repo}/stats/punch_card", get(punch_card))
}

// ----- data -------------------------------------------------------------------

/// `[week, additions, deletions, commits]` (week = Sunday 00:00 UTC, unix seconds).
type WeekRow = [i64; 4];

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthorStats {
    pub email: String,
    pub name: String,
    pub weeks: Vec<WeekRow>,
}

/// What `repo_stats.data` holds (all sparse except `punch`).
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatsData {
    pub authors: Vec<AuthorStats>,
    pub weeks: Vec<WeekRow>,
    /// `[day, commits]` (day = 00:00 UTC, unix seconds).
    pub days: Vec<[i64; 2]>,
    /// `[weekday (0 = Sunday), hour, commits]`, 168 rows, author local time.
    pub punch: Vec<[i64; 3]>,
}

/// Start of the (Sunday-based, UTC) week containing `t`.
pub fn week_of(t: i64) -> i64 {
    let day = t.div_euclid(DAY);
    // 1970-01-01 was a Thursday.
    let dow = (day + 4).rem_euclid(7);
    (day - dow) * DAY
}

/// `git log` record separator and field separator.
const RS: char = '\u{1e}';
const FS: char = '\u{1f}';

/// Aggregate `git log --format=<RS>%at<FS>%aE<FS>%aN<FS>%ad [--numstat]`
/// output (`--date=format:%w,%H`).
pub fn aggregate(log: &str) -> StatsData {
    let mut authors: BTreeMap<String, (String, BTreeMap<i64, [i64; 3]>)> = BTreeMap::new();
    let mut weeks: BTreeMap<i64, [i64; 3]> = BTreeMap::new();
    let mut days: BTreeMap<i64, i64> = BTreeMap::new();
    let mut punch = [[0i64; 24]; 7];
    for record in log.split(RS).filter(|r| !r.trim().is_empty()) {
        let mut lines = record.lines();
        let Some(header) = lines.next() else { continue };
        let f: Vec<&str> = header.split(FS).collect();
        let [at, email, name, local] = f[..] else {
            continue;
        };
        let Ok(at) = at.trim().parse::<i64>() else {
            continue;
        };
        let (mut added, mut deleted) = (0i64, 0i64);
        for l in lines {
            let mut parts = l.split('\t');
            let (Some(a), Some(d), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            added += a.parse::<i64>().unwrap_or(0);
            deleted += d.parse::<i64>().unwrap_or(0);
        }
        let w = week_of(at);
        let key = email.to_ascii_lowercase();
        let entry = authors
            .entry(key)
            .or_insert_with(|| (name.to_string(), BTreeMap::new()));
        let aw = entry.1.entry(w).or_default();
        aw[0] += added;
        aw[1] += deleted;
        aw[2] += 1;
        let tw = weeks.entry(w).or_default();
        tw[0] += added;
        tw[1] += deleted;
        tw[2] += 1;
        *days.entry(at.div_euclid(DAY) * DAY).or_default() += 1;
        if let Some((d, h)) = local.split_once(',')
            && let (Ok(d), Ok(h)) = (d.trim().parse::<usize>(), h.trim().parse::<usize>())
            && d < 7
            && h < 24
        {
            punch[d][h] += 1;
        }
    }
    let rows = |m: BTreeMap<i64, [i64; 3]>| -> Vec<WeekRow> {
        m.into_iter().map(|(w, [a, d, c])| [w, a, d, c]).collect()
    };
    StatsData {
        authors: authors
            .into_iter()
            .map(|(email, (name, w))| AuthorStats {
                email,
                name,
                weeks: rows(w),
            })
            .collect(),
        weeks: rows(weeks),
        days: days.into_iter().map(|(d, c)| [d, c]).collect(),
        punch: (0..7)
            .flat_map(|d| (0..24).map(move |h| (d, h)))
            .map(|(d, h)| [d as i64, h as i64, punch[d][h]])
            .collect(),
    }
}

// ----- job ----------------------------------------------------------------------

/// Recompute `repo_stats` for the default branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeStats {
    pub repo_id: i64,
}

impl JobPayload for ComputeStats {
    const KIND: &'static str = "repos.compute_stats";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Queue a statistics computation unless one is already pending.
pub async fn enqueue_stats(conn: &mut sqlx::PgConnection, repo_id: i64) -> ApiResult<()> {
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND failed_at IS NULL
                          AND (payload->>'repo_id')::bigint = $2)",
    )
    .bind(ComputeStats::KIND)
    .bind(repo_id)
    .fetch_one(&mut *conn)
    .await?;
    if !pending {
        bgh_core::jobs::enqueue_job(&mut *conn, &ComputeStats { repo_id }).await?;
    }
    Ok(())
}

/// After the default branch moved: refresh statistics that were asked for
/// before (repositories nobody looks at are computed on demand only).
pub async fn refresh_if_cached(conn: &mut sqlx::PgConnection, repo_id: i64) -> ApiResult<()> {
    let cached: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM repo_stats WHERE repo_id = $1)")
            .bind(repo_id)
            .fetch_one(&mut *conn)
            .await?;
    if cached {
        enqueue_stats(conn, repo_id).await?;
    }
    Ok(())
}

/// Job: one `git log` pass over the default branch.
pub async fn compute_stats(state: AppState, job: ComputeStats) -> anyhow::Result<()> {
    let Some(repo) = db::Repository::find(&state.db, job.repo_id).await? else {
        return Ok(());
    };
    let Ok(git) = crate::store(&state).cli(repo.id) else {
        return Ok(());
    };
    let Some(head) = git.resolve_commit(&repo.default_branch).await? else {
        sqlx::query("DELETE FROM repo_stats WHERE repo_id = $1")
            .bind(repo.id)
            .execute(&state.db)
            .await?;
        return Ok(());
    };
    let current: Option<String> =
        sqlx::query_scalar("SELECT commit_sha FROM repo_stats WHERE repo_id = $1")
            .bind(repo.id)
            .fetch_optional(&state.db)
            .await?;
    if current.as_deref() == Some(head.as_str()) {
        return Ok(());
    }
    let count = git.count(&head, None).await?;
    let format = format!("--format={RS}%at{FS}%aE{FS}%aN{FS}%ad");
    let mut args = vec!["log", "--no-renames", &format, "--date=format:%w,%H"];
    if count < LARGE_REPO_COMMITS {
        args.push("--numstat");
    }
    args.extend([head.as_str(), "--"]);
    let out = git.run(&args, &[], None).await?;
    let data = aggregate(&String::from_utf8_lossy(&out));
    sqlx::query(
        "INSERT INTO repo_stats (repo_id, commit_sha, commit_count, data, computed_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (repo_id) DO UPDATE
            SET commit_sha = EXCLUDED.commit_sha, commit_count = EXCLUDED.commit_count,
                data = EXCLUDED.data, computed_at = now()",
    )
    .bind(repo.id)
    .bind(&head)
    .bind(count as i64)
    .bind(serde_json::to_value(&data)?)
    .execute(&state.db)
    .await?;
    Ok(())
}

// ----- handlers -----------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct StatsRow {
    commit_sha: String,
    commit_count: i64,
    data: Value,
}

enum Loaded {
    /// Empty repository (no default branch commit).
    Empty,
    /// Being computed: 202.
    Pending,
    Ready {
        commit_count: u64,
        data: StatsData,
    },
}

async fn load(state: &AppState, access: &RepoAccess) -> ApiResult<Loaded> {
    let head = crate::store(state)
        .cli(access.repo.id)?
        .resolve_commit(&access.repo.default_branch)
        .await?;
    let Some(head) = head else {
        return Ok(Loaded::Empty);
    };
    let row: Option<StatsRow> =
        sqlx::query_as("SELECT commit_sha, commit_count, data FROM repo_stats WHERE repo_id = $1")
            .bind(access.repo.id)
            .fetch_optional(&state.db)
            .await?;
    match row {
        Some(r) if r.commit_sha == head => Ok(Loaded::Ready {
            commit_count: r.commit_count.max(0) as u64,
            data: serde_json::from_value(r.data).unwrap_or_default(),
        }),
        _ => {
            let mut conn = state.db.acquire().await?;
            enqueue_stats(&mut conn, access.repo.id).await?;
            Ok(Loaded::Pending)
        }
    }
}

fn accepted() -> Response {
    (StatusCode::ACCEPTED, Json(json!({}))).into_response()
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The 52 week starts ending with the current week, oldest first.
fn last_52_weeks(now: i64) -> Vec<i64> {
    let current = week_of(now);
    (0..52).rev().map(|i| current - i * WEEK).collect()
}

/// Dense week starts from the first to the last week of `weeks`.
fn week_span(weeks: &[WeekRow]) -> Vec<i64> {
    match (weeks.first(), weeks.last()) {
        (Some(f), Some(l)) => (0..=((l[0] - f[0]) / WEEK))
            .map(|i| f[0] + i * WEEK)
            .collect(),
        _ => vec![],
    }
}

#[derive(Serialize)]
struct ContributorWeek {
    w: i64,
    a: i64,
    d: i64,
    c: i64,
}

#[derive(Serialize)]
struct ContributorStats {
    author: SimpleUser,
    total: i64,
    weeks: Vec<ContributorWeek>,
}

/// `GET /repos/{owner}/{repo}/stats/contributors`: commit authors with an
/// account (top 100 by commits), each with weekly a/d/c over the whole
/// history, ordered by total ascending like GitHub.
async fn contributors(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (commit_count, data) = match load(&state, &access).await? {
        Loaded::Empty => return Ok(StatusCode::NO_CONTENT.into_response()),
        Loaded::Pending => return Ok(accepted()),
        Loaded::Ready { commit_count, data } => (commit_count, data),
    };
    let large = commit_count >= LARGE_REPO_COMMITS;
    let users = users_by_email(&state, data.authors.iter().map(|a| a.email.as_str())).await?;
    let mut per_user: HashMap<i64, (db::User, BTreeMap<i64, [i64; 3]>)> = HashMap::new();
    for a in &data.authors {
        let Some(u) = users.get(&a.email) else {
            continue;
        };
        let e = per_user
            .entry(u.id)
            .or_insert_with(|| (u.clone(), BTreeMap::new()));
        for [w, ad, de, c] in &a.weeks {
            let x = e.1.entry(*w).or_default();
            x[0] += ad;
            x[1] += de;
            x[2] += c;
        }
    }
    let span = week_span(&data.weeks);
    let mut out: Vec<ContributorStats> = per_user
        .into_values()
        .map(|(u, weeks)| ContributorStats {
            author: SimpleUser::new(&state.urls, &u),
            total: weeks.values().map(|x| x[2]).sum(),
            weeks: span
                .iter()
                .map(|w| {
                    let [a, d, c] = weeks.get(w).copied().unwrap_or_default();
                    let (a, d) = if large { (0, 0) } else { (a, d) };
                    ContributorWeek { w: *w, a, d, c }
                })
                .collect(),
        })
        .collect();
    // Top 100 by commits, then ascending.
    out.sort_by(|a, b| b.total.cmp(&a.total).then(a.author.id.cmp(&b.author.id)));
    out.truncate(100);
    out.reverse();
    Ok(Json(out).into_response())
}

#[derive(Serialize)]
struct WeekActivity {
    days: [i64; 7],
    total: i64,
    week: i64,
}

/// `GET /repos/{owner}/{repo}/stats/commit_activity`: the last 52 weeks,
/// commits per day (Sunday first).
async fn commit_activity(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let data = match load(&state, &access).await? {
        Loaded::Empty => return Ok(StatusCode::NO_CONTENT.into_response()),
        Loaded::Pending => return Ok(accepted()),
        Loaded::Ready { data, .. } => data,
    };
    Ok(Json(commit_activity_of(&data, now_secs())).into_response())
}

fn commit_activity_of(data: &StatsData, now: i64) -> Vec<WeekActivity> {
    let days: HashMap<i64, i64> = data.days.iter().map(|[d, c]| (*d, *c)).collect();
    last_52_weeks(now)
        .into_iter()
        .map(|week| {
            let mut d = [0i64; 7];
            for (i, slot) in d.iter_mut().enumerate() {
                *slot = days.get(&(week + i as i64 * DAY)).copied().unwrap_or(0);
            }
            WeekActivity {
                days: d,
                total: d.iter().sum(),
                week,
            }
        })
        .collect()
}

/// `GET /repos/{owner}/{repo}/stats/code_frequency`: `[week, additions,
/// -deletions]` for every week of the history; 422 for 10k+ commits.
async fn code_frequency(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let (commit_count, data) = match load(&state, &access).await? {
        Loaded::Empty => return Ok(StatusCode::NO_CONTENT.into_response()),
        Loaded::Pending => return Ok(accepted()),
        Loaded::Ready { commit_count, data } => (commit_count, data),
    };
    if commit_count >= LARGE_REPO_COMMITS {
        return Err(ApiError::unprocessable(
            "This repository contains more than 10,000 commits. Code frequency statistics are unavailable for repositories of this size.",
        ));
    }
    let weeks: HashMap<i64, WeekRow> = data.weeks.iter().map(|w| (w[0], *w)).collect();
    let out: Vec<[i64; 3]> = week_span(&data.weeks)
        .into_iter()
        .map(|w| {
            let r = weeks.get(&w).copied().unwrap_or([w, 0, 0, 0]);
            [w, r[1], -r[2]]
        })
        .collect();
    Ok(Json(out).into_response())
}

/// `GET /repos/{owner}/{repo}/stats/participation`: weekly commit counts
/// of the last 52 weeks, for everyone and for the repository owner.
async fn participation(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let data = match load(&state, &access).await? {
        Loaded::Empty => StatsData::default(),
        Loaded::Pending => return Ok(accepted()),
        Loaded::Ready { data, .. } => data,
    };
    let users = users_by_email(&state, data.authors.iter().map(|a| a.email.as_str())).await?;
    let owner_id = access.owner.id;
    let weeks = last_52_weeks(now_secs());
    let index: HashMap<i64, usize> = weeks.iter().enumerate().map(|(i, w)| (*w, i)).collect();
    let (mut all, mut mine) = (vec![0i64; 52], vec![0i64; 52]);
    for a in &data.authors {
        let is_owner = users.get(&a.email).is_some_and(|u| u.id == owner_id);
        for [w, _, _, c] in &a.weeks {
            if let Some(&i) = index.get(w) {
                all[i] += c;
                if is_owner {
                    mine[i] += c;
                }
            }
        }
    }
    Ok(Json(json!({ "all": all, "owner": mine })).into_response())
}

/// `GET /repos/{owner}/{repo}/stats/punch_card`: `[day, hour, commits]`.
async fn punch_card(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    match load(&state, &access).await? {
        Loaded::Empty => Ok(StatusCode::NO_CONTENT.into_response()),
        Loaded::Pending => Ok(accepted()),
        Loaded::Ready { data, .. } => Ok(Json(data.punch).into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_start_on_sunday() {
        // 2024-01-03 (Wednesday) 12:00 UTC → Sunday 2023-12-31.
        assert_eq!(week_of(1_704_283_200), 1_703_980_800);
        assert_eq!(week_of(1_703_980_800), 1_703_980_800);
    }

    #[test]
    fn aggregates_log() {
        let log = format!(
            "{RS}1704283200{FS}A@x.io{FS}Ann{FS}3,12\n\n3\t1\tsrc/a.rs\n-\t-\tbin\n\
             {RS}1704283300{FS}a@x.io{FS}Ann{FS}3,13\n\n1\t0\tb\n\
             {RS}1703000000{FS}b@x.io{FS}Bob{FS}0,9\n"
        );
        let d = aggregate(&log);
        assert_eq!(d.authors.len(), 2);
        let ann = &d.authors[0];
        assert_eq!(ann.email, "a@x.io");
        assert_eq!(ann.weeks, vec![[1_703_980_800, 4, 1, 2]]);
        assert_eq!(d.weeks.len(), 2);
        assert_eq!(d.punch.len(), 168);
        assert_eq!(d.punch[3 * 24 + 12], [3, 12, 1]);
        assert_eq!(d.punch[9], [0, 9, 1]);
        let act = commit_activity_of(&d, 1_704_283_200);
        assert_eq!(act.len(), 52);
        assert_eq!(act[51].week, 1_703_980_800);
        assert_eq!(act[51].days, [0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(act[51].total, 2);
    }
}
