//! Source archives (`/archive/{ref}.tar.gz|.zip`, legacy names, REST
//! `tarball`/`zipball` redirects).

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::archive::ArchiveFormat;

use super::TokenQuery;
use crate::browse::{cache_control, etag_of, not_modified};

/// Where cached archives live.
pub fn cache_dir(state: &AppState) -> std::path::PathBuf {
    state.config.data_dir.join("cache").join("archives")
}

/// GitHub names archive directories `{repo}-{ref}` with `/` → `-`, and
/// drops the `v` of version tags (`v1.2` → `repo-1.2`).
pub fn archive_name(repo: &str, refname: &str, is_tag: bool) -> String {
    let r = refname
        .strip_prefix("refs/heads/")
        .or_else(|| refname.strip_prefix("refs/tags/"))
        .unwrap_or(refname);
    let r = match r.strip_prefix('v') {
        Some(rest) if is_tag && rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => r,
    };
    format!("{repo}-{}", r.replace('/', "-"))
}

struct Resolved {
    access: RepoAccess,
    commit: String,
    refname: String,
    is_tag: bool,
}

async fn resolve_ref(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    spec: &str,
    token: Option<&str>,
) -> ApiResult<Resolved> {
    let access = super::access(state, auth, owner, repo, token).await?;
    let spec = spec.trim_matches('/').to_string();
    let (refname, commit, path, is_tag) = crate::store(state)
        .read(access.repo.id, move |r| {
            let (refname, commit, path) = r.split_ref_path(&spec)?;
            let short = refname.strip_prefix("refs/tags/").unwrap_or(&refname);
            let is_tag = !refname.starts_with("refs/heads/")
                && r.find_ref(&format!("refs/heads/{refname}"))?.is_none()
                && r.find_ref(&format!("refs/tags/{short}"))?.is_some();
            Ok((refname, commit, path, is_tag))
        })
        .await?;
    if !path.is_empty() {
        return Err(ApiError::NotFound);
    }
    Ok(Resolved {
        access,
        commit,
        refname,
        is_tag,
    })
}

async fn serve(
    state: &AppState,
    req: &HeaderMap,
    r: Resolved,
    format: ArchiveFormat,
    name: String,
) -> ApiResult<Response> {
    let immutable = bgh_git::is_sha(&r.refname);
    let private = r.access.repo.is_private();
    let cache = if immutable {
        cache_control(private, true)
    } else {
        HeaderValue::from_str(&format!(
            "{}, max-age=300",
            if private { "private" } else { "public" }
        ))
        .expect("header")
    };
    let prefix = format!("{name}/");
    let etag = etag_of(&[&r.commit, format.extension(), &prefix]);
    if let Some(resp) = not_modified(req, &etag, cache.clone()) {
        return Ok(resp);
    }
    let archive = bgh_git::archive::archive(
        &crate::store(state),
        r.access.repo.id,
        &r.commit,
        format,
        &prefix,
        &cache_dir(state),
    )
    .await?;
    let mut resp = Response::new(Body::from_stream(archive.stream));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    if let Some(len) = archive.len {
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    }
    let disposition = format!(
        "attachment; filename={}.{}",
        name.replace(['"', '\\', '\r', '\n', ';'], "_"),
        format.extension()
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(ApiError::internal)?,
    );
    h.insert(header::CACHE_CONTROL, cache);
    h.insert(header::ETAG, HeaderValue::from_str(&etag).expect("etag"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(resp)
}

/// `GET /{owner}/{repo}/archive/{ref}.tar.gz|.zip`
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Query(q): Query<TokenQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let (spec, format) = ArchiveFormat::split(&name).ok_or(ApiError::NotFound)?;
    let r = resolve_ref(
        &state,
        auth.as_ref(),
        &owner,
        &repo,
        spec,
        q.token.as_deref(),
    )
    .await?;
    let dir = archive_name(&r.access.repo.name, &r.refname, r.is_tag);
    serve(&state, &req, r, format, dir).await
}

#[allow(clippy::too_many_arguments)]
async fn legacy(
    state: AppState,
    auth: MaybeUser,
    owner: String,
    repo: String,
    spec: String,
    token: Option<String>,
    req: HeaderMap,
    format: ArchiveFormat,
) -> ApiResult<Response> {
    let r = resolve_ref(
        &state,
        auth.as_ref(),
        &owner,
        &repo,
        &spec,
        token.as_deref(),
    )
    .await?;
    let dir = format!(
        "{}-{}-{}",
        r.access.owner.login,
        r.access.repo.name,
        &r.commit[..7]
    );
    serve(&state, &req, r, format, dir).await
}

/// `GET /{owner}/{repo}/legacy.tar.gz/{ref}`
pub async fn legacy_tar(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    Query(q): Query<TokenQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    legacy(
        state,
        auth,
        owner,
        repo,
        spec,
        q.token,
        req,
        ArchiveFormat::TarGz,
    )
    .await
}

/// `GET /{owner}/{repo}/legacy.zip/{ref}`
pub async fn legacy_zip(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
    Query(q): Query<TokenQuery>,
    req: HeaderMap,
) -> ApiResult<Response> {
    legacy(
        state,
        auth,
        owner,
        repo,
        spec,
        q.token,
        req,
        ArchiveFormat::Zip,
    )
    .await
}

/// REST `tarball` / `zipball`: 302 to the legacy archive URL (with a
/// short-lived token for private repositories).
async fn redirect(
    state: AppState,
    auth: MaybeUser,
    owner: String,
    repo: String,
    spec: Option<String>,
    format: ArchiveFormat,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let spec = spec
        .map(|s| s.trim_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());
    let check = spec.clone();
    let path = crate::store(&state)
        .read(access.repo.id, move |r| Ok(r.split_ref_path(&check)?.2))
        .await?;
    if !path.is_empty() {
        return Err(ApiError::NotFound);
    }
    let kind = match format {
        ArchiveFormat::TarGz => "legacy.tar.gz",
        ArchiveFormat::Zip => "legacy.zip",
    };
    let mut location = state.urls.html(&format!(
        "/{}/{}/{kind}/{}",
        access.owner.login,
        access.repo.name,
        encode_path(&spec)
    ));
    // Browsers and `curl -L` don't resend credentials: private
    // repositories (and every repository in private mode) get a token.
    if access.repo.is_private() || bgh_core::privacy::private_mode(&state).await? {
        let token = super::token::issue(&state, access.repo.id).await?;
        location.push_str(&format!("?token={token}"));
    }
    Ok((
        StatusCode::FOUND,
        [(
            header::LOCATION,
            HeaderValue::from_str(&location).map_err(ApiError::internal)?,
        )],
    )
        .into_response())
}

pub async fn tarball_default(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    redirect(state, auth, owner, repo, None, ArchiveFormat::TarGz).await
}

pub async fn tarball(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    redirect(state, auth, owner, repo, Some(spec), ArchiveFormat::TarGz).await
}

pub async fn zipball_default(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    redirect(state, auth, owner, repo, None, ArchiveFormat::Zip).await
}

pub async fn zipball(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, spec)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    redirect(state, auth, owner, repo, Some(spec), ArchiveFormat::Zip).await
}

#[cfg(test)]
mod tests {
    use super::archive_name;

    #[test]
    fn names() {
        assert_eq!(archive_name("r", "main", false), "r-main");
        assert_eq!(archive_name("r", "feature/x", false), "r-feature-x");
        assert_eq!(archive_name("r", "v1.2.0", true), "r-1.2.0");
        assert_eq!(archive_name("r", "v1.2.0", false), "r-v1.2.0");
        assert_eq!(archive_name("r", "refs/heads/main", false), "r-main");
        assert_eq!(archive_name("r", "vnext", true), "r-vnext");
    }
}
