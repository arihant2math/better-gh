//! `package` webhook objects (container packages, P15).

use bgh_core::AppState;
use bgh_core::time::Timestamp;
use bgh_core::urls::encode_segment;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::common::{RepoCtx, sender, user_json};

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    name: String,
    package_type: String,
    owner_id: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    version_id: i64,
    digest: String,
    tags: Vec<String>,
    pushed_by: Option<i64>,
    v_created_at: DateTime<Utc>,
    v_updated_at: DateTime<Utc>,
}

/// Webhook `package` object with its `package_version` (`None` if gone).
pub async fn package(
    state: &AppState,
    _ctx: &RepoCtx,
    package_id: i64,
    version_id: i64,
    tag: Option<&str>,
) -> anyhow::Result<Option<Value>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT p.id, p.name, p.package_type, p.owner_id, p.created_at, p.updated_at,
                v.id AS version_id, v.digest, v.tags, v.pushed_by,
                v.created_at AS v_created_at, v.updated_at AS v_updated_at
           FROM packages p JOIN package_versions v ON v.package_id = p.id
          WHERE p.id = $1 AND v.id = $2",
    )
    .bind(package_id)
    .bind(version_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let Some(owner) = bgh_core::models::db::User::find(&state.db, r.owner_id).await? else {
        return Ok(None);
    };
    let urls = &state.urls;
    let kind = if owner.is_org() { "orgs" } else { "users" };
    let name_seg = encode_segment(&r.name);
    let html_url = urls.html(&format!(
        "/{kind}/{}/packages/{}/package/{name_seg}",
        owner.login, r.package_type
    ));
    let host = &urls.host;
    let image = format!("{host}/{}/{}", owner.login.to_lowercase(), r.name);
    let tag_name = tag
        .map(str::to_string)
        .or_else(|| r.tags.first().cloned())
        .unwrap_or_default();
    let package_url = if tag_name.is_empty() {
        format!("{image}@{}", r.digest)
    } else {
        format!("{image}:{tag_name}")
    };
    let owner_json = user_json(urls, &owner);
    let author = sender(state, r.pushed_by).await?;
    let ecosystem = r.package_type.to_uppercase();
    Ok(Some(json!({
        "id": r.id,
        "name": r.name,
        "namespace": owner.login,
        "description": "",
        "ecosystem": ecosystem,
        "package_type": ecosystem,
        "html_url": html_url,
        "created_at": Timestamp(r.created_at),
        "updated_at": Timestamp(r.updated_at),
        "owner": owner_json,
        "package_version": {
            "id": r.version_id,
            "version": r.digest,
            "name": r.digest,
            "description": "",
            "summary": "",
            "body": "",
            "body_html": "",
            "html_url": urls.html(&format!(
                "/{kind}/{}/packages/{}/{name_seg}/{}",
                owner.login, r.package_type, r.version_id
            )),
            "target_commitish": "",
            "target_oid": "",
            "created_at": Timestamp(r.v_created_at),
            "updated_at": Timestamp(r.v_updated_at),
            "metadata": [],
            "container_metadata": {
                "tag": { "name": tag_name, "digest": r.digest },
                "labels": {},
                "manifest": {},
            },
            "package_files": [],
            "installation_command": format!("docker pull {package_url}"),
            "package_url": package_url,
            "author": author,
            "draft": false,
            "prerelease": false,
        },
        "registry": {
            "about_url": "https://docs.github.com/packages/working-with-a-github-packages-registry/working-with-the-container-registry",
            "name": "GitHub CONTAINER registry",
            "type": "CONTAINER",
            "url": urls.html(""),
            "vendor": "GitHub Inc",
        },
    })))
}
