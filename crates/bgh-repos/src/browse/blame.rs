//! `GET /_bgh/repos/{owner}/{repo}/blame/{ref}/{path}`
//!
//! JSON (`{commit, path, ranges, commits}`) by default. With
//! `Accept: application/x-ndjson` the response streams one JSON object per
//! line as `git blame --incremental` attributes ranges (`{"range": ...,
//! "commit": ...}`, commit metadata on first use), then a final
//! `{"done": true}`. Complete results are cached in Redis by commit + path.

use std::collections::BTreeMap;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use bgh_core::prelude::*;
use bgh_git::PathLookup;
use bgh_git::blame::{Blame, BlameChunk, BlameCommit, BlamePrevious, BlameRange};
use futures::StreamExt;
use serde::Serialize;

use super::{
    CACHE_VERSION, Person, Target, accounts_by_email, cache_control, cache_get, cache_put, etag_of,
    json_response, precheck, resolve,
};

#[derive(Debug, Serialize)]
pub struct BlameCommitView {
    pub sha: String,
    pub summary: String,
    pub author: Person,
    pub committer: Person,
    pub previous: Option<BlamePrevious>,
    pub boundary: bool,
}

#[derive(Debug, Serialize)]
pub struct BlameView {
    pub commit: String,
    pub path: String,
    pub ranges: Vec<BlameRange>,
    pub commits: BTreeMap<String, BlameCommitView>,
}

fn cache_key(t: &Target) -> String {
    format!(
        "blame:{CACHE_VERSION}:{}:{}:{}",
        t.access.repo.id, t.commit, t.path
    )
}

async fn views(
    state: &AppState,
    commits: &[&BlameCommit],
) -> ApiResult<BTreeMap<String, BlameCommitView>> {
    let accounts = accounts_by_email(
        state,
        commits
            .iter()
            .flat_map(|c| [c.author_email.clone(), c.committer_email.clone()]),
    )
    .await?;
    let person = |name: &str, email: &str, time: i64| {
        let acct = accounts.get(&email.to_lowercase());
        Person {
            name: name.to_string(),
            email: email.to_string(),
            date: chrono::DateTime::from_timestamp(time, 0)
                .unwrap_or_default()
                .into(),
            login: acct.map(|a| a.0.clone()),
            avatar_url: acct.map(|a| a.1.clone()),
        }
    };
    Ok(commits
        .iter()
        .map(|c| {
            (
                c.sha.clone(),
                BlameCommitView {
                    sha: c.sha.clone(),
                    summary: c.summary.clone(),
                    author: person(&c.author_name, &c.author_email, c.author_time),
                    committer: person(&c.committer_name, &c.committer_email, c.committer_time),
                    previous: c.previous.clone(),
                    boundary: c.boundary,
                },
            )
        })
        .collect())
}

fn wants_stream(req: &HeaderMap) -> bool {
    req.get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/x-ndjson"))
}

pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let t = resolve(&state, auth.as_ref(), &owner, &repo, Some(&spec)).await?;
    let stream = wants_stream(&req);
    let key = format!("blame:{}:{}:{}", t.commit, t.path, stream);
    if let Some(r) = precheck(&req, &t, &key) {
        return Ok(r);
    }
    // Must be a file.
    let (commit, path) = (t.commit.clone(), t.path.clone());
    t.store(&state)
        .read(t.access.repo.id, move |r| {
            match r.lookup_path(&commit, &path)? {
                PathLookup::Entry(e) if e.kind.object_type() == "blob" => Ok(()),
                _ => Err(bgh_git::GitError::NotFound(path)),
            }
        })
        .await?;

    let cached: Option<Blame> = cache_get(&state, &cache_key(&t)).await;
    if stream {
        return stream_response(state, t, key, cached).await;
    }
    let blame = match cached {
        Some(b) => b,
        None => {
            let b = bgh_git::blame::blame(&t.store(&state), t.access.repo.id, &t.commit, &t.path)
                .await?;
            cache_put(&state, &cache_key(&t), &b).await;
            b
        }
    };
    let commits: Vec<&BlameCommit> = blame.commits.values().collect();
    let body = BlameView {
        commit: t.commit.clone(),
        path: t.path.clone(),
        commits: views(&state, &commits).await?,
        ranges: blame.ranges,
    };
    json_response(&req, &t, &key, &body)
}

fn line(v: &impl Serialize) -> bytes::Bytes {
    let mut b = serde_json::to_vec(v).unwrap_or_default();
    b.push(b'\n');
    b.into()
}

async fn chunk_line(state: &AppState, chunk: &BlameChunk) -> bytes::Bytes {
    #[derive(Serialize)]
    struct Out<'a> {
        range: &'a BlameRange,
        #[serde(skip_serializing_if = "Option::is_none")]
        commit: Option<BlameCommitView>,
    }
    let commit = match &chunk.commit {
        Some(c) => views(state, &[c])
            .await
            .ok()
            .and_then(|mut m| m.remove(&c.sha)),
        None => None,
    };
    line(&Out {
        range: &chunk.range,
        commit,
    })
}

async fn stream_response(
    state: AppState,
    t: Target,
    key: String,
    cached: Option<Blame>,
) -> ApiResult<Response> {
    let done = line(&serde_json::json!({"done": true}));
    let body = match cached {
        Some(blame) => {
            // Replay in line order, commit metadata on first use.
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for r in &blame.ranges {
                let commit = if seen.insert(r.sha.clone()) {
                    blame.commits.get(&r.sha).cloned()
                } else {
                    None
                };
                let chunk = BlameChunk {
                    range: r.clone(),
                    commit,
                };
                out.extend_from_slice(&chunk_line(&state, &chunk).await);
            }
            out.extend_from_slice(&done);
            Body::from(out)
        }
        None => {
            let live = bgh_git::blame::blame_stream(
                &t.store(&state),
                t.access.repo.id,
                &t.commit,
                &t.path,
            )
            .await?;
            let cache_key = cache_key(&t);
            struct St<S> {
                live: std::pin::Pin<Box<S>>,
                acc: Option<Blame>,
                state: AppState,
                key: String,
                finished: bool,
            }
            let st = St {
                live: Box::pin(live),
                acc: Some(Blame::default()),
                state: state.clone(),
                key: cache_key,
                finished: false,
            };
            let s = futures::stream::unfold(st, move |mut st| {
                let done = done.clone();
                async move {
                    if st.finished {
                        return None;
                    }
                    match st.live.next().await {
                        Some(Ok(chunk)) => {
                            if let Some(acc) = st.acc.as_mut() {
                                if let Some(c) = &chunk.commit {
                                    acc.commits.insert(c.sha.clone(), c.clone());
                                }
                                acc.ranges.push(chunk.range.clone());
                            }
                            let l = chunk_line(&st.state, &chunk).await;
                            Some((Ok::<_, std::io::Error>(l), st))
                        }
                        Some(Err(err)) => {
                            st.finished = true;
                            tracing::warn!(?err, "blame stream failed");
                            Some((Err(std::io::Error::other(err.to_string())), st))
                        }
                        None => {
                            st.finished = true;
                            if let Some(mut acc) = st.acc.take() {
                                acc.ranges.sort_by_key(|r| r.line);
                                cache_put(&st.state, &st.key, &acc).await;
                            }
                            Some((Ok(done), st))
                        }
                    }
                }
            });
            Body::from_stream(s)
        }
    };
    let mut resp = Response::new(body);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    h.insert(
        header::CACHE_CONTROL,
        cache_control(t.access.repo.is_private(), t.immutable()),
    );
    if t.immutable() {
        h.insert(
            header::ETAG,
            HeaderValue::from_str(&etag_of(&[CACHE_VERSION, &key])).expect("etag"),
        );
    }
    h.insert(
        header::VARY,
        HeaderValue::from_static("Accept, Cookie, Authorization"),
    );
    Ok(resp)
}
