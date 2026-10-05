//! Request extractors whose rejections render as GitHub-style errors.
//!
//! Use these instead of the axum originals in handlers:
//! * [`Json`]: accepts any content type (like GitHub), empty body = `{}`,
//!   syntax errors → 400 "Problems parsing JSON", type errors → 422.
//! * [`Query`]: invalid query → 422.
//! * [`Path`]: invalid path params → 404.

use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// JSON request body / response body.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<S, T> FromRequest<S> for Json<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        parse_json(&bytes).map(Json)
    }
}

/// Parse a JSON body with GitHub's error semantics.
pub fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let input: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        bytes
    };
    serde_json::from_slice(input).map_err(|e| {
        use serde_json::error::Category;
        match e.classify() {
            Category::Data => ApiError::unprocessable(format!("Invalid request.\n\n{e}")),
            _ => ApiError::bad_request("Problems parsing JSON"),
        }
    })
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Query string parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Query<T>(pub T);

impl<S, T> FromRequestParts<S> for Query<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        axum::extract::Query::<T>::try_from_uri(&parts.uri)
            .map(|q| Query(q.0))
            .map_err(|e| ApiError::unprocessable(format!("Invalid request.\n\n{}", e.body_text())))
    }
}

/// Path parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct Path<T>(pub T);

impl<S, T> FromRequestParts<S> for Path<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|p| Path(p.0))
            .map_err(|_| ApiError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Debug)]
    struct Body {
        #[serde(default)]
        name: Option<String>,
    }

    #[test]
    fn json_errors() {
        assert!(parse_json::<Body>(b"").unwrap().name.is_none());
        let err = parse_json::<Body>(b"{bad").unwrap_err();
        assert_eq!(err.status(), 400);
        let err = parse_json::<Body>(br#"{"name": 1}"#).unwrap_err();
        assert_eq!(err.status(), 422);
    }
}
