//! OCI Distribution v2 (`/v2/...`) and the token endpoint (`/v2/token`).
//!
//! Repository names are `{owner}/{package}[/...]`; the package is the
//! container package `{package}[/...]` of `owner`. Authentication follows
//! Docker's token flow: unauthenticated requests get `401` with
//! `WWW-Authenticate: Bearer realm="{base}/v2/token",service=..,scope=..`;
//! the client exchanges Basic `username:<PAT or GITHUB_TOKEN>` (passwords
//! are refused) for a short-lived JWT carrying the granted actions.
//! Personal access tokens are also accepted directly (Basic or Bearer).

mod blobs;
mod manifests;

use axum::Router;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bgh_core::auth::{AuthContext, AuthOptions};
use bgh_core::models::db;
use bgh_core::{ApiError, AppState};
use serde_json::json;

use crate::access::{self, Caps};
use crate::digest::valid_name;
use crate::model::{self, PackageRow};
use crate::token::{self, Access, Claims};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v2", get(base))
        .route("/v2/", get(base))
        .route("/v2/token", get(token_get).post(token_post))
        .route("/v2/{*rest}", any(dispatch))
}

pub const API_VERSION_HEADER: &str = "docker-distribution-api-version";

/// A distribution-spec error response.
#[derive(Debug)]
pub struct OciError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub detail: Option<serde_json::Value>,
    pub challenge: Option<String>,
    pub extra_headers: Vec<(&'static str, String)>,
}

impl OciError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            detail: None,
            challenge: None,
            extra_headers: Vec::new(),
        }
    }
    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.extra_headers.push((name, value.into()));
        self
    }
    pub fn blob_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "BLOB_UNKNOWN",
            "blob unknown to registry",
        )
    }
    pub fn manifest_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "MANIFEST_UNKNOWN",
            "manifest unknown",
        )
    }
    pub fn name_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NAME_UNKNOWN",
            "repository name not known to registry",
        )
    }
    pub fn digest_invalid(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "DIGEST_INVALID", msg)
    }
    pub fn upload_unknown() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "BLOB_UPLOAD_UNKNOWN",
            "blob upload unknown to registry",
        )
    }
    pub fn unsupported() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "UNSUPPORTED",
            "The operation is unsupported.",
        )
    }
    pub fn denied(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "DENIED", msg)
    }
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            "unknown error",
        )
    }
}

impl From<sqlx::Error> for OciError {
    fn from(err: sqlx::Error) -> Self {
        tracing::error!(?err, "registry database error");
        Self::internal()
    }
}

impl From<std::io::Error> for OciError {
    fn from(err: std::io::Error) -> Self {
        tracing::error!(?err, "registry storage error");
        Self::internal()
    }
}

impl From<ApiError> for OciError {
    fn from(err: ApiError) -> Self {
        let resp = err.into_response();
        match resp.status() {
            StatusCode::UNAUTHORIZED => Self::new(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "authentication required",
            ),
            StatusCode::FORBIDDEN => Self::denied("requested access to the resource is denied"),
            StatusCode::NOT_FOUND => Self::name_unknown(),
            _ => Self::internal(),
        }
    }
}

impl IntoResponse for OciError {
    fn into_response(self) -> Response {
        let mut err = json!({ "code": self.code, "message": self.message });
        if let Some(d) = self.detail {
            err["detail"] = d;
        }
        let mut resp = (
            self.status,
            [(header::CONTENT_TYPE, "application/json")],
            json!({ "errors": [err] }).to_string(),
        )
            .into_response();
        let h = resp.headers_mut();
        h.insert(API_VERSION_HEADER, HeaderValue::from_static("registry/2.0"));
        if let Some(c) = self.challenge
            && let Ok(v) = HeaderValue::from_str(&c)
        {
            h.insert(header::WWW_AUTHENTICATE, v);
        }
        for (k, v) in self.extra_headers {
            if let Ok(v) = HeaderValue::from_str(&v) {
                h.insert(k, v);
            }
        }
        resp
    }
}

pub type OciResult<T = Response> = Result<T, OciError>;

/// Response builder with the registry API version header.
pub fn builder(status: StatusCode) -> axum::http::response::Builder {
    Response::builder()
        .status(status)
        .header(API_VERSION_HEADER, "registry/2.0")
}

fn challenge(state: &AppState, scope: Option<&str>, insufficient: bool) -> String {
    let mut c = format!(
        "Bearer realm=\"{}/v2/token\",service=\"{}\"",
        state.config.base_url.trim_end_matches('/'),
        model::registry_host(state)
    );
    if let Some(s) = scope {
        c.push_str(&format!(",scope=\"{s}\""));
    }
    if insufficient {
        c.push_str(",error=\"insufficient_scope\"");
    }
    c
}

fn unauthorized(state: &AppState, scope: Option<&str>, insufficient: bool) -> OciError {
    let mut e = OciError::new(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "authentication required",
    );
    e.challenge = Some(challenge(state, scope, insufficient));
    e
}

/// The authenticated caller of a registry request.
pub struct Caller {
    /// Credentials sent directly (PAT, `GITHUB_TOKEN`).
    pub auth: Option<AuthContext>,
    /// A registry JWT from `/v2/token`.
    pub claims: Option<Claims>,
}

impl Caller {
    pub fn user_id(&self) -> Option<i64> {
        self.auth
            .as_ref()
            .map(|a| a.user.id)
            .or_else(|| self.claims.as_ref().and_then(|c| c.uid))
    }

    pub fn job_repo(&self) -> Option<i64> {
        match (&self.auth, &self.claims) {
            (Some(a), _) => bgh_core::perms::job_token_repo(a),
            (None, Some(c)) => c.jr,
            _ => None,
        }
    }

    fn is_anonymous(&self) -> bool {
        self.user_id().is_none()
    }
}

/// Resolve credentials. Invalid ones are a 401 with a challenge.
async fn resolve_caller(state: &AppState, headers: &HeaderMap) -> OciResult<Caller> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some((scheme, cred)) = v.split_once(' ')
        && scheme.eq_ignore_ascii_case("bearer")
        && token::is_jwt(cred.trim())
    {
        return match token::verify(state, cred.trim()).await {
            Some(claims) => Ok(Caller {
                auth: None,
                claims: Some(claims),
            }),
            None => Err(unauthorized(state, None, false)),
        };
    }
    match bgh_core::auth::authenticate(state, headers, AuthOptions::default()).await {
        Ok(auth) => Ok(Caller { auth, claims: None }),
        Err(_) => Err(unauthorized(state, None, false)),
    }
}

/// A repository (`{owner}/{package}`) addressed by a request.
pub struct Repo {
    /// Full name as requested (lowercase).
    pub full_name: String,
    pub owner: db::User,
    /// Package name (`{package}[/...]`).
    pub name: String,
    pub package: Option<PackageRow>,
}

/// Split and look up `{owner}/{package}`. `None` when the owner is unknown.
async fn load_repo(state: &AppState, full_name: &str) -> OciResult<Option<Repo>> {
    if !valid_name(full_name) {
        return Err(OciError::new(
            StatusCode::BAD_REQUEST,
            "NAME_INVALID",
            "invalid repository name",
        ));
    }
    let Some((owner, name)) = full_name.split_once('/') else {
        return Err(OciError::new(
            StatusCode::BAD_REQUEST,
            "NAME_INVALID",
            "repository name must be {owner}/{package}",
        ));
    };
    let Some(owner) = db::User::find_by_login(&state.db, owner).await? else {
        return Ok(None);
    };
    let package = model::find_package(&state.db, owner.id, "container", name).await?;
    Ok(Some(Repo {
        full_name: full_name.to_string(),
        owner,
        name: name.to_string(),
        package,
    }))
}

/// Capabilities of `caller` on `repo` (JWT claims, or computed).
async fn caps_for(state: &AppState, caller: &Caller, repo: &Repo) -> OciResult<Caps> {
    if let Some(c) = &caller.claims {
        return Ok(Caps {
            read: c.allows(&repo.full_name, "pull"),
            write: c.allows(&repo.full_name, "push"),
            admin: c.allows(&repo.full_name, "delete"),
        });
    }
    Ok(access::package_caps(
        state,
        caller.auth.as_ref(),
        &repo.owner,
        repo.package.as_ref(),
    )
    .await?)
}

/// Load `full_name` and require `action` (`pull` | `push` | `delete`).
/// Missing access is a 401 challenge for anonymous callers and tokens
/// lacking the scope, 403 for authenticated callers without permission
/// (404 when they can't even see the package).
pub async fn authorize(
    state: &AppState,
    caller: &Caller,
    full_name: &str,
    action: &'static str,
) -> OciResult<(Repo, Caps)> {
    let scope = format!("repository:{full_name}:{action}");
    let repo = load_repo(state, full_name).await?;
    let Some(repo) = repo else {
        if caller.is_anonymous() {
            return Err(unauthorized(state, Some(&scope), false));
        }
        return Err(OciError::name_unknown());
    };
    let caps = caps_for(state, caller, &repo).await?;
    if caps.allows(action) {
        return Ok((repo, caps));
    }
    if caller.is_anonymous() || caller.claims.is_some() {
        return Err(unauthorized(state, Some(&scope), caller.claims.is_some()));
    }
    if !caps.read && repo.package.is_some() {
        return Err(OciError::name_unknown());
    }
    Err(OciError::denied(
        "requested access to the resource is denied",
    ))
}

/// `GET /v2/`: 200 for authenticated callers, else a token challenge.
async fn base(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match resolve_caller(&state, &headers).await {
        Ok(c) if !c.is_anonymous() => builder(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("response"),
        Ok(_) => unauthorized(&state, None, false).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Every `/v2/{name}/...` endpoint.
async fn dispatch(
    State(state): State<AppState>,
    method: Method,
    axum::extract::Path(rest): axum::extract::Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let query = parse_query(query.as_deref());
    let result = async {
        let caller = resolve_caller(&state, &headers).await?;
        route(&state, &caller, &method, &rest, &query, &headers, body).await
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

async fn route(
    state: &AppState,
    caller: &Caller,
    method: &Method,
    rest: &str,
    query: &Query,
    headers: &HeaderMap,
    body: Body,
) -> OciResult {
    let rest = rest.trim_start_matches('/');
    if let Some(name) = rest.strip_suffix("/tags/list") {
        return match *method {
            Method::GET | Method::HEAD => manifests::tags_list(state, caller, name, query).await,
            _ => Err(OciError::unsupported()),
        };
    }
    if let Some((name, id)) = rest
        .rsplit_once("/blobs/uploads/")
        .or_else(|| rest.strip_suffix("/blobs/uploads").map(|n| (n, "")))
    {
        if id.is_empty() {
            return match *method {
                Method::POST => blobs::start_upload(state, caller, name, query, body).await,
                _ => Err(OciError::unsupported()),
            };
        }
        let Ok(id) = uuid::Uuid::parse_str(id) else {
            return Err(OciError::upload_unknown());
        };
        return match *method {
            Method::PATCH => blobs::patch_upload(state, caller, name, id, headers, body).await,
            Method::PUT => {
                blobs::finish_upload(state, caller, name, id, query, headers, body).await
            }
            Method::GET => blobs::upload_status(state, caller, name, id).await,
            Method::DELETE => blobs::cancel_upload(state, caller, name, id).await,
            _ => Err(OciError::unsupported()),
        };
    }
    if let Some((name, digest)) = rest.rsplit_once("/blobs/") {
        return match *method {
            Method::GET => blobs::get_blob(state, caller, name, digest, headers, false).await,
            Method::HEAD => blobs::get_blob(state, caller, name, digest, headers, true).await,
            Method::DELETE => blobs::delete_blob(state, caller, name, digest).await,
            _ => Err(OciError::unsupported()),
        };
    }
    if let Some((name, reference)) = rest.rsplit_once("/manifests/") {
        return match *method {
            Method::GET => manifests::get(state, caller, name, reference, false).await,
            Method::HEAD => manifests::get(state, caller, name, reference, true).await,
            Method::PUT => manifests::put(state, caller, name, reference, headers, body).await,
            Method::DELETE => manifests::delete(state, caller, name, reference).await,
            _ => Err(OciError::unsupported()),
        };
    }
    if let Some((name, digest)) = rest.rsplit_once("/referrers/") {
        return match *method {
            Method::GET | Method::HEAD => {
                manifests::referrers(state, caller, name, digest, query).await
            }
            _ => Err(OciError::unsupported()),
        };
    }
    Err(OciError::new(
        StatusCode::NOT_FOUND,
        "NAME_UNKNOWN",
        "unknown registry endpoint",
    ))
}

/// Decoded query parameters (repeated keys kept in order).
pub struct Query(Vec<(String, String)>);

impl Query {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    pub fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn parse_query(q: Option<&str>) -> Query {
    Query(
        url::form_urlencoded::parse(q.unwrap_or("").as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

async fn token_get(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let query = parse_query(query.as_deref());
    let auth = match bgh_core::auth::authenticate(&state, &headers, AuthOptions::default()).await {
        Ok(a) => a,
        Err(_) => return bad_credentials(&state),
    };
    issue(
        &state,
        auth,
        query.all("scope").map(str::to_string).collect(),
    )
    .await
}

/// OAuth2-style `POST /v2/token` (`grant_type=password`), used by
/// containerd-based clients.
async fn token_post(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    let form = parse_query(Some(&body));
    let mut auth_headers = headers.clone();
    if form.get("grant_type") == Some("password") {
        use base64::Engine;
        let creds = format!(
            "{}:{}",
            form.get("username").unwrap_or(""),
            form.get("password").unwrap_or("")
        );
        let v = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(creds)
        );
        if let Ok(v) = HeaderValue::from_str(&v) {
            auth_headers.insert(header::AUTHORIZATION, v);
        }
    } else if form.get("grant_type").is_some() {
        return OciError::new(
            StatusCode::BAD_REQUEST,
            "UNSUPPORTED",
            "unsupported grant_type",
        )
        .into_response();
    }
    let auth =
        match bgh_core::auth::authenticate(&state, &auth_headers, AuthOptions::default()).await {
            Ok(a) => a,
            Err(_) => return bad_credentials(&state),
        };
    let scopes = form
        .all("scope")
        .flat_map(|s| s.split(' '))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    issue(&state, auth, scopes).await
}

fn bad_credentials(state: &AppState) -> Response {
    let mut e = OciError::new(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "authentication required: use a personal access token or GITHUB_TOKEN as the password",
    );
    e.challenge = Some(challenge(state, None, false));
    e.into_response()
}

/// Mint a JWT granting the subset of `scopes` the caller is allowed.
async fn issue(state: &AppState, auth: Option<AuthContext>, scopes: Vec<String>) -> Response {
    let mut access: Vec<Access> = Vec::new();
    for scope in &scopes {
        let Some((kind, rest)) = scope.split_once(':') else {
            continue;
        };
        let Some((name, actions)) = rest.rsplit_once(':') else {
            continue;
        };
        if kind != "repository" {
            continue;
        }
        let wanted: Vec<&str> = actions.split(',').filter(|a| !a.is_empty()).collect();
        let Ok(Some(repo)) = load_repo(state, name).await else {
            access.push(Access {
                kind: kind.into(),
                name: name.into(),
                actions: vec![],
            });
            continue;
        };
        let caps =
            match access::package_caps(state, auth.as_ref(), &repo.owner, repo.package.as_ref())
                .await
            {
                Ok(c) => c,
                Err(_) => Caps::default(),
            };
        let mut granted: Vec<String> = Vec::new();
        for a in wanted {
            if a == "*" {
                for x in ["pull", "push", "delete"] {
                    if caps.allows(x) && !granted.iter().any(|g| g == x) {
                        granted.push(x.into());
                    }
                }
            } else if caps.allows(a) && !granted.iter().any(|g| g == a) {
                granted.push(a.into());
            }
        }
        match access.iter_mut().find(|x| x.name == name) {
            Some(existing) => {
                for g in granted {
                    if !existing.actions.contains(&g) {
                        existing.actions.push(g);
                    }
                }
            }
            None => access.push(Access {
                kind: kind.into(),
                name: name.into(),
                actions: granted,
            }),
        }
    }
    let now = chrono::Utc::now();
    let claims = Claims {
        iss: state.config.base_url.trim_end_matches('/').to_string(),
        sub: auth
            .as_ref()
            .map(|a| a.user.login.clone())
            .unwrap_or_default(),
        aud: model::registry_host(state),
        exp: now.timestamp() + token::TOKEN_TTL_SECS,
        nbf: now.timestamp() - 10,
        iat: now.timestamp(),
        jti: uuid::Uuid::new_v4().to_string(),
        access,
        uid: auth.as_ref().map(|a| a.user.id),
        jr: auth.as_ref().and_then(bgh_core::perms::job_token_repo),
    };
    let jwt = match token::sign(state, &claims).await {
        Ok(t) => t,
        Err(err) => {
            tracing::error!(?err, "signing registry token");
            return OciError::internal().into_response();
        }
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "token": jwt,
            "access_token": jwt,
            "expires_in": token::TOKEN_TTL_SECS,
            "issued_at": bgh_core::time::Timestamp(now),
        })
        .to_string(),
    )
        .into_response()
}
