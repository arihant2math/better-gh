//! `GET /repos/{owner}/{repo}/community/profile` (package P31): community
//! health files of the default branch, looked up like GitHub in the root,
//! `.github/` and `docs/` (README also in the root only for health).
//!
//! License detection belongs to P33: until a repository carries a detected
//! `license_spdx_id`, a present LICENSE/COPYING file reports GitHub's
//! "Other" license (`NOASSERTION`). Besides GitHub's `files` keys the
//! object carries `security` (SECURITY.md), used by the Insights checklist.

use axum::Router;
use axum::extract::State;
use axum::routing::get;
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use bgh_git::TreeEntryKind;
use serde::{Deserialize, Serialize};

use crate::cache;

pub fn routes() -> Router<AppState> {
    Router::new().route("/repos/{owner}/{repo}/community/profile", get(profile))
}

/// Paths (relative to the repository root) of the health files found.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Found {
    readme: Option<String>,
    code_of_conduct: Option<String>,
    /// The code of conduct is the Contributor Covenant.
    covenant: bool,
    contributing: Option<String>,
    license: Option<String>,
    issue_template: Option<String>,
    pull_request_template: Option<String>,
    security: Option<String>,
}

#[derive(Serialize)]
struct FileLink {
    url: String,
    html_url: String,
}

#[derive(Serialize)]
struct CodeOfConduct {
    key: String,
    name: String,
    url: Option<String>,
    html_url: Option<String>,
}

#[derive(Serialize)]
struct License {
    key: String,
    name: String,
    spdx_id: String,
    url: Option<String>,
    node_id: String,
    html_url: String,
}

#[derive(Serialize)]
struct Files {
    code_of_conduct: Option<CodeOfConduct>,
    code_of_conduct_file: Option<FileLink>,
    contributing: Option<FileLink>,
    issue_template: Option<FileLink>,
    pull_request_template: Option<FileLink>,
    license: Option<License>,
    readme: Option<FileLink>,
    security: Option<FileLink>,
}

#[derive(Serialize)]
struct Profile {
    health_percentage: i64,
    description: Option<String>,
    documentation: Option<String>,
    files: Files,
    updated_at: Option<Timestamp>,
    content_reports_enabled: bool,
}

/// Case-insensitive stem match: `name` is `stem` or `stem.<ext>`.
fn is_named(name: &str, stem: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == stem || n.strip_prefix(stem).is_some_and(|r| r.starts_with('.'))
}

struct Dir {
    prefix: &'static str,
    entries: Vec<bgh_git::ops::LsTreeEntry>,
}

impl Dir {
    fn file(&self, stems: &[&str]) -> Option<String> {
        stems.iter().find_map(|stem| {
            self.entries
                .iter()
                .find(|e| e.kind != TreeEntryKind::Tree && is_named(&e.path, stem))
                .map(|e| format!("{}{}", self.prefix, e.path))
        })
    }

    fn dir(&self, name: &str) -> Option<&bgh_git::ops::LsTreeEntry> {
        self.entries
            .iter()
            .find(|e| e.kind == TreeEntryKind::Tree && e.path.eq_ignore_ascii_case(name))
    }
}

async fn scan(git: &bgh_git::ops::GitCli, tree: &str) -> ApiResult<Found> {
    let root = Dir {
        prefix: "",
        entries: git.ls_tree(tree, false).await?,
    };
    let mut dirs = vec![];
    let mut issue_dir = None;
    if let Some(gh) = root.dir(".github") {
        let d = Dir {
            prefix: ".github/",
            entries: git.ls_tree(&gh.sha, false).await?,
        };
        if let Some(t) = d.dir("ISSUE_TEMPLATE") {
            let entries = git.ls_tree(&t.sha, false).await?;
            issue_dir = entries
                .iter()
                .filter(|e| e.kind != TreeEntryKind::Tree)
                .map(|e| e.path.clone())
                .filter(|p| {
                    let p = p.to_ascii_lowercase();
                    (p.ends_with(".md") || p.ends_with(".yml") || p.ends_with(".yaml"))
                        && p != "config.yml"
                        && p != "config.yaml"
                })
                .min()
                .map(|p| format!(".github/{}/{p}", t.path));
        }
        dirs.push(d);
    }
    if let Some(docs) = root.dir("docs") {
        dirs.push(Dir {
            prefix: "docs/",
            entries: git.ls_tree(&docs.sha, false).await?,
        });
    }
    // GitHub's precedence: .github/, root, docs/.
    let (gh, docs): (Vec<&Dir>, Vec<&Dir>) = dirs.iter().partition(|d| d.prefix == ".github/");
    let ordered: Vec<&Dir> = gh
        .into_iter()
        .chain(std::iter::once(&root))
        .chain(docs)
        .collect();
    let any = |stems: &[&str]| ordered.iter().find_map(|d| d.file(stems));

    let code_of_conduct = any(&["code_of_conduct", "code-of-conduct"]);
    // Read through the tree we scanned (`<tree>:<path>`).
    let covenant = match &code_of_conduct {
        Some(p) => git
            .run(&["cat-file", "-p", &format!("{tree}:{p}")], &[], None)
            .await
            .map(|b| {
                String::from_utf8_lossy(&b[..b.len().min(8192)])
                    .to_ascii_lowercase()
                    .contains("contributor covenant")
            })
            .unwrap_or(false),
        None => false,
    };
    Ok(Found {
        readme: any(&["readme"]),
        code_of_conduct,
        covenant,
        contributing: any(&["contributing"]),
        license: root.file(&["license", "licence", "copying"]),
        issue_template: issue_dir.or_else(|| any(&["issue_template"])),
        pull_request_template: any(&["pull_request_template"]),
        security: any(&["security"]),
    })
}

/// `GET /repos/{owner}/{repo}/community/profile`
async fn profile(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let git = crate::store(&state).cli(access.repo.id)?;
    let branch = access.repo.default_branch.clone();
    let found = match git.resolve_commit(&branch).await? {
        None => Found::default(),
        Some(head) => {
            let tree = git.commit(&head).await?.tree;
            cache::cached(&state, &format!("community:v1:{tree}"), || async {
                scan(&git, &tree).await
            })
            .await?
        }
    };
    let (o, r) = (&access.owner.login, &access.repo.name);
    let link = |p: &Option<String>| {
        p.as_ref().map(|p| FileLink {
            url: state
                .urls
                .api(&format!("/repos/{o}/{r}/contents/{}", encode_path(p))),
            html_url: state.urls.html(&format!(
                "/{o}/{r}/blob/{}/{}",
                encode_path(&branch),
                encode_path(p)
            )),
        })
    };
    let code_of_conduct = found.code_of_conduct.as_ref().map(|p| {
        let html_url = link(&Some(p.clone())).map(|l| l.html_url);
        if found.covenant {
            CodeOfConduct {
                key: "contributor_covenant".into(),
                name: "Contributor Covenant".into(),
                url: Some(state.urls.api("/codes_of_conduct/contributor_covenant")),
                html_url,
            }
        } else {
            CodeOfConduct {
                key: "other".into(),
                name: "Other".into(),
                url: None,
                html_url,
            }
        }
    });
    let license = found.license.as_ref().map(|p| {
        let html_url = link(&Some(p.clone()))
            .map(|l| l.html_url)
            .unwrap_or_default();
        match access.repo.license_spdx_id.as_deref() {
            Some(spdx) if spdx != "NOASSERTION" => License {
                key: spdx.to_ascii_lowercase(),
                name: spdx.to_string(),
                spdx_id: spdx.to_string(),
                url: Some(
                    state
                        .urls
                        .api(&format!("/licenses/{}", spdx.to_ascii_lowercase())),
                ),
                node_id: "MDc6TGljZW5zZTA=".into(),
                html_url,
            },
            _ => License {
                key: "other".into(),
                name: "Other".into(),
                spdx_id: "NOASSERTION".into(),
                url: None,
                node_id: "MDc6TGljZW5zZTA=".into(),
                html_url,
            },
        }
    });
    let description = access.repo.description.clone().filter(|d| !d.is_empty());
    let checks = [
        description.is_some(),
        found.readme.is_some(),
        found.code_of_conduct.is_some(),
        found.contributing.is_some(),
        found.license.is_some(),
        found.issue_template.is_some(),
        found.pull_request_template.is_some(),
    ];
    let health =
        (checks.iter().filter(|c| **c).count() as f64 * 100.0 / checks.len() as f64).round() as i64;
    let profile = Profile {
        health_percentage: health,
        description,
        documentation: access.repo.homepage.clone().filter(|h| !h.is_empty()),
        files: Files {
            code_of_conduct,
            code_of_conduct_file: link(&found.code_of_conduct),
            contributing: link(&found.contributing),
            issue_template: link(&found.issue_template),
            pull_request_template: link(&found.pull_request_template),
            license,
            readme: link(&found.readme),
            security: link(&found.security),
        },
        updated_at: Some(Timestamp(
            access.repo.pushed_at.unwrap_or(access.repo.updated_at),
        )),
        content_reports_enabled: false,
    };
    Ok(Json(serde_json::to_value(profile)?))
}

#[cfg(test)]
mod tests {
    use super::is_named;

    #[test]
    fn names() {
        assert!(is_named("README.md", "readme"));
        assert!(is_named("readme", "readme"));
        assert!(is_named("CODE_OF_CONDUCT.rst", "code_of_conduct"));
        assert!(!is_named("READMEFIRST", "readme"));
    }
}
