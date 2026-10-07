//! GitHub-style API errors.
//!
//! Every handler returns [`ApiResult<T>`]. Errors render as
//! `{"message": "...", "documentation_url": "...", "status": "404"}` plus an
//! `errors` array for validation failures, matching GitHub's REST API.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

pub type ApiResult<T, E = ApiError> = Result<T, E>;

const DOCS_URL: &str = "https://docs.github.com/rest";

/// One entry of the `errors` array of a 422 response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub resource: String,
    pub field: String,
    /// GitHub codes: `missing`, `missing_field`, `invalid`, `already_exists`,
    /// `unprocessable`, `custom`.
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl FieldError {
    pub fn new(resource: &str, field: &str, code: &str) -> Self {
        Self {
            resource: resource.into(),
            field: field.into(),
            code: code.into(),
            message: None,
        }
    }

    pub fn missing_field(resource: &str, field: &str) -> Self {
        Self::new(resource, field, "missing_field")
    }

    pub fn invalid(resource: &str, field: &str) -> Self {
        Self::new(resource, field, "invalid")
    }

    pub fn already_exists(resource: &str, field: &str) -> Self {
        Self::new(resource, field, "already_exists")
    }

    /// `code: custom` with a human-readable message.
    pub fn custom(resource: &str, field: &str, message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::new(resource, field, "custom")
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 400
    #[error("{0}")]
    BadRequest(String),
    /// 401. `www_authenticate` is set for git transport challenges.
    #[error("{message}")]
    Unauthorized {
        message: String,
        www_authenticate: Option<String>,
    },
    /// 403
    #[error("{0}")]
    Forbidden(String),
    /// 404
    #[error("Not Found")]
    NotFound,
    /// 409
    #[error("{0}")]
    Conflict(String),
    /// 410
    #[error("{0}")]
    Gone(String),
    /// 422
    #[error("{message}")]
    Validation {
        message: String,
        errors: Vec<FieldError>,
    },
    /// Any other status with a message.
    #[error("{1}")]
    Status(StatusCode, String),
    /// 500. The cause is logged, never sent to the client.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn not_found() -> Self {
        Self::NotFound
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    /// 401 "Requires authentication".
    pub fn requires_auth() -> Self {
        Self::Unauthorized {
            message: "Requires authentication".into(),
            www_authenticate: None,
        }
    }

    /// 401 "Bad credentials".
    pub fn bad_credentials() -> Self {
        Self::Unauthorized {
            message: "Bad credentials".into(),
            www_authenticate: None,
        }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    /// 422 "Validation Failed" with field errors.
    pub fn validation(errors: Vec<FieldError>) -> Self {
        Self::Validation {
            message: "Validation Failed".into(),
            errors,
        }
    }

    /// 422 with a single field error.
    pub fn invalid_field(err: FieldError) -> Self {
        Self::validation(vec![err])
    }

    /// 422 with a custom message and no field errors.
    pub fn unprocessable(msg: impl Into<String>) -> Self {
        Self::Validation {
            message: msg.into(),
            errors: vec![],
        }
    }

    pub fn internal(err: impl Into<anyhow::Error>) -> Self {
        Self::Internal(err.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Gone(_) => StatusCode::GONE,
            Self::Validation { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Status(s, _) => *s,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<&'a [FieldError]>,
    documentation_url: &'a str,
    status: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message: String = match &self {
            Self::Internal(err) => {
                tracing::error!(error = ?err, "internal server error");
                "Internal Server Error".into()
            }
            other => other.to_string(),
        };
        let errors = match &self {
            Self::Validation { errors, .. } if !errors.is_empty() => Some(errors.as_slice()),
            _ => None,
        };
        let body = ErrorBody {
            message: &message,
            errors,
            documentation_url: DOCS_URL,
            status: status.as_u16().to_string(),
        };
        let mut resp = (status, Json(body)).into_response();
        if let Self::Unauthorized {
            www_authenticate: Some(challenge),
            ..
        } = &self
            && let Ok(v) = HeaderValue::from_str(challenge)
        {
            resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
        }
        resp
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Internal(other.into()),
        }
    }
}

impl From<redis::RedisError> for ApiError {
    fn from(err: redis::RedisError) -> Self {
        Self::Internal(err.into())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(err: tokio::task::JoinError) -> Self {
        Self::Internal(err.into())
    }
}

/// If `err` is a Postgres unique violation, return the violated constraint
/// (index) name. Use it to turn races into 422 `already_exists`.
pub fn unique_violation(err: &sqlx::Error) -> Option<String> {
    match err {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            Some(db.constraint().unwrap_or_default().to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn renders_validation_error() {
        let resp = ApiError::invalid_field(FieldError::missing_field("Repository", "name"))
            .into_response();
        assert_eq!(resp.status(), 422);
        let v = body_json(resp).await;
        assert_eq!(v["message"], "Validation Failed");
        assert_eq!(v["status"], "422");
        assert_eq!(v["errors"][0]["resource"], "Repository");
        assert_eq!(v["errors"][0]["code"], "missing_field");
        assert!(v["errors"][0].get("message").is_none());
    }

    #[tokio::test]
    async fn renders_not_found_without_errors() {
        let v = body_json(ApiError::NotFound.into_response()).await;
        assert_eq!(v["message"], "Not Found");
        assert!(v.get("errors").is_none());
    }

    #[tokio::test]
    async fn hides_internal_errors() {
        let resp = ApiError::internal(anyhow::anyhow!("secret detail")).into_response();
        assert_eq!(resp.status(), 500);
        let v = body_json(resp).await;
        assert_eq!(v["message"], "Internal Server Error");
    }
}
