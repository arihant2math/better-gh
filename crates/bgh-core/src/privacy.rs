//! Private mode (`privacy.private_mode`): every anonymous request is
//! refused except the sign-in flows, static assets, `/healthz` and
//! `/api/v3/meta`.
//!
//! * API (`/api/...`) and private endpoints (`/_bgh/...`): 401
//!   "Requires authentication" (GitHub JSON).
//! * Git smart HTTP and LFS: 401 with `WWW-Authenticate: Basic` when no
//!   credentials were sent, so git prompts. Requests carrying credentials
//!   reach the git handlers, which authenticate them (passwords included)
//!   and — like every repository read — go through `RepoAccess`, which
//!   refuses anonymous callers in private mode as a second line.
//! * Web pages (`GET` accepting `text/html`): 302 to
//!   `/login?return_to=…`; the public sign-in pages still render.
//! * Everything else (raw files, archives, avatars, release downloads,
//!   the sync WebSocket): 401. Raw/archive URLs with a short-lived
//!   `?token=` reach the handler, which validates the token.
//! * The container registry (`/v2/...`) answers for itself: its Bearer
//!   challenge and token endpoint work as usual, and it refuses anonymous
//!   callers (and anonymous tokens) in private mode.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::settings;
use crate::state::AppState;

/// `WWW-Authenticate` challenge sent to git clients.
pub const GIT_REALM: &str = "Basic realm=\"Better GitHub\"";

/// Endpoints that work signed out in private mode (sign-in, sign-up,
/// password reset, SSO, OAuth token exchange, device flow, site info).
const EXEMPT: &[&str] = &[
    "/healthz",
    "/api/v3/meta",
    "/_bgh/site",
    "/_bgh/boot",
    "/_bgh/session",
    "/_bgh/session/two_factor",
    "/_bgh/auth/login",
    "/_bgh/auth/2fa",
    "/_bgh/auth/signup",
    "/_bgh/auth/logout",
    "/_bgh/signup",
    "/_bgh/password_reset",
    "/_bgh/emails/verify",
    "/_bgh/sso",
    "/login/oauth/access_token",
    "/login/device/code",
];

/// Exempt path prefixes (each also matches without the trailing slash via
/// [`EXEMPT`]).
const EXEMPT_PREFIXES: &[&str] = &[
    "/_bgh/password_reset/",
    "/_bgh/sso/",
    "/assets/",
    // SAML sign-in and SP endpoints (metadata, ACS, single logout).
    "/_bgh/saml/login",
    "/saml/",
];

/// Web client pages that render signed out (see `web/src/app/App.tsx`).
const PUBLIC_PAGES: &[&str] = &[
    "/login",
    "/signup",
    "/login/two-factor",
    "/password_reset",
    "/settings/emails/verify",
];

/// Whether `path` stays reachable anonymously in private mode.
pub fn is_exempt(path: &str) -> bool {
    let trimmed = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    if EXEMPT.contains(&trimmed) || PUBLIC_PAGES.contains(&trimmed) {
        return true;
    }
    if EXEMPT_PREFIXES.iter().any(|p| path.starts_with(p)) || path.starts_with("/password_reset/") {
        return true;
    }
    // Top-level static files of the web client (`/favicon.svg`, `/sw.js`,
    // `/manifest.webmanifest`); logins can't contain dots.
    let rel = &path[1.min(path.len())..];
    !rel.is_empty() && !rel.contains('/') && rel.contains('.')
}

/// Git smart-HTTP and LFS endpoints.
pub fn is_git_path(path: &str) -> bool {
    path.ends_with("/info/refs")
        || path.ends_with("/git-upload-pack")
        || path.ends_with("/git-receive-pack")
        || path.contains("/info/lfs/")
}

/// Container registry (OCI distribution, `/v2/...`).
pub fn is_registry_path(path: &str) -> bool {
    path == "/v2" || path.starts_with("/v2/")
}

/// Raw file and archive downloads, which accept a `?token=` instead of
/// credentials.
fn is_download_path(path: &str) -> bool {
    let mut segs = path.trim_start_matches('/').splitn(4, '/');
    let (_, _, kind) = (segs.next(), segs.next(), segs.next());
    matches!(
        kind,
        Some("raw" | "archive" | "legacy.tar.gz" | "legacy.zip")
    )
}

fn has_download_token(req: &Request) -> bool {
    req.uri().query().is_some_and(|q| {
        q.split('&')
            .any(|kv| kv.strip_prefix("token=").is_some_and(|v| !v.is_empty()))
    })
}

fn wants_html(req: &Request) -> bool {
    matches!(*req.method(), Method::GET | Method::HEAD)
        && req
            .headers()
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/html"))
}

/// The response for an anonymous request refused by private mode.
fn refuse(req: &Request) -> Response {
    let path = req.uri().path();
    if is_git_path(path) {
        return ApiError::Unauthorized {
            message: "Authentication required.".into(),
            www_authenticate: Some(GIT_REALM.into()),
        }
        .into_response();
    }
    if !path.starts_with("/api/") && !path.starts_with("/_bgh/") && wants_html(req) {
        let target = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let location = if target == "/" {
            "/login".to_string()
        } else {
            format!("/login?return_to={}", crate::urls::encode_segment(target))
        };
        let mut resp = StatusCode::FOUND.into_response();
        if let Ok(v) = HeaderValue::from_str(&location) {
            resp.headers_mut().insert(header::LOCATION, v);
        }
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, private"),
        );
        return resp;
    }
    ApiError::requires_auth().into_response()
}

/// Whether `settings` puts the site in private mode (cached lookup).
pub async fn private_mode(state: &AppState) -> crate::error::ApiResult<bool> {
    Ok(settings::load(state).await?.privacy.private_mode)
}

/// 401 for anonymous callers of the user/organization directory
/// (`GET /users`, `GET /organizations`) unless the site allows it
/// (`privacy.allow_anonymous_directory`, never in private mode).
pub async fn require_directory_access(
    state: &AppState,
    auth: Option<&crate::auth::AuthContext>,
) -> crate::error::ApiResult<()> {
    if auth.is_some() || settings::load(state).await?.privacy.anonymous_directory() {
        Ok(())
    } else {
        Err(ApiError::requires_auth())
    }
}

/// Middleware enforcing private mode (see the module docs). Cheap when the
/// mode is off: one cached settings lookup.
pub async fn private_mode_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.method() == Method::OPTIONS || is_exempt(req.uri().path()) {
        return next.run(req).await;
    }
    match private_mode(&state).await {
        Ok(true) => {}
        Ok(false) => return next.run(req).await,
        Err(err) => return err.into_response(),
    }
    let path = req.uri().path();
    // The container registry runs Docker's token flow itself (Bearer
    // challenge, its own JWTs) and refuses anonymous callers in private
    // mode.
    if is_registry_path(path) {
        return next.run(req).await;
    }
    if is_git_path(path) {
        // Git handlers accept passwords, which the API resolver doesn't:
        // let them authenticate whatever was sent.
        if req.headers().contains_key(header::AUTHORIZATION) {
            return next.run(req).await;
        }
        return refuse(&req);
    }
    if is_download_path(path) && has_download_token(&req) {
        return next.run(req).await;
    }
    match crate::auth::resolve_request(&state, &mut req).await {
        Ok(Some(_)) => next.run(req).await,
        Ok(None) => refuse(&req),
        Err(err) => err.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_paths() {
        for p in [
            "/healthz",
            "/api/v3/meta",
            "/_bgh/auth/login",
            "/_bgh/password_reset/abc",
            "/_bgh/sso/1/callback",
            "/login",
            "/login/",
            "/signup",
            "/password_reset/tok",
            "/assets/index-abc.js",
            "/favicon.svg",
            "/login/oauth/access_token",
        ] {
            assert!(is_exempt(p), "{p} should be exempt");
        }
        for p in [
            "/",
            "/api/v3/repos/o/r",
            "/api/v3/users",
            "/api/graphql",
            "/_bgh/sync/ws",
            "/_bgh/sync/bootstrap",
            "/avatars/u/1",
            "/o/r",
            "/o/r.git/info/refs",
            "/o/r/raw/main/README.md",
            "/login/oauth/authorize",
        ] {
            assert!(!is_exempt(p), "{p} should not be exempt");
        }
        assert!(is_git_path("/o/r.git/info/refs"));
        assert!(is_git_path("/o/r/git-upload-pack"));
        assert!(is_git_path("/o/r.git/info/lfs/objects/batch"));
        assert!(!is_git_path("/o/r/raw/main/info/refs.md"));
        assert!(is_download_path("/o/r/raw/main/a.txt"));
        assert!(is_download_path("/o/r/legacy.zip/main"));
        assert!(!is_download_path("/o/r/issues/1"));
        assert!(is_registry_path("/v2/"));
        assert!(is_registry_path("/v2/acme/app/manifests/latest"));
        assert!(!is_registry_path("/v2acme"));
    }
}
