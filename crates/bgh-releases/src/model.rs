//! Release/asset rows and their GitHub REST shapes (`release`,
//! `release-asset`, `reaction`).

use std::collections::HashMap;

use axum::http::HeaderMap;
use bgh_core::markdown::{self, RenderContext};
use bgh_core::models::api::{ReactionRollup, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_core::urls::{Urls, encode_path, encode_segment};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::FromRow;

/// `releases` row.
#[derive(Debug, Clone, FromRow)]
pub struct ReleaseRow {
    pub id: i64,
    pub repo_id: i64,
    pub tag_name: String,
    pub target_commitish: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub make_latest: Option<bool>,
    pub author_id: Option<i64>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ReleaseRow {
    pub const COLUMNS: &'static str = "id, repo_id, tag_name, target_commitish, name, body, \
        draft, prerelease, make_latest, author_id, published_at, created_at, updated_at";
}

/// `release_assets` row.
#[derive(Debug, Clone, FromRow)]
pub struct AssetRow {
    pub id: i64,
    pub release_id: i64,
    pub repo_id: i64,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
    pub size: i64,
    pub sha256: Option<String>,
    pub state: String,
    pub download_count: i64,
    pub uploader_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AssetRow {
    pub const COLUMNS: &'static str = "id, release_id, repo_id, name, label, content_type, size, \
        sha256, state, download_count, uploader_id, created_at, updated_at";
}

/// Which rendered bodies to include (`application/vnd.github.{raw,html,text,full}+json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyFormat {
    pub raw: bool,
    pub html: bool,
    pub text: bool,
}

impl BodyFormat {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let accept = headers
            .get(axum::http::header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if accept.contains(".full") {
            Self {
                raw: true,
                html: true,
                text: true,
            }
        } else if accept.contains(".html") {
            Self {
                raw: false,
                html: true,
                text: false,
            }
        } else if accept.contains(".text") {
            Self {
                raw: false,
                html: false,
                text: true,
            }
        } else {
            Self::default()
        }
    }
}

impl Default for BodyFormat {
    fn default() -> Self {
        Self {
            raw: true,
            html: false,
            text: false,
        }
    }
}

/// URLs of a repository's releases.
pub struct ReleaseUrls<'a> {
    pub urls: &'a Urls,
    pub owner: &'a str,
    pub repo: &'a str,
}

impl ReleaseUrls<'_> {
    pub fn release(&self, id: i64) -> String {
        self.urls.api(&format!(
            "/repos/{}/{}/releases/{id}",
            self.owner, self.repo
        ))
    }

    pub fn upload(&self, id: i64) -> String {
        self.urls.html(&format!(
            "/api/uploads/repos/{}/{}/releases/{id}/assets{{?name,label}}",
            self.owner, self.repo
        ))
    }

    pub fn html(&self, tag: &str) -> String {
        self.urls.html(&format!(
            "/{}/{}/releases/tag/{}",
            self.owner,
            self.repo,
            encode_path(tag)
        ))
    }

    pub fn asset(&self, id: i64) -> String {
        self.urls.api(&format!(
            "/repos/{}/{}/releases/assets/{id}",
            self.owner, self.repo
        ))
    }

    pub fn download(&self, tag: &str, name: &str) -> String {
        self.urls.html(&format!(
            "/{}/{}/releases/download/{}/{}",
            self.owner,
            self.repo,
            encode_path(tag),
            encode_segment(name)
        ))
    }

    pub fn archive(&self, kind: &str, tag: &str) -> String {
        self.urls.api(&format!(
            "/repos/{}/{}/{kind}/{}",
            self.owner,
            self.repo,
            encode_path(tag)
        ))
    }
}

/// `release-asset`.
#[derive(Debug, Clone, Serialize)]
pub struct Asset {
    pub url: String,
    pub browser_download_url: String,
    pub id: i64,
    pub node_id: String,
    pub name: String,
    pub label: Option<String>,
    pub state: String,
    pub content_type: String,
    pub size: i64,
    pub digest: Option<String>,
    pub download_count: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub uploader: Option<SimpleUser>,
}

impl Asset {
    pub fn new(ru: &ReleaseUrls<'_>, tag: &str, a: &AssetRow, uploader: Option<&db::User>) -> Self {
        Self {
            url: ru.asset(a.id),
            browser_download_url: ru.download(tag, &a.name),
            id: a.id,
            node_id: node_id::encode(NodeType::ReleaseAsset, a.id),
            name: a.name.clone(),
            label: a.label.clone(),
            state: a.state.clone(),
            content_type: a.content_type.clone(),
            size: a.size,
            digest: a.sha256.as_ref().map(|s| format!("sha256:{s}")),
            download_count: a.download_count,
            created_at: a.created_at.into(),
            updated_at: a.updated_at.into(),
            uploader: uploader.map(|u| SimpleUser::new(ru.urls, u)),
        }
    }
}

/// `release`.
#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub url: String,
    pub assets_url: String,
    pub upload_url: String,
    pub html_url: String,
    pub id: i64,
    pub author: SimpleUser,
    pub node_id: String,
    pub tag_name: String,
    pub target_commitish: String,
    pub name: Option<String>,
    pub draft: bool,
    pub immutable: bool,
    pub prerelease: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub published_at: Option<Timestamp>,
    pub assets: Vec<Asset>,
    pub tarball_url: Option<String>,
    pub zipball_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentions_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reactions: Option<ReactionRollup>,
}

/// Rendering context for a set of releases of one repository.
pub struct Renderer<'a> {
    pub state: &'a AppState,
    pub owner: &'a str,
    pub repo: &'a str,
    pub format: BodyFormat,
}

impl Renderer<'_> {
    fn urls(&self) -> ReleaseUrls<'_> {
        ReleaseUrls {
            urls: &self.state.urls,
            owner: self.owner,
            repo: self.repo,
        }
    }

    /// Render releases with their assets, authors and reaction rollups in
    /// a constant number of queries.
    pub async fn render(&self, rows: Vec<ReleaseRow>) -> ApiResult<Vec<Release>> {
        if rows.is_empty() {
            return Ok(vec![]);
        }
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        let assets: Vec<AssetRow> = sqlx::query_as(&format!(
            "SELECT {} FROM release_assets WHERE release_id = ANY($1) ORDER BY id",
            AssetRow::COLUMNS
        ))
        .bind(&ids)
        .fetch_all(&self.state.db)
        .await?;
        let reactions: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT subject_id, content, count(*) FROM reactions
              WHERE subject_type = 'release' AND subject_id = ANY($1)
              GROUP BY subject_id, content",
        )
        .bind(&ids)
        .fetch_all(&self.state.db)
        .await?;
        let users = bgh_core::views::users_by_id(
            self.state,
            rows.iter()
                .map(|r| r.author_id)
                .chain(assets.iter().map(|a| a.uploader_id)),
        )
        .await?;
        let mut assets_by: HashMap<i64, Vec<&AssetRow>> = HashMap::new();
        for a in &assets {
            assets_by.entry(a.release_id).or_default().push(a);
        }
        let mut reactions_by: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
        for (id, content, n) in reactions {
            reactions_by.entry(id).or_default().push((content, n));
        }
        let ru = self.urls();
        Ok(rows
            .iter()
            .map(|r| {
                let assets = assets_by
                    .get(&r.id)
                    .map(|v| {
                        v.iter()
                            .map(|a| {
                                Asset::new(
                                    &ru,
                                    &r.tag_name,
                                    a,
                                    a.uploader_id.and_then(|id| users.get(&id)),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let reactions = reactions_by.get(&r.id).map(|counts| {
                    ReactionRollup::from_counts(format!("{}/reactions", ru.release(r.id)), counts)
                });
                self.release(r, assets, users.get(&r.author_id.unwrap_or(0)), reactions)
            })
            .collect())
    }

    pub async fn render_one(&self, row: ReleaseRow) -> ApiResult<Release> {
        Ok(self
            .render(vec![row])
            .await?
            .pop()
            .expect("one release rendered"))
    }

    fn release(
        &self,
        r: &ReleaseRow,
        assets: Vec<Asset>,
        author: Option<&db::User>,
        reactions: Option<ReactionRollup>,
    ) -> Release {
        let ru = self.urls();
        let url = ru.release(r.id);
        let body = r.body.clone();
        let body_html = self.format.html.then(|| {
            markdown::render(
                body.as_deref().unwrap_or(""),
                &RenderContext::new(&self.state.config.base_url).with_repo(self.owner, self.repo),
            )
        });
        let body_text = self.format.text.then(|| body.clone().unwrap_or_default());
        let mentions = body.as_deref().map(count_mentions).filter(|n| *n > 0);
        Release {
            assets_url: format!("{url}/assets"),
            upload_url: ru.upload(r.id),
            html_url: ru.html(&r.tag_name),
            url,
            id: r.id,
            author: SimpleUser::or_ghost(&self.state.urls, author),
            node_id: node_id::encode(NodeType::Release, r.id),
            tag_name: r.tag_name.clone(),
            target_commitish: r.target_commitish.clone(),
            name: r.name.clone(),
            draft: r.draft,
            immutable: false,
            prerelease: r.prerelease,
            created_at: r.created_at.into(),
            updated_at: r.updated_at.into(),
            published_at: ts(r.published_at),
            assets,
            tarball_url: (!r.draft).then(|| ru.archive("tarball", &r.tag_name)),
            zipball_url: (!r.draft).then(|| ru.archive("zipball", &r.tag_name)),
            body: self.format.raw.then_some(body),
            body_html,
            body_text,
            mentions_count: mentions,
            reactions: reactions.filter(|r| r.total_count > 0),
        }
    }
}

/// Distinct `@login` mentions in a markdown body.
fn count_mentions(body: &str) -> i64 {
    let mut seen = std::collections::HashSet::new();
    let bytes = body.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'@' || (i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'`')) {
            continue;
        }
        let login: String = body[i + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if !login.is_empty() {
            seen.insert(login.to_lowercase());
        }
    }
    seen.len() as i64
}

/// Compact client shape recorded in `sync_actions` for model `release`.
pub fn release_sync_json(r: &ReleaseRow) -> Value {
    json!({
        "id": r.id,
        "repo_id": r.repo_id,
        "tag_name": r.tag_name,
        "name": r.name,
        "draft": r.draft,
        "prerelease": r.prerelease,
        "author_id": r.author_id,
        "published_at": r.published_at.map(Timestamp::from),
        "created_at": Timestamp::from(r.created_at),
    })
}

/// `reaction`.
#[derive(Debug, Clone, Serialize)]
pub struct Reaction {
    pub id: i64,
    pub node_id: String,
    pub user: Option<SimpleUser>,
    pub content: String,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions() {
        assert_eq!(count_mentions("thanks @alice and @bob, @alice"), 2);
        assert_eq!(count_mentions("mail me a@b.c"), 0);
        assert_eq!(count_mentions("nobody"), 0);
    }
}
