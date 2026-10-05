//! An in-test fake GitHub REST API (axum) serving the fixtures in
//! `fixtures/github/` with GitHub's mechanics: token auth, `Link`
//! pagination (issues come in two pages, the second through
//! `/repositories/{id}/…` like real next links), `ETag` / `304`, the asset
//! download redirect to a storage host, rate-limit answers, and a webhook
//! receiver at `/hook`. Knobs flip failures on and off.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

pub const SOURCE: &str = "octo-org/hello-world";
pub const ASSET_BODY: &str = "Hello world\n";

#[derive(Default)]
pub struct Knobs {
    /// `/issues/comments` answers 410 (a non-retried failure).
    pub fail_comments: AtomicBool,
    /// The next `/labels` request is rate limited (primary limit, reset in 1 s).
    pub rate_limit_labels: AtomicBool,
    /// The next `/milestones` request hits a secondary limit (`Retry-After: 1`).
    pub secondary_limit_milestones: AtomicBool,
}

pub struct FakeState {
    pub base: String,
    pub token: String,
    pub clone_url: Mutex<String>,
    pub knobs: Knobs,
    pub requests: Mutex<Vec<String>>,
    pub not_modified: AtomicUsize,
    /// The storage host received an `Authorization` header (must not).
    pub leaked_auth: AtomicBool,
    /// `X-GitHub-Event` of every webhook delivery received at `/hook`.
    pub hooks: Mutex<Vec<String>>,
}

pub struct Fake {
    pub base: String,
    pub state: Arc<FakeState>,
}

impl Fake {
    pub async fn start(token: &str) -> Fake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(FakeState {
            base: base.clone(),
            token: token.to_string(),
            clone_url: Mutex::new(String::new()),
            knobs: Knobs::default(),
            requests: Mutex::new(Vec::new()),
            not_modified: AtomicUsize::new(0),
            leaked_auth: AtomicBool::new(false),
            hooks: Mutex::new(Vec::new()),
        });
        let app = Router::new().fallback(handle).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Fake { base, state }
    }

    pub fn set_clone_url(&self, url: &str) {
        *self.state.clone_url.lock().unwrap() = url.to_string();
    }

    pub fn hits(&self, needle: &str) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains(needle))
            .count()
    }

    pub fn hook_events(&self) -> Vec<String> {
        self.state.hooks.lock().unwrap().clone()
    }
}

fn fixture(state: &FakeState, name: &str) -> Value {
    let path = format!(
        "{}/tests/it/fixtures/github/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let raw = raw
        .replace("https://api.github.com", &state.base)
        .replace("https://github.com", &format!("{}/web", state.base))
        .replace("{{CLONE_URL}}", &state.clone_url.lock().unwrap());
    serde_json::from_str(&raw).unwrap()
}

fn message(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({"message": msg}))).into_response()
}

/// JSON with an `ETag`; `304` when `If-None-Match` matches.
fn json(state: &FakeState, headers: &HeaderMap, body: &Value, link: Option<String>) -> Response {
    let bytes = serde_json::to_vec(body).unwrap();
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let etag = format!("\"{:x}\"", h.finish());
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        state.not_modified.fetch_add(1, Ordering::SeqCst);
        return (StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response();
    }
    let mut res = (
        StatusCode::OK,
        [
            ("content-type", "application/json".to_string()),
            ("etag", etag),
        ],
        bytes,
    )
        .into_response();
    if let Some(link) = link {
        res.headers_mut().insert("link", link.parse().unwrap());
    }
    res
}

async fn handle(
    State(state): State<Arc<FakeState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    state
        .requests
        .lock()
        .unwrap()
        .push(format!("{path}?{query}"));

    if path == "/hook" && method == Method::POST {
        let event = headers
            .get("x-github-event")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
            .to_string();
        let _ = body;
        state.hooks.lock().unwrap().push(event);
        return StatusCode::OK.into_response();
    }
    if let Some(name) = path.strip_prefix("/download/") {
        if headers.contains_key("authorization") {
            state.leaked_auth.store(true, Ordering::SeqCst);
        }
        return match name {
            "hello.txt" => (
                StatusCode::OK,
                [("content-type", "application/octet-stream")],
                ASSET_BODY,
            )
                .into_response(),
            _ => message(StatusCode::NOT_FOUND, "Not Found"),
        };
    }
    let expected = format!("Bearer {}", state.token);
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return message(StatusCode::UNAUTHORIZED, "Bad credentials");
    }

    let repo = format!("/repos/{SOURCE}");
    let rest = path.strip_prefix(&repo).map(str::to_string).or_else(|| {
        path.strip_prefix("/repositories/1296269")
            .map(str::to_string)
    });
    let page2 = query.split('&').any(|p| p == "page=2");
    match rest.as_deref() {
        Some("") => json(&state, &headers, &fixture(&state, "repo.json"), None),
        Some("/labels") => {
            if state.knobs.rate_limit_labels.swap(false, Ordering::SeqCst) {
                let reset = chrono::Utc::now().timestamp() + 1;
                return (
                    StatusCode::FORBIDDEN,
                    [
                        ("x-ratelimit-remaining", "0".to_string()),
                        ("x-ratelimit-reset", reset.to_string()),
                    ],
                    axum::Json(serde_json::json!({"message": "API rate limit exceeded"})),
                )
                    .into_response();
            }
            json(&state, &headers, &fixture(&state, "labels.json"), None)
        }
        Some("/milestones") => {
            if state
                .knobs
                .secondary_limit_milestones
                .swap(false, Ordering::SeqCst)
            {
                return (
                    StatusCode::FORBIDDEN,
                    [("retry-after", "1")],
                    axum::Json(
                        serde_json::json!({"message": "You have exceeded a secondary rate limit"}),
                    ),
                )
                    .into_response();
            }
            json(&state, &headers, &fixture(&state, "milestones.json"), None)
        }
        Some("/issues") => {
            let all = fixture(&state, "issues.json");
            let items = all.as_array().unwrap();
            if page2 {
                json(&state, &headers, &Value::Array(items[3..].to_vec()), None)
            } else {
                let link = format!(
                    "<{}/repositories/1296269/issues?state=all&sort=created&direction=asc&per_page=100&page=2>; rel=\"next\", <{}/repositories/1296269/issues?state=all&sort=created&direction=asc&per_page=100&page=2>; rel=\"last\"",
                    state.base, state.base
                );
                json(
                    &state,
                    &headers,
                    &Value::Array(items[..3].to_vec()),
                    Some(link),
                )
            }
        }
        Some("/issues/comments") => {
            if state.knobs.fail_comments.load(Ordering::SeqCst) {
                return message(StatusCode::GONE, "Comments are temporarily gone");
            }
            json(
                &state,
                &headers,
                &fixture(&state, "issue_comments.json"),
                None,
            )
        }
        Some("/issues/events") => json(
            &state,
            &headers,
            &fixture(&state, "issue_events.json"),
            None,
        ),
        Some("/issues/1/reactions") => json(
            &state,
            &headers,
            &fixture(&state, "reactions_issue_1.json"),
            None,
        ),
        Some("/issues/comments/1000000001/reactions") => json(
            &state,
            &headers,
            &fixture(&state, "reactions_comment_1000000001.json"),
            None,
        ),
        Some("/releases") => json(&state, &headers, &fixture(&state, "releases.json"), None),
        Some("/releases/assets/10") => {
            let accept = headers
                .get("accept")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if accept.contains("octet-stream") {
                (
                    StatusCode::FOUND,
                    // Another origin, like GitHub's storage host.
                    [(
                        "location",
                        format!(
                            "{}/download/hello.txt",
                            state.base.replace("127.0.0.1", "localhost")
                        ),
                    )],
                )
                    .into_response()
            } else {
                message(StatusCode::NOT_FOUND, "Not Found")
            }
        }
        Some("/teams") => json(&state, &headers, &fixture(&state, "teams.json"), None),
        _ => match path.as_str() {
            "/users/octocat" | "/users/hubot" | "/users/monalisa" => {
                let login = path.trim_start_matches("/users/");
                json(
                    &state,
                    &headers,
                    &fixture(&state, &format!("users/{login}.json")),
                    None,
                )
            }
            "/orgs/octo-org/teams/core/members" => json(
                &state,
                &headers,
                &fixture(&state, "team_members_core.json"),
                None,
            ),
            _ => message(StatusCode::NOT_FOUND, "Not Found"),
        },
    }
}
