//! Workflow status badge: `GET /{owner}/{repo}/actions/workflows/{file}/badge.svg`
//! (`?branch=`, `?event=`), as embedded in READMEs.
//!
//! Reflects the latest completed run of the workflow on `branch` (default:
//! the repository's default branch), optionally restricted to `event`:
//! "passing", "failing", or "no status" when there is none. Served with
//! no-cache headers so README viewers (and proxies) always refetch it.

use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use serde::Deserialize;

use crate::api::workflows::find_workflow;

#[derive(Debug, Deserialize)]
pub struct BadgeQuery {
    pub branch: Option<String>,
    pub event: Option<String>,
}

/// Badge message and color for a run conclusion (`None`: no run).
pub fn status_of(conclusion: Option<&str>) -> (&'static str, &'static str) {
    match conclusion {
        Some("success") => ("passing", "#2ea44f"),
        Some("failure" | "timed_out" | "startup_failure" | "action_required") => {
            ("failing", "#cb2431")
        }
        Some("cancelled") => ("cancelled", "#6a737d"),
        _ => ("no status", "#6a737d"),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Approximate text width (11px Verdana-ish) for layout.
fn text_width(s: &str) -> u32 {
    s.chars()
        .map(|c| match c {
            'i' | 'l' | 'j' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' | ' ' => 4,
            'm' | 'w' | 'M' | 'W' => 10,
            c if c.is_ascii_uppercase() => 8,
            _ => 7,
        })
        .sum()
}

/// Render a two-part badge (`label | message`).
pub fn render(label: &str, message: &str, color: &str) -> String {
    let lw = text_width(label) + 12;
    let mw = text_width(message) + 12;
    let w = lw + mw;
    let (label, message) = (escape(label), escape(message));
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="20" role="img" aria-label="{label}: {message}"><title>{label}: {message}</title><linearGradient id="s" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/><stop offset="1" stop-opacity=".1"/></linearGradient><clipPath id="r"><rect width="{w}" height="20" rx="3" fill="#fff"/></clipPath><g clip-path="url(#r)"><rect width="{lw}" height="20" fill="#555"/><rect x="{lw}" width="{mw}" height="20" fill="{color}"/><rect width="{w}" height="20" fill="url(#s)"/></g><g fill="#fff" text-anchor="middle" font-family="Verdana,Geneva,DejaVu Sans,sans-serif" font-size="11"><text x="{lx}" y="15" fill="#010101" fill-opacity=".3">{label}</text><text x="{lx}" y="14">{label}</text><text x="{mx}" y="15" fill="#010101" fill-opacity=".3">{message}</text><text x="{mx}" y="14">{message}</text></g></svg>"##,
        lx = lw / 2,
        mx = lw + mw / 2,
    )
}

pub async fn badge(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo, file)): Path<(String, String, String)>,
    Query(q): Query<BadgeQuery>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let wf = find_workflow(&state, &access, &file).await?;
    let branch = q
        .branch
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| access.repo.default_branch.clone());
    let conclusion: Option<Option<String>> = sqlx::query_scalar(
        "SELECT conclusion FROM actions_runs
          WHERE workflow_id = $1 AND head_branch = $2 AND status = 'completed'
            AND ($3::text IS NULL OR event = $3)
          ORDER BY id DESC LIMIT 1",
    )
    .bind(wf.id)
    .bind(&branch)
    .bind(q.event.filter(|e| !e.is_empty()))
    .fetch_optional(&state.db)
    .await?;
    let (message, color) = status_of(conclusion.flatten().as_deref());
    let svg = render(&wf.name, message, color);
    let mut res = svg.into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("image/svg+xml; charset=utf-8"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("max-age=0, no-cache, no-store, must-revalidate, private"),
    );
    h.insert(header::EXPIRES, HeaderValue::from_static("0"));
    Ok(res)
}
