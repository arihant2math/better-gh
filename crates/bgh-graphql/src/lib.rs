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

// GraphQL resolvers take one Rust argument per GraphQL argument.
#![allow(clippy::too_many_arguments)]

mod conn;
mod cost;
mod ctx;
mod guard;
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
use axum::http::request::Parts;
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
            query::Query,
            mutation::Mutation::default(),
            EmptySubscription,
        )
        .register_output_type::<model::Node>()
        .register_output_type::<model::Actor>()
        .register_output_type::<model::RepositoryOwner>()
        .limit_depth(32)
        .limit_recursive_depth(64)
        .extension(cost::CostLimit)
        .finish()
    })
}

/// The schema in SDL (for docs and tests).
pub fn sdl() -> String {
    schema().sdl()
}

/// GHES version reported by `GET /meta`. Clients (`gh`) gate GraphQL
/// feature detection on it; 3.17 selects the classic issue search syntax and
/// no classic projects.
pub const COMPAT_GHES_VERSION: &str = "3.17.0";

/// REST API routes: `GET /meta` (GraphQL itself lives outside `/api/v3`).
pub fn router() -> Router<AppState> {
    Router::new().route("/meta", get(meta))
}

/// `GET /meta` (GitHub Enterprise Server shape), including the SSH host
/// keys (`ssh_keys`, `ssh_key_fingerprints.SHA256_<ALG>`) for pinning
/// `known_hosts`.
async fn meta(State(state): State<AppState>) -> axum::Json<Value> {
    let host_keys = bgh_repos::ssh::host_public_keys(&state);
    let fingerprints: serde_json::Map<String, Value> = host_keys
        .iter()
        .map(|(alg, _, fp)| (format!("SHA256_{alg}"), Value::from(fp.as_str())))
        .collect();
    let ssh_keys: Vec<&str> = host_keys.iter().map(|(_, k, _)| k.as_str()).collect();
    axum::Json(json!({
        "verifiable_password_authentication": false,
        "installed_version": COMPAT_GHES_VERSION,
        "bgh_version": env!("CARGO_PKG_VERSION"),
        "ssh_key_fingerprints": fingerprints,
        "ssh_keys": ssh_keys,
        "hooks": [], "web": [], "api": [], "git": [], "packages": [],
        "pages": [], "importer": [], "actions": [], "dependabot": [],
    }))
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
    parts: Parts,
    body: Bytes,
) -> Response {
    // Cookie-authenticated POSTs are CSRF-checked by the server-wide
    // middleware (bgh_core::auth::csrf_middleware).
    let request: async_graphql::Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return problems_parsing(&e.to_string()),
    };
    let ip = client_ip(&state, &parts);
    execute(state, auth.0, ip, request, Method::POST).await
}

async fn get_graphql(
    State(state): State<AppState>,
    auth: MaybeUser,
    parts: Parts,
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
    let ip = client_ip(&state, &parts);
    execute(state, auth.0, ip, request, Method::GET).await
}

async fn execute(
    state: AppState,
    auth: Option<AuthContext>,
    client_ip: String,
    request: async_graphql::Request,
    method: Method,
) -> Response {
    // Every query reaches the parser through here (POST and GET; there is
    // no batching or subscription transport). Variables are already bounded
    // by serde_json's recursion limit (128) when the request is decoded.
    if let Err(rejection) = guard::check(&request.query) {
        let (ty, message) = rejection.error();
        let mut ext = async_graphql::ErrorExtensionValues::default();
        ext.set("type", ty);
        let mut err = async_graphql::ServerError::new(message, None);
        err.extensions = Some(ext);
        return respond(async_graphql::Response::from_errors(vec![err]), None);
    }
    let loaders = loaders::Loaders::new(&state, auth.as_ref());
    let cost = std::sync::Arc::new(cost::CostCell::default());
    let mut request = request
        .data(Gql {
            state,
            auth,
            client_ip,
        })
        .data(loaders)
        .data(cost.clone());
    if method == Method::GET {
        request = request.data(mutation::ReadOnly);
    }
    let response = schema().execute(request).await;
    respond(response, cost.quota())
}

fn respond(
    response: async_graphql::Response,
    quota: Option<bgh_core::ratelimit::Quota>,
) -> Response {
    let mut body = serde_json::to_value(&response).unwrap_or_else(|_| json!({}));
    github_errors(&mut body);
    let mut resp = axum::Json(body).into_response();
    let h = resp.headers_mut();
    h.insert("x-github-media-type", HeaderValue::from_static("github.v4"));
    if let Some(q) = quota {
        q.apply(h);
    }
    resp
}

fn client_ip(state: &AppState, parts: &Parts) -> String {
    bgh_core::auth::client_ip(&state.config, &parts.headers, &parts.extensions)
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
