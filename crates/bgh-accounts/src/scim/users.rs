//! SCIM `Users` (enterprise and organization tenants).

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::prelude::*;
use bgh_core::settings::{self, SignupPolicy};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::types::Json as SqlJson;
use uuid::Uuid;

use super::{
    ListQuery, PatchOperation, ScimError, ScimResult, Tenant, USER_SCHEMA, as_bool, deprovision,
    enterprise_auth, list_response, org_auth, patch_operations, reactivate, scim_json,
};
use crate::{orgs, sso, users};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ScimUserRow {
    pub id: Uuid,
    pub org_id: Option<i64>,
    pub user_id: i64,
    pub external_id: Option<String>,
    pub user_name: String,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub formatted_name: Option<String>,
    pub emails: SqlJson<Vec<Value>>,
    pub roles: SqlJson<Vec<Value>>,
    pub active: bool,
    pub suspended_by_scim: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const COLUMNS: &str = "id, org_id, user_id, external_id, user_name, display_name, given_name, \
     family_name, formatted_name, emails, roles, active, suspended_by_scim, created_at, updated_at";

/// The writable attributes of a SCIM user.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields {
    pub external_id: Option<String>,
    pub user_name: String,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub formatted_name: Option<String>,
    pub emails: Vec<Value>,
    pub roles: Vec<Value>,
    pub active: bool,
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn list(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.iter().filter(|x| x.is_object()).cloned().collect(),
        Value::Object(_) => vec![v.clone()],
        _ => Vec::new(),
    }
}

impl Fields {
    fn from_row(r: &ScimUserRow) -> Self {
        Self {
            external_id: r.external_id.clone(),
            user_name: r.user_name.clone(),
            display_name: r.display_name.clone(),
            given_name: r.given_name.clone(),
            family_name: r.family_name.clone(),
            formatted_name: r.formatted_name.clone(),
            emails: r.emails.0.clone(),
            roles: r.roles.0.clone(),
            active: r.active,
        }
    }

    /// From a full resource (POST / PUT).
    pub fn from_resource(v: &Value) -> ScimResult<Self> {
        let mut f = Fields {
            active: true,
            ..Default::default()
        };
        let obj = v
            .as_object()
            .ok_or_else(|| ScimError::bad("invalidSyntax", "Body must be a JSON object"))?;
        for (k, val) in obj {
            f.set(k, val)?;
        }
        f.validate()?;
        Ok(f)
    }

    fn validate(&self) -> ScimResult<()> {
        if self.user_name.trim().is_empty() {
            return Err(ScimError::bad("invalidValue", "userName is required"));
        }
        if self.user_name.len() > 255 {
            return Err(ScimError::bad("invalidValue", "userName is too long"));
        }
        Ok(())
    }

    /// Set one attribute (`path` as in SCIM, case-insensitive).
    fn set(&mut self, path: &str, v: &Value) -> ScimResult<()> {
        let p = path.to_ascii_lowercase();
        let p = p
            .strip_prefix("urn:ietf:params:scim:schemas:core:2.0:user:")
            .unwrap_or(&p);
        match p {
            "active" => {
                self.active = as_bool(v)
                    .ok_or_else(|| ScimError::bad("invalidValue", "active must be a boolean"))?;
            }
            "username" => self.user_name = opt_str(v).unwrap_or_default(),
            "externalid" => self.external_id = opt_str(v),
            "displayname" => self.display_name = opt_str(v),
            "name" => {
                if let Some(o) = v.as_object() {
                    for (k, val) in o {
                        self.set(&format!("name.{k}"), val)?;
                    }
                } else if v.is_null() {
                    self.given_name = None;
                    self.family_name = None;
                    self.formatted_name = None;
                }
            }
            "name.givenname" => self.given_name = opt_str(v),
            "name.familyname" => self.family_name = opt_str(v),
            "name.formatted" => self.formatted_name = opt_str(v),
            "emails" => self.emails = list(v),
            "roles" => self.roles = list(v),
            _ => {
                // `emails[type eq "work"].value`
                if let Some(t) = typed_email_path(p) {
                    let value = opt_str(v);
                    if let Some(e) = self.emails.iter_mut().find(|e| {
                        e["type"]
                            .as_str()
                            .is_some_and(|x| x.eq_ignore_ascii_case(&t))
                    }) {
                        e["value"] = json!(value);
                    } else if let Some(value) = value {
                        self.emails.push(json!({
                            "value": value,
                            "type": t,
                            "primary": self.emails.is_empty(),
                        }));
                    }
                }
                // Other attributes (enterprise extension, …) are ignored.
            }
        }
        Ok(())
    }

    fn remove(&mut self, path: &str) -> ScimResult<()> {
        let p = path.to_ascii_lowercase();
        match p.as_str() {
            "active" | "username" => Err(ScimError::bad(
                "mutability",
                format!("{path} can't be removed"),
            )),
            "emails" => {
                self.emails.clear();
                Ok(())
            }
            "roles" => {
                self.roles.clear();
                Ok(())
            }
            _ => {
                if let Some(t) = typed_email_path(&p) {
                    self.emails.retain(|e| {
                        !e["type"]
                            .as_str()
                            .is_some_and(|x| x.eq_ignore_ascii_case(&t))
                    });
                    return Ok(());
                }
                self.set(&p, &Value::Null)
            }
        }
    }

    /// Apply `PatchOp` operations.
    pub fn patch(&mut self, ops: &[PatchOperation]) -> ScimResult<()> {
        for o in ops {
            match (&o.path, o.op.as_str()) {
                (Some(p), "remove") => self.remove(p)?,
                (Some(p), "add")
                    if p.eq_ignore_ascii_case("emails") || p.eq_ignore_ascii_case("roles") =>
                {
                    let target = if p.eq_ignore_ascii_case("emails") {
                        &mut self.emails
                    } else {
                        &mut self.roles
                    };
                    for item in list(&o.value) {
                        if !target.iter().any(|x| x["value"] == item["value"]) {
                            target.push(item);
                        }
                    }
                }
                (Some(p), _) => self.set(p, &o.value)?,
                (None, "remove") => {
                    return Err(ScimError::bad("noTarget", "remove needs a path"));
                }
                (None, _) => {
                    let obj = o.value.as_object().ok_or_else(|| {
                        ScimError::bad("invalidValue", "value must be an object without a path")
                    })?;
                    for (k, val) in obj {
                        self.set(k, val)?;
                    }
                }
            }
        }
        self.validate()
    }

    pub fn primary_email(&self) -> Option<String> {
        self.emails
            .iter()
            .find(|e| e["primary"].as_bool() == Some(true) || e["primary"] == json!("true"))
            .or_else(|| self.emails.first())
            .and_then(|e| opt_str(&e["value"]))
    }

    fn email_values(&self) -> Vec<String> {
        self.emails
            .iter()
            .filter_map(|e| opt_str(&e["value"]))
            .filter(|e| e.contains('@'))
            .collect()
    }

    /// The account name: displayName, formatted, or given + family.
    pub fn name(&self) -> Option<String> {
        self.display_name
            .clone()
            .or_else(|| self.formatted_name.clone())
            .or_else(|| {
                let n = [self.given_name.as_deref(), self.family_name.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(n).filter(|n| !n.is_empty())
            })
    }

    /// `Some(owner?)` when roles were sent.
    pub fn enterprise_owner(&self) -> Option<bool> {
        if self.roles.is_empty() {
            return None;
        }
        Some(self.roles.iter().any(|r| {
            r["value"].as_str().is_some_and(|v| {
                let v = v.to_ascii_lowercase().replace([' ', '-'], "_");
                v == "enterprise_owner"
            })
        }))
    }
}

fn typed_email_path(p: &str) -> Option<String> {
    let rest = p.strip_prefix("emails[type eq \"")?;
    let (t, tail) = rest.split_once("\"]")?;
    (tail == ".value").then(|| t.to_string())
}

/// The SCIM resource of a row.
pub fn resource(base: &str, r: &ScimUserRow, enterprise: bool) -> Value {
    let mut v = json!({
        "schemas": [USER_SCHEMA],
        "id": r.id.to_string(),
        "externalId": r.external_id,
        "userName": r.user_name,
        "displayName": r.display_name,
        "name": {
            "givenName": r.given_name,
            "familyName": r.family_name,
            "formatted": r.formatted_name,
        },
        "emails": r.emails.0,
        "active": r.active,
        "meta": {
            "resourceType": "User",
            "created": Timestamp::from(r.created_at),
            "lastModified": Timestamp::from(r.updated_at),
            "location": format!("{base}/Users/{}", r.id),
        },
    });
    if enterprise {
        v["roles"] = json!(r.roles.0);
    }
    v
}

fn parse_id(id: &str) -> ScimResult<Uuid> {
    Uuid::parse_str(id).map_err(|_| ScimError::not_found())
}

async fn load(state: &AppState, tenant: &Tenant, id: &str) -> ScimResult<ScimUserRow> {
    let id = parse_id(id)?;
    sqlx::query_as::<_, ScimUserRow>(&format!(
        "SELECT {COLUMNS} FROM scim_users WHERE id = $1 AND coalesce(org_id, 0) = coalesce($2, 0)"
    ))
    .bind(id)
    .bind(tenant.org_id())
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ScimError::not_found)
}

async fn list_users(
    state: &AppState,
    tenant: &Tenant,
    base: &str,
    q: &ListQuery,
) -> ScimResult<Response> {
    let mut conds = vec!["coalesce(org_id, 0) = coalesce($1, 0)".to_string()];
    let mut binds: Vec<String> = Vec::new();
    for (attr, value) in q.filter()? {
        let n = binds.len() + 2;
        conds.push(match attr.as_str() {
            "username" => format!("lower(user_name) = lower(${n})"),
            "externalid" => format!("external_id = ${n}"),
            "id" => format!("id::text = ${n}"),
            "displayname" => format!("display_name = ${n}"),
            "emails" | "emails.value" => format!(
                "EXISTS (SELECT 1 FROM jsonb_array_elements(emails) e WHERE lower(e->>'value') = lower(${n}))"
            ),
            "active" => format!("active::text = lower(${n})"),
            _ => {
                return Err(ScimError::bad(
                    "invalidFilter",
                    format!("Unsupported filter attribute {attr:?}"),
                ));
            }
        });
        binds.push(value);
    }
    let where_ = conds.join(" AND ");
    let count_sql = format!("SELECT count(*) FROM scim_users WHERE {where_}");
    let mut count = sqlx::query_scalar::<_, i64>(&count_sql).bind(tenant.org_id());
    for b in &binds {
        count = count.bind(b);
    }
    let total = count.fetch_one(&state.db).await?;
    let n = binds.len() + 2;
    let rows_sql = format!(
        "SELECT {COLUMNS} FROM scim_users WHERE {where_} ORDER BY created_at, id LIMIT ${n} OFFSET ${}",
        n + 1
    );
    let mut rows = sqlx::query_as::<_, ScimUserRow>(&rows_sql).bind(tenant.org_id());
    for b in &binds {
        rows = rows.bind(b);
    }
    let rows = rows
        .bind(q.limit())
        .bind(q.offset())
        .fetch_all(&state.db)
        .await?;
    let enterprise = matches!(tenant, Tenant::Enterprise);
    Ok(list_response(
        total,
        q.offset(),
        rows.iter().map(|r| resource(base, r, enterprise)).collect(),
    ))
}

/// The local account a new SCIM user stands for: the account with that
/// login (or the local part of an email userName), else the owner of the
/// primary email, else a new account.
async fn find_or_create_account(
    state: &AppState,
    actor: &AuthContext,
    tenant: &Tenant,
    f: &Fields,
) -> ScimResult<db::User> {
    let local = f
        .user_name
        .split('@')
        .next()
        .unwrap_or(&f.user_name)
        .to_string();
    let existing: Option<db::User> = sqlx::query_as(&format!(
        "SELECT {} FROM users u
          WHERE u.type = 'User' AND lower(u.login) IN (lower($1), lower($2))
            AND NOT EXISTS (SELECT 1 FROM scim_users s
                             WHERE s.user_id = u.id AND coalesce(s.org_id, 0) = coalesce($3, 0))
          ORDER BY lower(u.login) = lower($1) DESC LIMIT 1",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&f.user_name)
    .bind(&local)
    .bind(tenant.org_id())
    .fetch_optional(&state.db)
    .await?;
    if let Some(u) = existing {
        return Ok(u);
    }
    if let Some(email) = f.primary_email() {
        let by_email: Option<db::User> = sqlx::query_as(&format!(
            "SELECT {} FROM users u JOIN user_emails e ON e.user_id = u.id
              WHERE lower(e.email) = lower($1) AND e.verified AND u.type = 'User'
                AND NOT EXISTS (SELECT 1 FROM scim_users s
                                 WHERE s.user_id = u.id AND coalesce(s.org_id, 0) = coalesce($2, 0))",
            db::prefixed("u", db::User::COLUMNS)
        ))
        .bind(&email)
        .bind(tenant.org_id())
        .fetch_optional(&state.db)
        .await?;
        if let Some(u) = by_email {
            return Ok(u);
        }
    }
    if let Tenant::Org(_) = tenant {
        let s = settings::load(state).await?;
        if s.signup.policy != SignupPolicy::Open && !actor.user.site_admin {
            return Err(ScimError::new(
                StatusCode::FORBIDDEN,
                None,
                "No account matches this userName and sign-up is restricted: ask a site administrator to provision it.",
            ));
        }
    }
    let login = sso::available_login(state, &f.user_name).await?;
    let mut tx = Tx::begin(state).await?;
    let mut email = None;
    for e in f.email_values() {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM user_emails WHERE lower(email) = lower($1) AND verified)",
        )
        .bind(&e)
        .fetch_one(&mut *tx)
        .await?;
        if !taken {
            email = Some(e);
            break;
        }
    }
    let host = url::Url::parse(&state.config.base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "localhost".into());
    let email = email.unwrap_or_else(|| format!("{login}@users.noreply.{host}"));
    let user = users::insert_user(
        &mut tx,
        &login,
        &email,
        f.name().as_deref(),
        None,
        Some(false),
        true,
    )
    .await?;
    audit::log(
        &mut *tx,
        Some(&actor.user),
        "user.create",
        audit::Target::User(user.id),
        json!({ "login": login, "scim": true }),
    )
    .await?;
    tx.commit().await?;
    Ok(user)
}

/// Bring the account in line with the SCIM user after a write.
async fn apply(
    state: &AppState,
    actor: &AuthContext,
    tenant: &Tenant,
    row: &ScimUserRow,
    f: &Fields,
) -> ScimResult<()> {
    let user = db::User::find(&state.db, row.user_id)
        .await?
        .ok_or_else(ScimError::not_found)?;
    match tenant {
        Tenant::Enterprise => {
            let mut tx = Tx::begin(state).await?;
            let mut changed = false;
            if let Some(name) = f.name()
                && user.name.as_deref() != Some(name.as_str())
            {
                sqlx::query("UPDATE users SET name = $2, updated_at = now() WHERE id = $1")
                    .bind(user.id)
                    .bind(&name)
                    .execute(&mut *tx)
                    .await?;
                changed = true;
            }
            for email in f.email_values() {
                sqlx::query(
                    "INSERT INTO user_emails (user_id, email, verified, is_primary)
                     SELECT $1, $2, true,
                            NOT EXISTS (SELECT 1 FROM user_emails WHERE user_id = $1 AND is_primary)
                      WHERE NOT EXISTS (SELECT 1 FROM user_emails WHERE lower(email) = lower($2))",
                )
                .bind(user.id)
                .bind(&email)
                .execute(&mut *tx)
                .await?;
            }
            if f.active
                && let Some(owner) = f.enterprise_owner()
                && owner != user.site_admin
            {
                crate::saml::provision::set_site_admin(
                    &mut tx,
                    &user,
                    owner,
                    json!({ "scim": true }),
                )
                .await?;
                changed = true;
            }
            if changed {
                tx.sync_user(user.id).await?;
            }
            tx.commit().await?;
            if !f.active {
                let suspended =
                    deprovision(state, &user, "deprovisioned by the identity provider").await?;
                if suspended {
                    sqlx::query("UPDATE scim_users SET suspended_by_scim = true WHERE id = $1")
                        .bind(row.id)
                        .execute(&state.db)
                        .await?;
                }
            } else if row.suspended_by_scim {
                reactivate(state, &user).await?;
                sqlx::query("UPDATE scim_users SET suspended_by_scim = false WHERE id = $1")
                    .bind(row.id)
                    .execute(&state.db)
                    .await?;
            }
            super::groups::sync_user_teams(state, row.id).await?;
        }
        Tenant::Org(org) => {
            let member = bgh_core::perms::org_role(&state.db, org.id, user.id).await?;
            if f.active && member.is_none() {
                let mut tx = Tx::begin(state).await?;
                orgs::add_member(&mut tx, org, &user, "member", &[], actor.user.id).await?;
                tx.commit().await?;
            } else if !f.active && member.is_some() {
                orgs::remove_member(state, &actor.user, org, &user).await?;
            }
        }
    }
    Ok(())
}

fn created(location: String, body: Value) -> Response {
    let mut resp = scim_json(StatusCode::CREATED, body);
    if let Ok(v) = HeaderValue::from_str(&location) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp
}

async fn create(
    state: &AppState,
    actor: &AuthContext,
    tenant: &Tenant,
    base: &str,
    body: &Value,
) -> ScimResult<Response> {
    let f = Fields::from_resource(body)?;
    let clash: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scim_users
                         WHERE coalesce(org_id, 0) = coalesce($1, 0)
                           AND (lower(user_name) = lower($2)
                                OR ($3::text IS NOT NULL AND external_id = $3)))",
    )
    .bind(tenant.org_id())
    .bind(&f.user_name)
    .bind(&f.external_id)
    .fetch_one(&state.db)
    .await?;
    if clash {
        return Err(ScimError::conflict(
            "User already exists (userName or externalId)",
        ));
    }
    let user = find_or_create_account(state, actor, tenant, &f).await?;
    let row: ScimUserRow = sqlx::query_as(&format!(
        "INSERT INTO scim_users (id, org_id, user_id, external_id, user_name, display_name,
                                 given_name, family_name, formatted_name, emails, roles, active)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING {COLUMNS}"
    ))
    .bind(Uuid::new_v4())
    .bind(tenant.org_id())
    .bind(user.id)
    .bind(&f.external_id)
    .bind(&f.user_name)
    .bind(&f.display_name)
    .bind(&f.given_name)
    .bind(&f.family_name)
    .bind(&f.formatted_name)
    .bind(SqlJson(&f.emails))
    .bind(SqlJson(&f.roles))
    .bind(f.active)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match bgh_core::db::unique_violation(&e).as_deref() {
        Some(_) => ScimError::conflict("User already exists"),
        None => e.into(),
    })?;
    audit::log(
        &state.db,
        Some(&actor.user),
        "user.scim_provision",
        audit::Target::User(user.id),
        json!({ "login": user.login, "user_name": f.user_name, "org_id": tenant.org_id() }),
    )
    .await?;
    apply(state, actor, tenant, &row, &f).await?;
    let row = load(state, tenant, &row.id.to_string()).await?;
    Ok(created(
        format!("{base}/Users/{}", row.id),
        resource(base, &row, matches!(tenant, Tenant::Enterprise)),
    ))
}

async fn update(
    state: &AppState,
    actor: &AuthContext,
    tenant: &Tenant,
    base: &str,
    id: &str,
    f: Fields,
) -> ScimResult<Response> {
    let row = load(state, tenant, id).await?;
    let clash: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM scim_users
                         WHERE coalesce(org_id, 0) = coalesce($1, 0) AND id <> $2
                           AND (lower(user_name) = lower($3)
                                OR ($4::text IS NOT NULL AND external_id = $4)))",
    )
    .bind(tenant.org_id())
    .bind(row.id)
    .bind(&f.user_name)
    .bind(&f.external_id)
    .fetch_one(&state.db)
    .await?;
    if clash {
        return Err(ScimError::conflict(
            "userName or externalId is already taken",
        ));
    }
    if f != Fields::from_row(&row) {
        sqlx::query(
            "UPDATE scim_users SET external_id = $2, user_name = $3, display_name = $4,
                    given_name = $5, family_name = $6, formatted_name = $7, emails = $8,
                    roles = $9, active = $10, updated_at = now()
              WHERE id = $1",
        )
        .bind(row.id)
        .bind(&f.external_id)
        .bind(&f.user_name)
        .bind(&f.display_name)
        .bind(&f.given_name)
        .bind(&f.family_name)
        .bind(&f.formatted_name)
        .bind(SqlJson(&f.emails))
        .bind(SqlJson(&f.roles))
        .bind(f.active)
        .execute(&state.db)
        .await?;
        if f.active != row.active {
            audit::log(
                &state.db,
                Some(&actor.user),
                if f.active {
                    "user.scim_reactivate"
                } else {
                    "user.scim_deprovision"
                },
                audit::Target::User(row.user_id),
                json!({ "user_name": f.user_name, "org_id": tenant.org_id() }),
            )
            .await?;
        }
        let row = load(state, tenant, id).await?;
        apply(state, actor, tenant, &row, &f).await?;
    }
    let row = load(state, tenant, id).await?;
    Ok(scim_json(
        StatusCode::OK,
        resource(base, &row, matches!(tenant, Tenant::Enterprise)),
    ))
}

async fn patch(
    state: &AppState,
    actor: &AuthContext,
    tenant: &Tenant,
    base: &str,
    id: &str,
    body: &Value,
) -> ScimResult<Response> {
    let ops = patch_operations(body)?;
    let row = load(state, tenant, id).await?;
    let mut f = Fields::from_row(&row);
    f.patch(&ops)?;
    update(state, actor, tenant, base, id, f).await
}

async fn delete(
    state: &AppState,
    actor: &AuthContext,
    tenant: &Tenant,
    id: &str,
) -> ScimResult<Response> {
    let row = load(state, tenant, id).await?;
    let user = db::User::find(&state.db, row.user_id).await?;
    let groups = super::groups::groups_of(state, row.id).await?;
    if let Some(user) = &user {
        match tenant {
            Tenant::Enterprise => {
                deprovision(state, user, "removed by the identity provider").await?;
            }
            Tenant::Org(org) => {
                orgs::remove_member(state, &actor.user, org, user).await?;
            }
        }
    }
    sqlx::query("DELETE FROM scim_users WHERE id = $1")
        .bind(row.id)
        .execute(&state.db)
        .await?;
    audit::log(
        &state.db,
        Some(&actor.user),
        "user.scim_delete",
        audit::Target::User(row.user_id),
        json!({ "user_name": row.user_name, "org_id": tenant.org_id() }),
    )
    .await?;
    super::groups::sync_teams(state, &groups).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn body_value(body: &axum::body::Bytes) -> ScimResult<Value> {
    serde_json::from_slice(body)
        .map_err(|_| ScimError::bad("invalidSyntax", "Problems parsing JSON"))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

type Body = axum::body::Bytes;

/// `GET /scim/v2/enterprises/{enterprise}/Users`
pub async fn enterprise_list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(enterprise): Path<String>,
    Query(q): Query<ListQuery>,
) -> ScimResult<Response> {
    enterprise_auth(&state, &auth).await?;
    let t = Tenant::Enterprise;
    list_users(&state, &t, &t.base(&state, &enterprise), &q).await
}

/// `POST /scim/v2/enterprises/{enterprise}/Users`
pub async fn enterprise_create(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(enterprise): Path<String>,
    body: Body,
) -> ScimResult<Response> {
    let a = enterprise_auth(&state, &auth).await?;
    let t = Tenant::Enterprise;
    create(
        &state,
        &a,
        &t,
        &t.base(&state, &enterprise),
        &body_value(&body)?,
    )
    .await
}

/// `GET /scim/v2/enterprises/{enterprise}/Users/{scim_user_id}`
pub async fn enterprise_get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((enterprise, id)): Path<(String, String)>,
) -> ScimResult<Response> {
    enterprise_auth(&state, &auth).await?;
    let t = Tenant::Enterprise;
    let row = load(&state, &t, &id).await?;
    Ok(scim_json(
        StatusCode::OK,
        resource(&t.base(&state, &enterprise), &row, true),
    ))
}

/// `PUT /scim/v2/enterprises/{enterprise}/Users/{scim_user_id}`
pub async fn enterprise_put(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((enterprise, id)): Path<(String, String)>,
    body: Body,
) -> ScimResult<Response> {
    let a = enterprise_auth(&state, &auth).await?;
    let t = Tenant::Enterprise;
    let f = Fields::from_resource(&body_value(&body)?)?;
    update(&state, &a, &t, &t.base(&state, &enterprise), &id, f).await
}

/// `PATCH /scim/v2/enterprises/{enterprise}/Users/{scim_user_id}`
pub async fn enterprise_patch(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((enterprise, id)): Path<(String, String)>,
    body: Body,
) -> ScimResult<Response> {
    let a = enterprise_auth(&state, &auth).await?;
    let t = Tenant::Enterprise;
    patch(
        &state,
        &a,
        &t,
        &t.base(&state, &enterprise),
        &id,
        &body_value(&body)?,
    )
    .await
}

/// `DELETE /scim/v2/enterprises/{enterprise}/Users/{scim_user_id}`
pub async fn enterprise_delete(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((_enterprise, id)): Path<(String, String)>,
) -> ScimResult<Response> {
    let a = enterprise_auth(&state, &auth).await?;
    delete(&state, &a, &Tenant::Enterprise, &id).await
}

/// `GET /scim/v2/organizations/{org}/Users`
pub async fn org_list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
    Query(q): Query<ListQuery>,
) -> ScimResult<Response> {
    let (_, org) = org_auth(&state, &auth, &org).await?;
    let t = Tenant::Org(Box::new(org));
    list_users(&state, &t, &t.base(&state, ""), &q).await
}

/// `POST /scim/v2/organizations/{org}/Users`
pub async fn org_create(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path(org): Path<String>,
    body: Body,
) -> ScimResult<Response> {
    let (a, org) = org_auth(&state, &auth, &org).await?;
    let t = Tenant::Org(Box::new(org));
    create(&state, &a, &t, &t.base(&state, ""), &body_value(&body)?).await
}

/// `GET /scim/v2/organizations/{org}/Users/{scim_user_id}`
pub async fn org_get(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, String)>,
) -> ScimResult<Response> {
    let (_, org) = org_auth(&state, &auth, &org).await?;
    let t = Tenant::Org(Box::new(org));
    let row = load(&state, &t, &id).await?;
    Ok(scim_json(
        StatusCode::OK,
        resource(&t.base(&state, ""), &row, false),
    ))
}

/// `PUT /scim/v2/organizations/{org}/Users/{scim_user_id}`
pub async fn org_put(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, String)>,
    body: Body,
) -> ScimResult<Response> {
    let (a, org) = org_auth(&state, &auth, &org).await?;
    let t = Tenant::Org(Box::new(org));
    let f = Fields::from_resource(&body_value(&body)?)?;
    update(&state, &a, &t, &t.base(&state, ""), &id, f).await
}

/// `PATCH /scim/v2/organizations/{org}/Users/{scim_user_id}`
pub async fn org_patch(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, String)>,
    body: Body,
) -> ScimResult<Response> {
    let (a, org) = org_auth(&state, &auth, &org).await?;
    let t = Tenant::Org(Box::new(org));
    patch(
        &state,
        &a,
        &t,
        &t.base(&state, ""),
        &id,
        &body_value(&body)?,
    )
    .await
}

/// `DELETE /scim/v2/organizations/{org}/Users/{scim_user_id}`
pub async fn org_delete(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((org, id)): Path<(String, String)>,
) -> ScimResult<Response> {
    let (a, org) = org_auth(&state, &auth, &org).await?;
    delete(&state, &a, &Tenant::Org(Box::new(org)), &id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_fields() {
        let mut f = Fields::from_resource(&json!({
            "userName": "mona",
            "name": {"givenName": "Mona", "familyName": "Octocat"},
            "emails": [{"value": "m@example.com", "type": "work", "primary": true}],
        }))
        .unwrap();
        assert!(f.active);
        assert_eq!(f.name().as_deref(), Some("Mona Octocat"));
        let ops = patch_operations(&json!({"Operations": [
            {"op": "replace", "path": "emails[type eq \"work\"].value", "value": "n@example.com"},
            {"op": "Replace", "value": {"active": "False", "name.givenName": "Nona"}},
            {"op": "add", "path": "roles", "value": [{"value": "Enterprise Owner"}]},
        ]}))
        .unwrap();
        f.patch(&ops).unwrap();
        assert!(!f.active);
        assert_eq!(f.given_name.as_deref(), Some("Nona"));
        assert_eq!(f.primary_email().as_deref(), Some("n@example.com"));
        assert_eq!(f.enterprise_owner(), Some(true));
        let rm = patch_operations(&json!({"Operations": [{"op": "remove", "path": "userName"}]}))
            .unwrap();
        assert!(f.patch(&rm).is_err());
        assert!(Fields::from_resource(&json!({"emails": []})).is_err());
    }
}
