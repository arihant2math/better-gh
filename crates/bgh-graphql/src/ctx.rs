//! Per-request GraphQL context and error mapping.

use async_graphql::{Context, ErrorExtensions};
use bgh_core::auth::AuthContext;
use bgh_core::error::ApiError;
use bgh_core::state::AppState;

/// Request-scoped data available to every resolver.
pub struct Gql {
    pub state: AppState,
    pub auth: Option<AuthContext>,
    /// Client IP (rate-limit identity of anonymous callers).
    pub client_ip: String,
}

impl Gql {
    pub fn viewer_id(&self) -> Option<i64> {
        self.auth.as_ref().map(|a| a.user.id)
    }

    /// The authenticated caller, or a GitHub-style "must be authenticated"
    /// error.
    pub fn require_auth(&self) -> async_graphql::Result<&AuthContext> {
        self.auth.as_ref().ok_or_else(|| {
            err(
                "FORBIDDEN",
                "This endpoint requires you to be authenticated.",
            )
        })
    }
}

/// Shortcut: `let g = gql(ctx);`
pub fn gql<'a>(ctx: &Context<'a>) -> &'a Gql {
    ctx.data_unchecked::<Gql>()
}

pub type GResult<T> = async_graphql::Result<T>;

/// A GraphQL error with a GitHub error `type` (`NOT_FOUND`, `FORBIDDEN`,
/// `UNPROCESSABLE`, ...). The handler lifts `extensions.type` to the
/// top-level `type` key GitHub uses.
pub fn err(ty: &str, message: impl Into<String>) -> async_graphql::Error {
    async_graphql::Error::new(message).extend_with(|_, e| e.set("type", ty))
}

pub fn not_found(message: impl Into<String>) -> async_graphql::Error {
    err("NOT_FOUND", message)
}

/// Map a REST-layer error onto a GraphQL error.
pub fn api_err(e: ApiError) -> async_graphql::Error {
    let ty = match &e {
        ApiError::NotFound => "NOT_FOUND",
        ApiError::Forbidden(_) => "FORBIDDEN",
        ApiError::Unauthorized { .. } => "FORBIDDEN",
        ApiError::Validation { .. } | ApiError::BadRequest(_) | ApiError::Conflict(_) => {
            "UNPROCESSABLE"
        }
        ApiError::Gone(_) => "UNPROCESSABLE",
        ApiError::Status(..) => "UNPROCESSABLE",
        ApiError::Internal(cause) => {
            tracing::error!(error = ?cause, "graphql resolver error");
            return err(
                "INTERNAL",
                "Something went wrong while executing your query.",
            );
        }
    };
    let message = match &e {
        ApiError::NotFound => "Could not resolve to a node.".to_string(),
        ApiError::Validation { message, errors } if !errors.is_empty() => {
            let details: Vec<String> = errors
                .iter()
                .map(|f| {
                    f.message
                        .clone()
                        .unwrap_or_else(|| format!("{} {} {}", f.resource, f.field, f.code))
                })
                .collect();
            format!("{message}: {}", details.join(", "))
        }
        other => other.to_string(),
    };
    err(ty, message)
}

/// `?`-friendly conversion of anything that converts into [`ApiError`].
pub trait OrGql<T> {
    fn gql(self) -> GResult<T>;
}

impl<T, E: Into<ApiError>> OrGql<T> for Result<T, E> {
    fn gql(self) -> GResult<T> {
        self.map_err(|e| api_err(e.into()))
    }
}
