//! Email addresses: `GET|POST|DELETE /user/emails`, `GET /user/public_emails`,
//! `PATCH /user/email/visibility`, and web-client verification
//! (`/_bgh/emails/...`).

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit;
use bgh_core::error::unique_violation;
use bgh_core::mail;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::json::{Email, EmailRow};
use crate::{util, validate};

const VERIFY_TTL_HOURS: i64 = 72;

/// Emails may be given as `{"emails": [...]}`, a bare array or a string.
fn parse_emails(body: Value) -> ApiResult<Vec<String>> {
    let list = match body {
        Value::Object(mut o) => o.remove("emails").unwrap_or(Value::Null),
        other => other,
    };
    let emails: Vec<String> = match list {
        Value::String(s) => vec![s],
        Value::Array(items) => items
            .into_iter()
            .map(|v| match v {
                Value::String(s) => Ok(s),
                _ => Err(ApiError::invalid_field(FieldError::invalid(
                    "User", "emails",
                ))),
            })
            .collect::<ApiResult<_>>()?,
        _ => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "User", "emails",
            )));
        }
    };
    if emails.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "User", "emails",
        )));
    }
    Ok(emails.into_iter().map(|e| e.trim().to_string()).collect())
}

async fn user_emails(state: &AppState, user_id: i64) -> ApiResult<Vec<EmailRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM user_emails WHERE user_id = $1 ORDER BY is_primary DESC, id",
        EmailRow::COLUMNS
    ))
    .bind(user_id)
    .fetch_all(&state.db)
    .await?)
}

/// `GET /user/emails` (scope `user:email`).
pub async fn list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<Email>> {
    auth.require_scope("user:email")?;
    let rows: Vec<EmailRow> = sqlx::query_as(&format!(
        "SELECT {} FROM user_emails WHERE user_id = $1 ORDER BY is_primary DESC, id
          LIMIT $2 OFFSET $3",
        EmailRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|e| Email::from(&e)))
}

/// `GET /user/public_emails` (scope `user:email`).
pub async fn list_public(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
) -> ApiResult<Page<Email>> {
    auth.require_scope("user:email")?;
    let rows: Vec<EmailRow> = sqlx::query_as(&format!(
        "SELECT {} FROM user_emails WHERE user_id = $1 AND visibility = 'public'
          ORDER BY is_primary DESC, id LIMIT $2 OFFSET $3",
        EmailRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(p.limit_plus_one())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(p.page(rows).map(|e| Email::from(&e)))
}

/// Queue a verification mail for `email_id`.
async fn send_verification(
    state: &AppState,
    tx: &mut Tx,
    user: &db::User,
    email_id: i64,
    email: &str,
) -> ApiResult<()> {
    let token = util::create_account_token(
        tx,
        user.id,
        "email_verification",
        Some(email_id),
        chrono::Duration::hours(VERIFY_TTL_HOURS),
    )
    .await?;
    let link = state
        .urls
        .html(&format!("/settings/emails/verify?token={token}"));
    util::queue_mail(
        tx,
        mail::templates::verify_email(
            &state.config.site_name,
            email,
            &user.login,
            &link,
            VERIFY_TTL_HOURS,
        ),
    )
    .await
}

/// `POST /user/emails` (scope `user`) → 201 with the added (unverified)
/// addresses; a verification mail is sent to each.
pub async fn add(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Vec<Email>>)> {
    auth.require_scope("user")?;
    let emails = parse_emails(body)?;
    if let Some(bad) = emails.iter().find(|e| !validate::is_valid_email(e)) {
        return Err(ApiError::invalid_field(FieldError::custom(
            "User",
            "email",
            format!("{bad} is not a valid email address"),
        )));
    }
    let mut tx = Tx::begin(&state).await?;
    let mut out = Vec::new();
    for email in &emails {
        let row: EmailRow = sqlx::query_as(&format!(
            "INSERT INTO user_emails (user_id, email, verified, is_primary)
             VALUES ($1, $2, false, false) RETURNING {}",
            EmailRow::COLUMNS
        ))
        .bind(auth.user.id)
        .bind(email)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match unique_violation(&e).as_deref() {
            Some("user_emails_email_key") => ApiError::invalid_field(FieldError::custom(
                "User",
                "email",
                "email is already in use",
            )),
            _ => e.into(),
        })?;
        send_verification(&state, &mut tx, &auth.user, row.id, &row.email).await?;
        bgh_core::signatures::forget_email(&mut tx, &row.email).await?;
        out.push(Email::from(&row));
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "user.add_email",
        audit::Target::User(auth.user.id),
        json!({ "emails": emails }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)))
}

/// `DELETE /user/emails` (scope `user`) → 204. The primary address can't
/// be deleted.
pub async fn remove(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<Value>,
) -> ApiResult<StatusCode> {
    auth.require_scope("user")?;
    let emails: Vec<String> = parse_emails(body)?
        .into_iter()
        .map(|e| e.to_lowercase())
        .collect();
    let mut tx = Tx::begin(&state).await?;
    let rows: Vec<EmailRow> = sqlx::query_as(&format!(
        "SELECT {} FROM user_emails WHERE user_id = $1 AND lower(email) = ANY($2) FOR UPDATE",
        EmailRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(&emails)
    .fetch_all(&mut *tx)
    .await?;
    if rows.len() != emails.len() {
        return Err(ApiError::NotFound);
    }
    if rows.iter().any(|r| r.is_primary) {
        return Err(ApiError::invalid_field(FieldError::custom(
            "User",
            "email",
            "cannot delete your primary email address",
        )));
    }
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    sqlx::query("DELETE FROM user_emails WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    for r in &rows {
        bgh_core::signatures::forget_email(&mut tx, &r.email).await?;
    }
    // A deleted address can't stay the public profile email.
    let user: db::User = sqlx::query_as(&format!(
        "UPDATE users SET email = NULL, updated_at = now()
          WHERE id = $1 AND lower(email) = ANY($2) RETURNING {}",
        db::User::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(&emails)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_else(|| auth.user.clone());
    if user.email.is_none() && auth.user.email.is_some() {
        util::sync_profile(&mut tx, &user).await?;
    }
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "user.remove_email",
        audit::Target::User(auth.user.id),
        json!({ "emails": emails }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct VisibilityBody {
    pub visibility: Option<String>,
}

/// `PATCH /user/email/visibility` → the user's emails.
pub async fn set_visibility(
    State(state): State<AppState>,
    auth: RequireUser,
    Json(body): Json<VisibilityBody>,
) -> ApiResult<Json<Vec<Email>>> {
    auth.require_scope("user")?;
    let visibility = match body.visibility.as_deref() {
        Some(v @ ("public" | "private")) => v.to_string(),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "User",
                "visibility",
            )));
        }
        None => {
            return Err(ApiError::invalid_field(FieldError::missing_field(
                "User",
                "visibility",
            )));
        }
    };
    let mut tx = Tx::begin(&state).await?;
    let primary: Option<String> = sqlx::query_scalar(
        "UPDATE user_emails SET visibility = $2 WHERE user_id = $1 AND is_primary RETURNING email",
    )
    .bind(auth.user.id)
    .bind(&visibility)
    .fetch_optional(&mut *tx)
    .await?;
    let public = if visibility == "public" {
        primary
    } else {
        None
    };
    let user: db::User = sqlx::query_as(&format!(
        "UPDATE users SET email = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        db::User::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(&public)
    .fetch_one(&mut *tx)
    .await?;
    util::sync_profile(&mut tx, &user).await?;
    tx.commit().await?;
    let rows = user_emails(&state, auth.user.id).await?;
    Ok(Json(rows.iter().map(Email::from).collect()))
}

// ---------------------------------------------------------------------------
// Web client
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct VerifyBody {
    #[serde(default)]
    pub token: String,
}

/// `POST /_bgh/emails/verify {token}` → 200 with the verified email. No
/// authentication needed: the mailed token is the proof.
pub async fn verify(
    State(state): State<AppState>,
    Json(body): Json<VerifyBody>,
) -> ApiResult<Json<Email>> {
    let mut tx = Tx::begin(&state).await?;
    let (token_id, _user_id, email_id) =
        util::find_account_token(&mut *tx, "email_verification", body.token.trim())
            .await?
            .ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM account_tokens WHERE id = $1")
        .bind(token_id)
        .execute(&mut *tx)
        .await?;
    let row: EmailRow = sqlx::query_as(&format!(
        "UPDATE user_emails SET verified = true WHERE id = $1 RETURNING {}",
        EmailRow::COLUMNS
    ))
    .bind(email_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    bgh_core::signatures::forget_email(&mut tx, &row.email).await?;
    tx.commit().await?;
    Ok(Json(Email::from(&row)))
}

/// `POST /_bgh/user/emails/{email}/verification` → 202, re-sends the mail.
pub async fn resend_verification(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(email): Path<String>,
) -> ApiResult<StatusCode> {
    util::require_session(&auth)?;
    let key = format!("verify_mail:{}", auth.user.id);
    if bgh_core::ratelimit::hit(&state, &key, 3600).await? > 10 {
        return Err(ApiError::Status(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many verification emails. Please try again later.".into(),
        ));
    }
    let row: EmailRow = sqlx::query_as(&format!(
        "SELECT {} FROM user_emails WHERE user_id = $1 AND lower(email) = lower($2)",
        EmailRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(&email)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if row.verified {
        return Err(ApiError::unprocessable("Email is already verified"));
    }
    let mut tx = Tx::begin(&state).await?;
    send_verification(&state, &mut tx, &auth.user, row.id, &row.email).await?;
    tx.commit().await?;
    Ok(StatusCode::ACCEPTED)
}

/// `PUT /_bgh/user/emails/{email}/primary` → 200 emails; the address must
/// be verified.
pub async fn set_primary(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(email): Path<String>,
) -> ApiResult<Json<Vec<Email>>> {
    util::require_session(&auth)?;
    let mut tx = Tx::begin(&state).await?;
    let row: EmailRow = sqlx::query_as(&format!(
        "SELECT {} FROM user_emails WHERE user_id = $1 AND lower(email) = lower($2) FOR UPDATE",
        EmailRow::COLUMNS
    ))
    .bind(auth.user.id)
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    if !row.verified {
        return Err(ApiError::unprocessable(
            "Only verified email addresses can be primary",
        ));
    }
    if !row.is_primary {
        let visibility: Option<String> = sqlx::query_scalar(
            "UPDATE user_emails SET is_primary = false, visibility = NULL
              WHERE user_id = $1 AND is_primary RETURNING coalesce(visibility, 'private')",
        )
        .bind(auth.user.id)
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query("UPDATE user_emails SET is_primary = true, visibility = $2 WHERE id = $1")
            .bind(row.id)
            .bind(visibility.unwrap_or_else(|| "private".into()))
            .execute(&mut *tx)
            .await?;
        audit::log(
            &mut *tx,
            Some(&auth.user),
            "user.change_primary_email",
            audit::Target::User(auth.user.id),
            json!({ "email": row.email }),
        )
        .await?;
    }
    tx.commit().await?;
    let rows = user_emails(&state, auth.user.id).await?;
    Ok(Json(rows.iter().map(Email::from).collect()))
}
