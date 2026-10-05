//! `GET /_bgh/repos/{owner}/{repo}/refs`: compact branch + tag list for the
//! ref picker.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use bgh_core::prelude::*;
use serde::Serialize;

use super::{Target, json_response};

#[derive(Debug, Serialize)]
pub struct RefItem {
    pub name: String,
    /// Commit the ref points to (tags are peeled).
    pub sha: String,
}

#[derive(Debug, Serialize)]
pub struct Refs {
    pub default_branch: String,
    pub branches: Vec<RefItem>,
    pub tags: Vec<RefItem>,
}

pub async fn list(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
    req: HeaderMap,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let store = crate::store(&state);
    let (branches, mut tags) = store
        .read(access.repo.id, |r| Ok((r.branches()?, r.tags()?)))
        .await?;
    // Newest-looking tags first (version-aware descending), like GitHub.
    tags.sort_by(|a, b| version_cmp(b.short_name(), a.short_name()));
    let item = |r: &bgh_git::RefInfo| RefItem {
        name: r.short_name().to_string(),
        sha: r.peeled.clone(),
    };
    let body = Refs {
        default_branch: access.repo.default_branch.clone(),
        branches: branches.iter().map(item).collect(),
        tags: tags.iter().map(item).collect(),
    };
    let t = Target {
        access,
        refname: String::new(),
        commit: String::new(),
        path: String::new(),
    };
    json_response(&req, &t, "", &body)
}

/// Compare names treating digit runs numerically (`v1.10` > `v1.9`).
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (da, db) = (&a[..na], &b[..nb]);
                let ta = da.iter().skip_while(|&&c| c == b'0').count();
                let tb = db.iter().skip_while(|&&c| c == b'0').count();
                let ord = ta.cmp(&tb).then_with(|| da[na - ta..].cmp(&db[nb - tb..]));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[na..];
                b = &b[nb..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::version_cmp;
    use std::cmp::Ordering::*;

    #[test]
    fn versions() {
        assert_eq!(version_cmp("v1.10", "v1.9"), Greater);
        assert_eq!(version_cmp("v1.2", "v1.2"), Equal);
        assert_eq!(version_cmp("v2", "v10"), Less);
        assert_eq!(version_cmp("a", "b"), Less);
    }
}
