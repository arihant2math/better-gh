//! A generic in-test fake REST API (axum) for the P51 fixtures: a route
//! table `path → fixture file`, token auth (GitHub `Authorization: Bearer`
//! or GitLab `PRIVATE-TOKEN`), `{{VAR}}` placeholders filled in by the
//! test (real commit SHAs, the clone URL), optional `Link` pagination and
//! `ETag` / `304`. Unknown paths answer 404 like the real APIs.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

pub struct Route {
    pub file: String,
    /// Serve the array in pages of this size (`?page=N`, `Link: rel="next"`).
    pub per_page: Option<usize>,
}

pub struct ApiState {
    pub base: String,
    dir: String,
    auth_header: &'static str,
    auth_value: String,
    /// Source URL prefixes in the fixtures, rewritten to `base`.
    rewrites: Vec<String>,
    routes: Mutex<HashMap<String, Route>>,
    vars: Mutex<HashMap<String, String>>,
    pub requests: Mutex<Vec<String>>,
}

pub struct FakeApi {
    pub base: String,
    pub state: Arc<ApiState>,
}

impl FakeApi {
    /// `dir` under `tests/it/fixtures/`; `auth_header` is `authorization`
    /// (value `Bearer {token}`) or `private-token` (value `{token}`).
    pub async fn start(
        dir: &str,
        auth_header: &'static str,
        auth_value: String,
        rewrites: &[&str],
    ) -> FakeApi {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(ApiState {
            base: base.clone(),
            dir: format!("{}/tests/it/fixtures/{dir}", env!("CARGO_MANIFEST_DIR")),
            auth_header,
            auth_value,
            rewrites: rewrites.iter().map(|s| s.to_string()).collect(),
            routes: Mutex::new(HashMap::new()),
            vars: Mutex::new(HashMap::new()),
            requests: Mutex::new(Vec::new()),
        });
        let app = Router::new().fallback(handle).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeApi { base, state }
    }

    pub fn route(&self, path: &str, file: &str) -> &Self {
        self.state.routes.lock().unwrap().insert(
            path.to_string(),
            Route {
                file: file.to_string(),
                per_page: None,
            },
        );
        self
    }

    pub fn paged_route(&self, path: &str, file: &str, per_page: usize) -> &Self {
        self.state.routes.lock().unwrap().insert(
            path.to_string(),
            Route {
                file: file.to_string(),
                per_page: Some(per_page),
            },
        );
        self
    }

    /// Answer 404 for `path` from now on.
    pub fn unroute(&self, path: &str) -> &Self {
        self.state.routes.lock().unwrap().remove(path);
        self
    }

    pub fn state_var(&self, key: &str) -> Option<String> {
        self.state.vars.lock().unwrap().get(key).cloned()
    }

    pub fn set(&self, key: &str, value: &str) {
        self.state
            .vars
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
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
}

fn load(state: &ApiState, file: &str) -> Value {
    let path = format!("{}/{file}", state.dir);
    let mut raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    for prefix in &state.rewrites {
        raw = raw.replace(prefix.as_str(), &state.base);
    }
    for (k, v) in state.vars.lock().unwrap().iter() {
        raw = raw.replace(&format!("{{{{{k}}}}}"), v);
    }
    assert!(!raw.contains("{{"), "{path}: unfilled placeholder");
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn message(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({"message": msg}))).into_response()
}

fn json(headers: &HeaderMap, body: &Value, link: Option<String>) -> Response {
    let bytes = serde_json::to_vec(body).unwrap();
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    let etag = format!("\"{:x}\"", h.finish());
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
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

async fn handle(State(state): State<Arc<ApiState>>, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().to_string();
    let query = uri.query().unwrap_or("").to_string();
    state
        .requests
        .lock()
        .unwrap()
        .push(format!("{path}?{query}"));
    let got = headers.get(state.auth_header).and_then(|v| v.to_str().ok());
    if got != Some(state.auth_value.as_str()) {
        return message(StatusCode::UNAUTHORIZED, "401 Unauthorized");
    }
    let (file, per_page) = {
        let routes = state.routes.lock().unwrap();
        match routes.get(&path) {
            Some(r) => (r.file.clone(), r.per_page),
            None => return message(StatusCode::NOT_FOUND, "404 Not Found"),
        }
    };
    let body = load(&state, &file);
    let Some(per_page) = per_page else {
        return json(&headers, &body, None);
    };
    let items = body.as_array().cloned().unwrap_or_default();
    let page: usize = query
        .split('&')
        .find_map(|p| p.strip_prefix("page="))
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let start = (page - 1) * per_page;
    let slice: Vec<Value> = items.iter().skip(start).take(per_page).cloned().collect();
    let link = (start + per_page < items.len()).then(|| {
        format!(
            "<{}{path}?per_page={per_page}&page={}>; rel=\"next\"",
            state.base,
            page + 1
        )
    });
    json(&headers, &Value::Array(slice), link)
}
