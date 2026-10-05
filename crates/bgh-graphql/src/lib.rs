//! bgh-graphql: GitHub GraphQL v4 compatible subset at `/api/graphql`.
//!
//! Covers what the `gh` CLI (and similar clients) use: `viewer`,
//! `repository` (issues, pull requests, labels, milestones, refs, releases,
//! ...), `node`/`nodes`, `search`, `rateLimit`, and the issue / pull request
//! / repository mutations. Reads go straight to the core tables through
//! per-request DataLoaders (no N+1); writes call the owning domain crates'
//! service functions so business rules, sync records and events stay in one
//! place.
//!
//! See `docs/packages/graphql.md` for the covered surface.

mod conn;
mod ctx;
mod loaders;
mod model;
mod mutation;
mod query;
mod scalars;
mod search;

use std::sync::OnceLock;

use async_graphql::{EmptySubscription, Schema};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgh_core::prelude::*;
use serde_json::{Value, json};

pub use ctx::Gql;

pub type BghSchema = Schema<query::Query, mutation::Mutation, EmptySubscription>;

/// The schema (built once).
pub fn schema() -> &'static BghSchema {
    static SCHEMA: OnceLock<BghSchema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        Schema::build(
            query::Query::default(),
            mutation::Mutation::default(),
            EmptySubscription,
        )
        .register_output_type::<model::Node>()
        .register_output_type::<model::Actor>()
        .register_output_type::<model::RepositoryOwner>()
        .limit_depth(32)
        .limit_recursive_depth(64)
        .finish()
    })
}

/// The schema in SDL (for docs and tests).
pub fn sdl() -> String {
    schema().sdl()
}

/// REST API routes (none: GraphQL lives outside `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new()
}

/// `/api/graphql` (POST for queries and mutations, GET for queries).
pub fn web_router() -> Router<AppState> {
    Router::new().route("/api/graphql", get(get_graphql).post(post_graphql))
}

/// Register background job handlers and event listeners.
pub fn register(_reg: &mut Registry) {}

async fn post_graphql(
    State(state): State<AppState>,
    auth: MaybeUser,
    body: Bytes,
) -> Response {
    // Cookie-authenticated POSTs are CSRF-checked by the server-wide
    // middleware (bgh_core::auth::csrf_middleware).
    let request: async_graphql::Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return problems_parsing(&e.to_string()),
    };
    execute(state, auth.0, request, Method::POST).await
}

async fn get_graphql(
    State(state): State<AppState>,
    auth: MaybeUser,
    RawQuery(query): RawQuery,
) -> Response {
    let params: Vec<(String, String)> = query
        .as_deref()
        .map(|q| {
            q.split('&')
                .filter_map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    Some((decode_component(k)?, decode_component(v)?))
                })
                .collect()
        })
        .unwrap_or_default();
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let Some(q) = get("query") else {
        return problems_parsing("A query attribute must be specified and must be a string.");
    };
    let mut request = async_graphql::Request::new(q);
    if let Some(op) = get("operationName") {
        request = request.operation_name(op);
    }
    if let Some(vars) = get("variables").filter(|v| !v.is_empty()) {
        match serde_json::from_str::<Value>(&vars) {
            Ok(v) => request = request.variables(async_graphql::Variables::from_json(v)),
            Err(e) => return problems_parsing(&e.to_string()),
        }
    }
    execute(state, auth.0, request, Method::GET).await
}

async fn execute(
    state: AppState,
    auth: Option<AuthContext>,
    request: async_graphql::Request,
    method: Method,
) -> Response {
    let loaders = loaders::Loaders::new(&state, auth.as_ref());
    let mut request = request.data(Gql { state, auth }).data(loaders);
    if method == Method::GET {
        request = request.data(mutation::ReadOnly);
    }
    let response = schema().execute(request).await;
    let mut body = serde_json::to_value(&response).unwrap_or_else(|_| json!({}));
    github_errors(&mut body);
    let mut resp = axum::Json(body).into_response();
    let h = resp.headers_mut();
    h.insert("x-github-media-type", HeaderValue::from_static("github.v4"));
    h.insert("x-ratelimit-limit", HeaderValue::from_static("5000"));
    h.insert("x-ratelimit-remaining", HeaderValue::from_static("4999"));
    h.insert("x-ratelimit-used", HeaderValue::from_static("1"));
    h.insert("x-ratelimit-resource", HeaderValue::from_static("graphql"));
    resp
}

/// GitHub puts the error class in a top-level `type` key (`NOT_FOUND`,
/// `FORBIDDEN`, ...), and `gh` matches on it.
fn github_errors(body: &mut Value) {
    let Some(errors) = body.get_mut("errors").and_then(Value::as_array_mut) else {
        return;
    };
    for e in errors {
        let Some(obj) = e.as_object_mut() else {
            continue;
        };
        let ty = obj
            .get_mut("extensions")
            .and_then(Value::as_object_mut)
            .and_then(|ext| ext.remove("type"));
        if obj
            .get("extensions")
            .and_then(Value::as_object)
            .is_some_and(|m| m.is_empty())
        {
            obj.remove("extensions");
        }
        if let Some(ty) = ty {
            obj.insert("type".into(), ty);
        }
    }
}

fn problems_parsing(detail: &str) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({
            "message": format!("Problems parsing JSON: {detail}"),
            "documentation_url": "https://docs.github.com/graphql",
        })),
    )
        .into_response()
}

fn decode_component(s: &str) -> Option<String> {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
