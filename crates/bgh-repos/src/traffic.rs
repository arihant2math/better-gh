//! Repository traffic (package P31): clones and page views of the last
//! 14 days.
//!
//! * `GET /repos/{o}/{r}/traffic/views` / `traffic/clones` (`per=day|week`)
//! * `GET /repos/{o}/{r}/traffic/popular/paths` / `popular/referrers`
//! * `POST /_bgh/traffic/views`: page-view beacon of the web client
//!
//! Reading traffic needs push access (403 for readers). A clone is an
//! upload-pack negotiation that wants objects without any `have` line
//! ([`CloneTap`], over HTTP and SSH). Visitors are counted per account, or
//! per salted IP hash when anonymous. Counters older than
//! [`RETENTION_DAYS`] are pruned by the `repos.traffic_prune` service.

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use bgh_core::prelude::*;
use chrono::{Datelike, Duration as ChronoDuration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

/// Days of traffic shown by the API.
pub const WINDOW_DAYS: i64 = 14;
/// Days of counters kept.
pub const RETENTION_DAYS: i64 = 31;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/repos/{owner}/{repo}/traffic/views", get(views))
        .route("/repos/{owner}/{repo}/traffic/clones", get(clones))
        .route("/repos/{owner}/{repo}/traffic/popular/paths", get(paths))
        .route(
            "/repos/{owner}/{repo}/traffic/popular/referrers",
            get(referrers),
        )
}

pub fn web_routes() -> Router<AppState> {
    Router::new().route("/_bgh/traffic/views", post(record_view))
}

// ----- visitors -----------------------------------------------------------------

/// Visitor key: `u:<user id>`, else a salted hash of the client IP.
pub fn visitor(state: &AppState, user_id: Option<i64>, ip: &str) -> String {
    match user_id {
        Some(id) => format!("u:{id}"),
        None => {
            let h = bgh_core::crypto::sha256_hex(&format!(
                "bgh-traffic\0{}\0{ip}",
                state.config.base_url
            ));
            format!("ip:{}", &h[..32])
        }
    }
}

// ----- clones -------------------------------------------------------------------

/// Count one clone of `repo_id` by `visitor` today.
pub async fn record_clone(state: &AppState, repo_id: i64, visitor: &str) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO repo_traffic_clones (repo_id, day, visitor)
         VALUES ($1, (now() AT TIME ZONE 'UTC')::date, $2)
         ON CONFLICT (repo_id, day, visitor)
            DO UPDATE SET count = repo_traffic_clones.count + 1",
    )
    .bind(repo_id)
    .bind(visitor)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Wrap an upload-pack request stream so a clone is recorded once the
/// negotiation shows it (wants, then `done` without any `have`).
pub fn observe_clone<R: AsyncRead + Send + Unpin + 'static>(
    state: &AppState,
    repo_id: i64,
    visitor: String,
    input: R,
) -> CloneTap<R> {
    let state = state.clone();
    CloneTap::new(
        input,
        Box::new(move || {
            tokio::spawn(async move {
                if let Err(e) = record_clone(&state, repo_id, &visitor).await {
                    tracing::warn!(error = %e, repo_id, "recording clone failed");
                }
            });
        }),
    )
}

/// Smart-HTTP variant of [`observe_clone`]: decodes the (possibly gzipped)
/// body and drops `Content-Encoding` from `headers` accordingly.
pub fn observe_http_clone(
    state: &AppState,
    repo_id: i64,
    visitor: String,
    headers: &mut HeaderMap,
    body: Body,
) -> Body {
    let reader = bgh_git::smart_http::body_reader(headers, body);
    headers.remove(header::CONTENT_ENCODING);
    let tap = observe_clone(state, repo_id, visitor, reader);
    Body::from_stream(ReaderStream::with_capacity(tap, 64 * 1024))
}

/// Stop inspecting after this many bytes (large `have` negotiations).
const TAP_LIMIT: usize = 1 << 20;

/// Pass-through reader that parses pkt-lines of an upload-pack request and
/// calls `on_clone` when the client wants objects without having any.
pub struct CloneTap<R> {
    inner: R,
    buf: Vec<u8>,
    seen: usize,
    wants: usize,
    on_clone: Option<Box<dyn FnOnce() + Send>>,
}

impl<R> CloneTap<R> {
    pub fn new(inner: R, on_clone: Box<dyn FnOnce() + Send>) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            seen: 0,
            wants: 0,
            on_clone: Some(on_clone),
        }
    }

    fn inspect(&mut self, data: &[u8]) {
        if self.on_clone.is_none() {
            return;
        }
        self.seen += data.len();
        if self.seen > TAP_LIMIT {
            self.stop();
            return;
        }
        self.buf.extend_from_slice(data);
        loop {
            let Some(len) = self.buf.get(..4) else {
                return;
            };
            let Some(len) = std::str::from_utf8(len)
                .ok()
                .and_then(|s| usize::from_str_radix(s, 16).ok())
            else {
                return self.stop();
            };
            if len < 4 {
                // flush / delim / response-end
                self.buf.drain(..4);
                continue;
            }
            if self.buf.len() < len {
                return;
            }
            let line: Vec<u8> = self.buf.drain(..len).skip(4).collect();
            if line.starts_with(b"want ") {
                self.wants += 1;
            } else if line.starts_with(b"have ") {
                return self.stop();
            } else if line.trim_ascii_end() == b"done" {
                if self.wants > 0
                    && let Some(f) = self.on_clone.take()
                {
                    f();
                }
                return self.stop();
            }
        }
    }

    fn stop(&mut self) {
        self.on_clone = None;
        self.buf = Vec::new();
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CloneTap<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = out.filled().len();
        let res = Pin::new(&mut self.inner).poll_read(cx, out);
        if let Poll::Ready(Ok(())) = &res {
            let new = out.filled()[before..].to_vec();
            if !new.is_empty() {
                self.inspect(&new);
            }
        }
        res
    }
}

// ----- page views ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ViewBeacon {
    owner: String,
    repo: String,
    path: String,
    #[serde(default)]
    referrer: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// Referrer host (`github.com`, `google.com`); `""` when absent or invalid.
fn referrer_host(r: Option<&str>) -> String {
    r.and_then(|r| url::Url::parse(r).ok())
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        })
        .map(|h| h.chars().take(255).collect())
        .unwrap_or_default()
}

/// `POST /_bgh/traffic/views`: the web client reports a repository page
/// view. Always 204 (never reveals whether the repository exists).
async fn record_view(State(state): State<AppState>, req: Request) -> ApiResult<StatusCode> {
    let ip = bgh_core::auth::client_ip(&state.config, req.headers(), req.extensions());
    let (parts, body) = req.into_parts();
    let auth = bgh_core::auth::authenticate(&state, &parts.headers, Default::default())
        .await
        .ok()
        .flatten();
    let bytes = axum::body::to_bytes(body, 16 * 1024)
        .await
        .map_err(|_| ApiError::bad_request("Problems parsing JSON"))?;
    let Ok(b) = serde_json::from_slice::<ViewBeacon>(&bytes) else {
        return Err(ApiError::bad_request("Problems parsing JSON"));
    };
    let Ok(access) = RepoAccess::load(&state, auth.as_ref(), &b.owner, &b.repo).await else {
        return Ok(StatusCode::NO_CONTENT);
    };
    // Canonical `/{owner}/{repo}{rest}` path (query and hash stripped).
    let rest = b
        .path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .splitn(4, '/')
        .nth(3)
        .unwrap_or("");
    let rest: String = rest.trim_end_matches('/').chars().take(512).collect();
    let path = if rest.is_empty() {
        format!("/{}/{}", access.owner.login, access.repo.name)
    } else {
        format!("/{}/{}/{rest}", access.owner.login, access.repo.name)
    };
    let title: String = b.title.unwrap_or_default().chars().take(255).collect();
    let visitor = visitor(&state, auth.as_ref().map(|a| a.user.id), &ip);
    sqlx::query(
        "INSERT INTO repo_traffic_views (repo_id, day, visitor, path, referrer, title)
         VALUES ($1, (now() AT TIME ZONE 'UTC')::date, $2, $3, $4, $5)
         ON CONFLICT (repo_id, day, visitor, path, referrer)
            DO UPDATE SET count = repo_traffic_views.count + 1, title = EXCLUDED.title",
    )
    .bind(access.repo.id)
    .bind(&visitor)
    .bind(&path)
    .bind(referrer_host(b.referrer.as_deref()))
    .bind(&title)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- API ------------------------------------------------------------------------

async fn load_push(
    state: &AppState,
    auth: &MaybeUser,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, auth.as_ref(), owner, repo).await?;
    if access.permission < Permission::Write {
        return Err(if auth.as_ref().is_some() {
            ApiError::forbidden("Must have push access to repository.")
        } else {
            ApiError::NotFound
        });
    }
    Ok(access)
}

#[derive(Debug, Deserialize)]
struct PerQuery {
    per: Option<String>,
}

#[derive(Serialize)]
struct Bucket {
    timestamp: Timestamp,
    count: i64,
    uniques: i64,
}

#[derive(Serialize)]
struct ViewsBody {
    count: i64,
    uniques: i64,
    views: Vec<Bucket>,
}

#[derive(Serialize)]
struct ClonesBody {
    count: i64,
    uniques: i64,
    clones: Vec<Bucket>,
}

#[derive(sqlx::FromRow)]
struct DayVisitor {
    day: NaiveDate,
    visitor: String,
    count: i64,
}

fn first_day(today: NaiveDate) -> NaiveDate {
    today - ChronoDuration::days(WINDOW_DAYS - 1)
}

/// Daily (or Monday-based weekly) buckets over the window, plus totals.
fn buckets(rows: &[DayVisitor], today: NaiveDate, weekly: bool) -> (i64, i64, Vec<Bucket>) {
    use std::collections::{BTreeMap, HashSet};
    let start = first_day(today);
    let key = |d: NaiveDate| {
        if weekly {
            d - ChronoDuration::days(d.weekday().num_days_from_monday() as i64)
        } else {
            d
        }
    };
    let mut map: BTreeMap<NaiveDate, (i64, HashSet<&str>)> = BTreeMap::new();
    let mut d = start;
    while d <= today {
        map.entry(key(d)).or_default();
        d += ChronoDuration::days(1);
    }
    let mut all: HashSet<&str> = HashSet::new();
    let mut total = 0;
    for r in rows {
        if r.day < start || r.day > today {
            continue;
        }
        let e = map.entry(key(r.day)).or_default();
        e.0 += r.count;
        e.1.insert(&r.visitor);
        all.insert(&r.visitor);
        total += r.count;
    }
    let list = map
        .into_iter()
        .map(|(d, (count, v))| Bucket {
            timestamp: Timestamp(d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc()),
            count,
            uniques: v.len() as i64,
        })
        .collect();
    (total, all.len() as i64, list)
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

fn weekly(q: &PerQuery) -> ApiResult<bool> {
    match q.per.as_deref() {
        None | Some("day") => Ok(false),
        Some("week") => Ok(true),
        Some(_) => Err(ApiError::invalid_field(FieldError::invalid(
            "Traffic", "per",
        ))),
    }
}

/// `GET /repos/{owner}/{repo}/traffic/views`
async fn views(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<PerQuery>,
) -> ApiResult<Json<ViewsBody>> {
    let access = load_push(&state, &auth, &owner, &repo).await?;
    let weekly = weekly(&q)?;
    let today = today();
    let rows: Vec<DayVisitor> = sqlx::query_as(
        "SELECT day, visitor, sum(count)::bigint AS count FROM repo_traffic_views
          WHERE repo_id = $1 AND day >= $2 GROUP BY day, visitor",
    )
    .bind(access.repo.id)
    .bind(first_day(today))
    .fetch_all(&state.db)
    .await?;
    let (count, uniques, views) = buckets(&rows, today, weekly);
    Ok(Json(ViewsBody {
        count,
        uniques,
        views,
    }))
}

/// `GET /repos/{owner}/{repo}/traffic/clones`
async fn clones(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<PerQuery>,
) -> ApiResult<Json<ClonesBody>> {
    let access = load_push(&state, &auth, &owner, &repo).await?;
    let weekly = weekly(&q)?;
    let today = today();
    let rows: Vec<DayVisitor> = sqlx::query_as(
        "SELECT day, visitor, count::bigint AS count FROM repo_traffic_clones
          WHERE repo_id = $1 AND day >= $2",
    )
    .bind(access.repo.id)
    .bind(first_day(today))
    .fetch_all(&state.db)
    .await?;
    let (count, uniques, clones) = buckets(&rows, today, weekly);
    Ok(Json(ClonesBody {
        count,
        uniques,
        clones,
    }))
}

#[derive(Serialize, sqlx::FromRow)]
struct PopularPath {
    path: String,
    title: String,
    count: i64,
    uniques: i64,
}

#[derive(Serialize, sqlx::FromRow)]
struct PopularReferrer {
    referrer: String,
    count: i64,
    uniques: i64,
}

/// `GET /repos/{owner}/{repo}/traffic/popular/paths`: top 10 over 14 days.
async fn paths(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<PopularPath>>> {
    let access = load_push(&state, &auth, &owner, &repo).await?;
    let rows: Vec<PopularPath> = sqlx::query_as(
        "SELECT path, (array_agg(title ORDER BY day DESC))[1] AS title,
                sum(count)::bigint AS count, count(DISTINCT visitor) AS uniques
           FROM repo_traffic_views WHERE repo_id = $1 AND day >= $2
          GROUP BY path ORDER BY count DESC, uniques DESC, path LIMIT 10",
    )
    .bind(access.repo.id)
    .bind(first_day(today()))
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

/// `GET /repos/{owner}/{repo}/traffic/popular/referrers`: top 10 over 14 days.
async fn referrers(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<PopularReferrer>>> {
    let access = load_push(&state, &auth, &owner, &repo).await?;
    let rows: Vec<PopularReferrer> = sqlx::query_as(
        "SELECT referrer, sum(count)::bigint AS count, count(DISTINCT visitor) AS uniques
           FROM repo_traffic_views WHERE repo_id = $1 AND day >= $2 AND referrer <> ''
          GROUP BY referrer ORDER BY count DESC, uniques DESC, referrer LIMIT 10",
    )
    .bind(access.repo.id)
    .bind(first_day(today()))
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

// ----- retention --------------------------------------------------------------

/// Delete counters older than [`RETENTION_DAYS`] (idempotent, so several
/// processes may run it).
pub async fn prune(state: &AppState) -> anyhow::Result<()> {
    let cutoff = today() - ChronoDuration::days(RETENTION_DAYS);
    sqlx::query("DELETE FROM repo_traffic_views WHERE day < $1")
        .bind(cutoff)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM repo_traffic_clones WHERE day < $1")
        .bind(cutoff)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Service `repos.traffic_prune`: prune every 6 hours.
pub async fn prune_service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    loop {
        if let Err(e) = prune(&state).await {
            tracing::warn!(error = %e, "traffic prune failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(6 * 3600)) => {}
            _ = shutdown.cancelled() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(chunks: &[&[u8]]) -> bool {
        let hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let h = hit.clone();
        let mut t = CloneTap::new(
            &b""[..],
            Box::new(move || h.store(true, std::sync::atomic::Ordering::SeqCst)),
        );
        for c in chunks {
            t.inspect(c);
        }
        hit.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn pkt(s: &str) -> Vec<u8> {
        format!("{:04x}{s}", s.len() + 4).into_bytes()
    }

    #[test]
    fn detects_clone_vs_fetch() {
        let a = "a".repeat(40);
        let mut v0 = pkt(&format!("want {a} side-band-64k\n"));
        v0.extend(b"0000");
        v0.extend(pkt("done\n"));
        assert!(tap(&[&v0]));
        // Split anywhere.
        let (x, y) = v0.split_at(7);
        assert!(tap(&[x, y]));

        let mut fetch = pkt(&format!("want {a}\n"));
        fetch.extend(b"0000");
        fetch.extend(pkt(&format!("have {a}\n")));
        fetch.extend(pkt("done\n"));
        assert!(!tap(&[&fetch]));

        let mut v2 = pkt("command=fetch\n");
        v2.extend(pkt("agent=git/2.40\n"));
        v2.extend(b"0001");
        v2.extend(pkt("thin-pack\n"));
        v2.extend(pkt(&format!("want {a}\n")));
        v2.extend(pkt("done\n"));
        v2.extend(b"0000");
        assert!(tap(&[&v2]));

        let mut ls = pkt("command=ls-refs\n");
        ls.extend(b"0000");
        assert!(!tap(&[&ls]));
    }

    #[test]
    fn buckets_window() {
        let today = NaiveDate::from_ymd_opt(2024, 1, 17).unwrap(); // Wednesday
        let rows = vec![
            DayVisitor {
                day: today,
                visitor: "a".into(),
                count: 2,
            },
            DayVisitor {
                day: today - ChronoDuration::days(1),
                visitor: "a".into(),
                count: 1,
            },
            DayVisitor {
                day: today - ChronoDuration::days(1),
                visitor: "b".into(),
                count: 1,
            },
            DayVisitor {
                day: today - ChronoDuration::days(20),
                visitor: "c".into(),
                count: 9,
            },
        ];
        let (count, uniques, days) = buckets(&rows, today, false);
        assert_eq!((count, uniques, days.len()), (4, 2, 14));
        assert_eq!(days[12].uniques, 2);
        let (_, _, weeks) = buckets(&rows, today, true);
        // 2024-01-04 (Thu) .. 2024-01-17 (Wed): Mondays 1st, 8th, 15th.
        assert_eq!(weeks.len(), 3);
        assert_eq!(weeks[2].count, 4);
        assert_eq!(weeks[2].uniques, 2);
    }

    #[test]
    fn referrer_hosts() {
        assert_eq!(
            referrer_host(Some("https://www.google.com/search?q=x")),
            "google.com"
        );
        assert_eq!(referrer_host(Some("nonsense")), "");
        assert_eq!(referrer_host(None), "");
    }
}
