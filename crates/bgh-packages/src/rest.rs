//! GitHub Packages REST API (`/user/packages`, `/users/{u}/packages`,
//! `/orgs/{org}/packages`, versions, delete and restore).

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use bgh_core::audit;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::access::{self, Caps};
use crate::model::{self, PACKAGE_TYPES, PackageRow, VersionRow};
use crate::visible::{self, ListFilter};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/packages", get(list_viewer))
        .route("/users/{username}/packages", get(list_user))
        .route("/orgs/{org}/packages", get(list_org))
        .route(
            "/user/packages/{package_type}/{package_name}",
            get(viewer_get).delete(viewer_delete),
        )
        .route(
            "/user/packages/{package_type}/{package_name}/restore",
            post(viewer_restore),
        )
        .route(
            "/user/packages/{package_type}/{package_name}/versions",
            get(viewer_versions),
        )
        .route(
            "/user/packages/{package_type}/{package_name}/versions/{version_id}",
            get(viewer_version).delete(viewer_version_delete),
        )
        .route(
            "/user/packages/{package_type}/{package_name}/versions/{version_id}/restore",
            post(viewer_version_restore),
        )
        .merge(owner_routes("users"))
        .merge(owner_routes("orgs"))
}

/// `/{users|orgs}/{owner}/packages/{type}/{name}[/...]`.
fn owner_routes(kind: &'static str) -> Router<AppState> {
    let param = if kind == "users" { "username" } else { "org" };
    let base = format!("/{kind}/{{{param}}}/packages/{{package_type}}/{{package_name}}");
    Router::new()
        .route(
            &base,
            get(move |s, a, p| owner_get(kind, s, a, p))
                .delete(move |s, a, p| owner_delete(kind, s, a, p)),
        )
        .route(
            &format!("{base}/restore"),
            post(move |s, a, p| owner_restore(kind, s, a, p)),
        )
        .route(
            &format!("{base}/versions"),
            get(move |s, a, p, q, pg| owner_versions(kind, s, a, p, q, pg)),
        )
        .route(
            &format!("{base}/versions/{{version_id}}"),
            get(move |s, a, p| owner_version(kind, s, a, p))
                .delete(move |s, a, p| owner_version_delete(kind, s, a, p)),
        )
        .route(
            &format!("{base}/versions/{{version_id}}/restore"),
            post(move |s, a, p| owner_version_restore(kind, s, a, p)),
        )
}

#[derive(Deserialize)]
pub struct ListQuery {
    package_type: Option<String>,
    visibility: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct VersionsQuery {
    state: Option<String>,
}

fn check_type(t: Option<&str>) -> ApiResult<&str> {
    match t {
        None => Err(ApiError::invalid_field(FieldError::missing_field(
            "Package",
            "package_type",
        ))),
        Some(t) if PACKAGE_TYPES.contains(&t) => Ok(t),
        Some(_) => Err(ApiError::invalid_field(FieldError::invalid(
            "Package",
            "package_type",
        ))),
    }
}

async fn list(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: Option<&db::User>,
    q: &ListQuery,
    p: Pagination,
) -> ApiResult<Page<model::Package>> {
    let package_type = check_type(q.package_type.as_deref())?;
    let visibility = match q.visibility.as_deref() {
        None => None,
        Some(v @ ("public" | "private" | "internal")) => Some(v),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Package",
                "visibility",
            )));
        }
    };
    let rows = visible::list(
        state,
        auth,
        &ListFilter {
            owner_id: owner.map(|o| o.id),
            repo_id: None,
            package_type: Some(package_type),
            visibility,
            query: None,
        },
        p.limit_plus_one(),
        p.offset(),
    )
    .await?;
    let page = p.page(rows);
    let items = model::packages_json(state, auth, &page.items).await?;
    Ok(Page {
        items,
        link: page.link,
    })
}

async fn list_viewer(
    State(state): State<AppState>,
    RequireUser(auth): RequireUser,
    Query(q): Query<ListQuery>,
    p: Pagination,
) -> ApiResult<Page<model::Package>> {
    let user = auth.user.clone();
    list(&state, Some(&auth), Some(&user), &q, p).await
}

async fn list_user(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(owner): Path<String>,
    Query(q): Query<ListQuery>,
    p: Pagination,
) -> ApiResult<Page<model::Package>> {
    let owner = visible::owner(&state, &owner).await?;
    list(&state, auth.as_ref(), Some(&owner), &q, p).await
}

async fn list_org(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(owner): Path<String>,
    Query(q): Query<ListQuery>,
    p: Pagination,
) -> ApiResult<Page<model::Package>> {
    let owner = visible::owner(&state, &owner).await?;
    if !owner.is_org() {
        return Err(ApiError::NotFound);
    }
    list(&state, auth.as_ref(), Some(&owner), &q, p).await
}

/// Owner from `/{users|orgs}/{owner}` or the authenticated user.
async fn resolve_owner(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
) -> ApiResult<db::User> {
    match sel {
        None => auth
            .map(|a| a.user.clone())
            .ok_or_else(ApiError::requires_auth),
        Some((kind, login)) => {
            let owner = visible::owner(state, login).await?;
            match kind {
                "users" => Ok(owner),
                "orgs" if owner.is_org() => Ok(owner),
                _ => Err(ApiError::NotFound),
            }
        }
    }
}

/// A live package the caller can read (404 otherwise).
pub async fn load(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &db::User,
    package_type: &str,
    name: &str,
) -> ApiResult<(PackageRow, Caps)> {
    let pkg = model::find_package(&state.db, owner.id, package_type, name)
        .await?
        .ok_or(ApiError::NotFound)?;
    let caps = access::package_caps(state, auth, owner, Some(&pkg)).await?;
    if !caps.read {
        return Err(ApiError::NotFound);
    }
    Ok((pkg, caps))
}

fn require_admin(auth: Option<&AuthContext>, caps: &Caps) -> ApiResult<()> {
    if caps.admin {
        return Ok(());
    }
    match auth {
        None => Err(ApiError::requires_auth()),
        Some(a) if !a.has_scope("delete:packages") => a.require_scope("delete:packages"),
        Some(_) => Err(ApiError::forbidden(
            "You must have admin permissions on this package.",
        )),
    }
}

fn audit_target(owner: &db::User) -> audit::Target {
    if owner.is_org() {
        audit::Target::Org(owner.id)
    } else {
        audit::Target::User(owner.id)
    }
}

// ----- package ---------------------------------------------------------------

async fn get_package(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
) -> ApiResult<Json<model::Package>> {
    let owner = resolve_owner(state, auth, sel).await?;
    let (pkg, _) = load(state, auth, &owner, package_type, name).await?;
    let mut json = model::packages_json(state, auth, std::slice::from_ref(&pkg)).await?;
    json.pop().map(Json).ok_or(ApiError::NotFound)
}

async fn delete_package(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
) -> ApiResult<StatusCode> {
    let owner = resolve_owner(state, auth, sel).await?;
    let (pkg, caps) = load(state, auth, &owner, package_type, name).await?;
    require_admin(auth, &caps)?;
    let mut tx = Tx::begin(state).await?;
    sqlx::query("UPDATE packages SET deleted_at = now() WHERE id = $1")
        .bind(pkg.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        auth.map(|a| &a.user),
        "package.delete",
        audit_target(&owner),
        json!({ "package": pkg.name, "package_type": pkg.package_type, "package_id": pkg.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_package(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
) -> ApiResult<StatusCode> {
    let owner = resolve_owner(state, auth, sel).await?;
    let deleted: Option<PackageRow> = sqlx::query_as(&format!(
        "SELECT {} FROM packages
          WHERE owner_id = $1 AND package_type = $2 AND lower(name) = lower($3)
            AND deleted_at > now() - interval '30 days'
          ORDER BY deleted_at DESC LIMIT 1",
        PackageRow::COLUMNS
    ))
    .bind(owner.id)
    .bind(package_type)
    .bind(name)
    .fetch_optional(&state.db)
    .await?;
    let pkg = deleted.ok_or(ApiError::NotFound)?;
    let caps = access::package_caps(state, auth, &owner, Some(&pkg)).await?;
    if !caps.read && !caps.admin {
        return Err(ApiError::NotFound);
    }
    require_admin(auth, &caps)?;
    let mut tx = Tx::begin(state).await?;
    sqlx::query("UPDATE packages SET deleted_at = NULL, updated_at = now() WHERE id = $1")
        .bind(pkg.id)
        .execute(&mut *tx)
        .await
        .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
            Some("packages_owner_type_name_key") => ApiError::conflict(
                "A package with this name already exists. Delete it before restoring.",
            ),
            _ => e.into(),
        })?;
    audit::log(
        &mut *tx,
        auth.map(|a| &a.user),
        "package.restore",
        audit_target(&owner),
        json!({ "package": pkg.name, "package_type": pkg.package_type, "package_id": pkg.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- versions --------------------------------------------------------------

async fn list_versions(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
    q: &VersionsQuery,
    p: Pagination,
) -> ApiResult<Page<model::PackageVersion>> {
    let owner = resolve_owner(state, auth, sel).await?;
    let (pkg, _) = load(state, auth, &owner, package_type, name).await?;
    let deleted = match q.state.as_deref() {
        None | Some("active") => false,
        Some("deleted") => true,
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "PackageVersion",
                "state",
            )));
        }
    };
    let rows: Vec<VersionRow> = sqlx::query_as(&format!(
        "SELECT {} FROM package_versions
          WHERE package_id = $1 AND (deleted_at IS NOT NULL) = $2
          ORDER BY created_at DESC, id DESC LIMIT $3 OFFSET $4",
        VersionRow::COLUMNS
    ))
    .bind(pkg.id)
    .bind(deleted)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows)
        .map(|v| model::version_json(state, &owner, &pkg, &v)))
}

async fn load_version(state: &AppState, pkg: &PackageRow, id: i64) -> ApiResult<VersionRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM package_versions WHERE id = $1 AND package_id = $2",
        VersionRow::COLUMNS
    ))
    .bind(id)
    .bind(pkg.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn get_version(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
    id: i64,
) -> ApiResult<Json<model::PackageVersion>> {
    let owner = resolve_owner(state, auth, sel).await?;
    let (pkg, _) = load(state, auth, &owner, package_type, name).await?;
    let v = load_version(state, &pkg, id).await?;
    Ok(Json(model::version_json(state, &owner, &pkg, &v)))
}

async fn delete_version(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
    id: i64,
) -> ApiResult<StatusCode> {
    let owner = resolve_owner(state, auth, sel).await?;
    let (pkg, caps) = load(state, auth, &owner, package_type, name).await?;
    let v = load_version(state, &pkg, id).await?;
    require_admin(auth, &caps)?;
    if v.deleted_at.is_some() {
        return Err(ApiError::NotFound);
    }
    let mut tx = Tx::begin(state).await?;
    // Lock the package so two concurrent deletes can't remove the last two.
    sqlx::query("SELECT id FROM packages WHERE id = $1 FOR UPDATE")
        .bind(pkg.id)
        .execute(&mut *tx)
        .await?;
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM package_versions WHERE package_id = $1 AND deleted_at IS NULL",
    )
    .bind(pkg.id)
    .fetch_one(&mut *tx)
    .await?;
    if live <= 1 {
        return Err(ApiError::bad_request(
            "You cannot delete the last version of a package. You must delete the package instead.",
        ));
    }
    sqlx::query("UPDATE package_versions SET deleted_at = now() WHERE id = $1")
        .bind(v.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE packages SET updated_at = now() WHERE id = $1")
        .bind(pkg.id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        auth.map(|a| &a.user),
        "package_version.delete",
        audit_target(&owner),
        json!({ "package": pkg.name, "package_id": pkg.id, "version": v.digest, "version_id": v.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn restore_version(
    state: &AppState,
    auth: Option<&AuthContext>,
    sel: Option<(&str, &str)>,
    package_type: &str,
    name: &str,
    id: i64,
) -> ApiResult<StatusCode> {
    let owner = resolve_owner(state, auth, sel).await?;
    let (pkg, caps) = load(state, auth, &owner, package_type, name).await?;
    let v = load_version(state, &pkg, id).await?;
    require_admin(auth, &caps)?;
    let mut tx = Tx::begin(state).await?;
    // Tags moved to other versions meanwhile stay where they are.
    sqlx::query(
        "UPDATE package_versions v SET deleted_at = NULL, updated_at = now(),
                tags = ARRAY(SELECT t FROM unnest(v.tags) AS t
                              WHERE NOT EXISTS (SELECT 1 FROM package_versions o
                                                 WHERE o.package_id = v.package_id AND o.id <> v.id
                                                   AND o.deleted_at IS NULL AND t = ANY(o.tags)))
          WHERE v.id = $1",
    )
    .bind(v.id)
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        auth.map(|a| &a.user),
        "package_version.restore",
        audit_target(&owner),
        json!({ "package": pkg.name, "package_id": pkg.id, "version": v.digest, "version_id": v.id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- route adapters --------------------------------------------------------

async fn viewer_get(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n)): Path<(String, String)>,
) -> ApiResult<Json<model::Package>> {
    get_package(&s, Some(&a), None, &t, &n).await
}
async fn viewer_delete(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    delete_package(&s, Some(&a), None, &t, &n).await
}
async fn viewer_restore(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    restore_package(&s, Some(&a), None, &t, &n).await
}
async fn viewer_versions(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n)): Path<(String, String)>,
    Query(q): Query<VersionsQuery>,
    p: Pagination,
) -> ApiResult<Page<model::PackageVersion>> {
    list_versions(&s, Some(&a), None, &t, &n, &q, p).await
}
async fn viewer_version(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n, id)): Path<(String, String, i64)>,
) -> ApiResult<Json<model::PackageVersion>> {
    get_version(&s, Some(&a), None, &t, &n, id).await
}
async fn viewer_version_delete(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    delete_version(&s, Some(&a), None, &t, &n, id).await
}
async fn viewer_version_restore(
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((t, n, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    restore_version(&s, Some(&a), None, &t, &n, id).await
}

async fn owner_get(
    k: &'static str,
    State(s): State<AppState>,
    a: MaybeUser,
    Path((o, t, n)): Path<(String, String, String)>,
) -> ApiResult<Json<model::Package>> {
    get_package(&s, a.as_ref(), Some((k, &o)), &t, &n).await
}
async fn owner_delete(
    k: &'static str,
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((o, t, n)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    delete_package(&s, Some(&a), Some((k, &o)), &t, &n).await
}
async fn owner_restore(
    k: &'static str,
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((o, t, n)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    restore_package(&s, Some(&a), Some((k, &o)), &t, &n).await
}
async fn owner_versions(
    k: &'static str,
    State(s): State<AppState>,
    a: MaybeUser,
    Path((o, t, n)): Path<(String, String, String)>,
    Query(q): Query<VersionsQuery>,
    p: Pagination,
) -> ApiResult<Page<model::PackageVersion>> {
    list_versions(&s, a.as_ref(), Some((k, &o)), &t, &n, &q, p).await
}
async fn owner_version(
    k: &'static str,
    State(s): State<AppState>,
    a: MaybeUser,
    Path((o, t, n, id)): Path<(String, String, String, i64)>,
) -> ApiResult<Json<model::PackageVersion>> {
    get_version(&s, a.as_ref(), Some((k, &o)), &t, &n, id).await
}
async fn owner_version_delete(
    k: &'static str,
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((o, t, n, id)): Path<(String, String, String, i64)>,
) -> ApiResult<StatusCode> {
    delete_version(&s, Some(&a), Some((k, &o)), &t, &n, id).await
}
async fn owner_version_restore(
    k: &'static str,
    State(s): State<AppState>,
    RequireUser(a): RequireUser,
    Path((o, t, n, id)): Path<(String, String, String, i64)>,
) -> ApiResult<StatusCode> {
    restore_version(&s, Some(&a), Some((k, &o)), &t, &n, id).await
}
