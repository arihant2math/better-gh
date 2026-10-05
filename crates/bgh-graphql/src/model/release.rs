//! Releases and release assets.

use std::sync::Arc;

use async_graphql::{Context, ID, Object};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;

use super::enums::{ReleaseOrder, ReleaseOrderField};
use super::issue::{html_to_text, render_html};
use super::repo::{Ref, Repository};
use super::{Actor, User, actor, nid};
use crate::conn::{ConnArgs, Page, connection};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::{AssetRow, Loaders, ReleaseRow, one};
use crate::scalars::{DateTime, HTML, URI, dt, odt};

/// A release contains the content for a release.
#[derive(Clone)]
pub struct Release {
    pub repo: Repository,
    pub r: Arc<ReleaseRow>,
}

impl Release {
    pub fn new(repo: Repository, r: Arc<ReleaseRow>) -> Self {
        Self { repo, r }
    }

    fn html_path(&self) -> String {
        format!(
            "/{}/releases/tag/{}",
            self.repo.row().full_name(),
            bgh_core::urls::encode_segment(&self.r.tag_name)
        )
    }
}

#[Object]
impl Release {
    pub async fn id(&self) -> ID {
        nid(NodeType::Release, self.r.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.r.id)
    }
    pub async fn name(&self) -> Option<String> {
        self.r.name.clone()
    }
    pub async fn tag_name(&self) -> String {
        self.r.tag_name.clone()
    }
    pub async fn tag_commit(&self, ctx: &Context<'_>) -> GResult<Option<super::Commit>> {
        super::git::commit(
            ctx,
            self.repo.clone(),
            &format!("refs/tags/{}", self.r.tag_name),
        )
        .await
    }
    pub async fn tag(&self, ctx: &Context<'_>) -> GResult<Option<Ref>> {
        Ref::load(
            ctx,
            self.repo.clone(),
            &format!("refs/tags/{}", self.r.tag_name),
        )
        .await
    }
    pub async fn description(&self) -> Option<String> {
        self.r.body.clone()
    }
    #[graphql(name = "descriptionHTML")]
    pub async fn description_html(&self, ctx: &Context<'_>) -> Option<HTML> {
        Some(render_html(
            ctx,
            &self.repo,
            self.r.body.as_deref().unwrap_or(""),
        ))
    }
    pub async fn short_description_html(&self, ctx: &Context<'_>) -> Option<HTML> {
        let text =
            html_to_text(&render_html(ctx, &self.repo, self.r.body.as_deref().unwrap_or("")).0);
        Some(HTML(text.chars().take(200).collect()))
    }
    pub async fn is_draft(&self) -> bool {
        self.r.draft
    }
    pub async fn is_prerelease(&self) -> bool {
        self.r.prerelease
    }
    pub async fn is_latest(&self, ctx: &Context<'_>) -> GResult<bool> {
        let l = ctx.data_unchecked::<Loaders>();
        Ok(one(&l.latest_release, self.r.repo_id)
            .await?
            .is_some_and(|x| x.id == self.r.id))
    }
    pub async fn is_immutable(&self) -> bool {
        false
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.r.created_at)
    }
    pub async fn published_at(&self) -> Option<DateTime> {
        odt(self.r.published_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.r.created_at)
    }
    pub async fn author(&self, ctx: &Context<'_>) -> GResult<Option<User>> {
        actor::user(ctx, self.r.author_id).await
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.html(&self.html_path()))
    }
    pub async fn resource_path(&self) -> URI {
        URI(self.html_path())
    }
    pub async fn repository(&self) -> Repository {
        self.repo.clone()
    }
    pub async fn release_assets(
        &self,
        ctx: &Context<'_>,
        first: Option<i32>,
        last: Option<i32>,
        after: Option<String>,
        before: Option<String>,
        name: Option<String>,
    ) -> GResult<ReleaseAssetConnection> {
        let l = ctx.data_unchecked::<Loaders>();
        let rows = one(&l.release_assets, self.r.id).await?.unwrap_or_default();
        let items = rows
            .iter()
            .filter(|a| name.as_deref().is_none_or(|n| n == a.name))
            .map(|a| ReleaseAsset {
                release: self.clone(),
                a: Arc::new(a.clone()),
            })
            .collect();
        Ok(Page::from_vec(items, &ConnArgs::new(first, last, after, before))?.into())
    }
    pub async fn reaction_groups(
        &self,
        ctx: &Context<'_>,
    ) -> GResult<Option<Vec<super::misc::ReactionGroup>>> {
        Ok(Some(
            super::misc::load_reaction_groups(ctx, "release", self.r.id).await?,
        ))
    }
    pub async fn viewer_can_react(&self, ctx: &Context<'_>) -> bool {
        gql(ctx).auth.is_some()
    }
}

connection!(ReleaseConnection, ReleaseEdge, Release);

/// A release asset contains the content for a release asset.
#[derive(Clone)]
pub struct ReleaseAsset {
    release: Release,
    a: Arc<AssetRow>,
}

#[Object]
impl ReleaseAsset {
    pub async fn id(&self) -> ID {
        nid(NodeType::ReleaseAsset, self.a.id)
    }
    pub async fn database_id(&self) -> Option<i64> {
        Some(self.a.id)
    }
    pub async fn name(&self) -> String {
        self.a.name.clone()
    }
    pub async fn content_type(&self) -> String {
        self.a.content_type.clone()
    }
    pub async fn size(&self) -> i32 {
        i32::try_from(self.a.size).unwrap_or(i32::MAX)
    }
    pub async fn download_count(&self) -> i32 {
        self.a.download_count as i32
    }
    pub async fn download_url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.html(&format!(
            "/{}/releases/download/{}/{}",
            self.release.repo.row().full_name(),
            bgh_core::urls::encode_segment(&self.release.r.tag_name),
            bgh_core::urls::encode_segment(&self.a.name)
        )))
    }
    pub async fn url(&self, ctx: &Context<'_>) -> URI {
        URI(gql(ctx).state.urls.api(&format!(
            "/repos/{}/releases/assets/{}",
            self.release.repo.row().full_name(),
            self.a.id
        )))
    }
    pub async fn created_at(&self) -> DateTime {
        dt(self.a.created_at)
    }
    pub async fn updated_at(&self) -> DateTime {
        dt(self.a.updated_at)
    }
    pub async fn uploaded_by(&self, ctx: &Context<'_>) -> GResult<Option<Actor>> {
        actor::actor(ctx, self.a.uploader_id).await
    }
    pub async fn release(&self) -> Release {
        self.release.clone()
    }
}

connection!(ReleaseAssetConnection, ReleaseAssetEdge, ReleaseAsset);

pub async fn list_for_repo(
    ctx: &Context<'_>,
    repo: &Repository,
    args: ConnArgs,
    order_by: Option<ReleaseOrder>,
) -> GResult<ReleaseConnection> {
    let g = gql(ctx);
    let drafts = repo.row().perm >= Permission::Write;
    let order = order_by.unwrap_or(ReleaseOrder {
        field: ReleaseOrderField::CreatedAt,
        direction: super::enums::OrderDirection::Desc,
    });
    let col = match order.field {
        ReleaseOrderField::CreatedAt => "created_at",
        ReleaseOrderField::Name => "lower(coalesce(name, tag_name))",
    };
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM releases WHERE repo_id = $1 AND ($2 OR NOT draft)",
    )
    .bind(repo.rid())
    .bind(drafts)
    .fetch_one(&g.state.db)
    .await
    .gql()?;
    let w = args.window(Some(total))?;
    let rows: Vec<ReleaseRow> = sqlx::query_as(&format!(
        "SELECT {} FROM releases WHERE repo_id = $1 AND ($2 OR NOT draft)
          ORDER BY {col} {d}, id {d} LIMIT $3 OFFSET $4",
        ReleaseRow::COLUMNS,
        d = order.direction.sql()
    ))
    .bind(repo.rid())
    .bind(drafts)
    .bind(w.fetch())
    .bind(w.offset)
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let page = Page::from_fetched(rows, w, Some(total));
    Ok(page.map(|r| Release::new(repo.clone(), Arc::new(r))).into())
}

pub async fn by_tag(ctx: &Context<'_>, repo: &Repository, tag: &str) -> GResult<Option<Release>> {
    let g = gql(ctx);
    let drafts = repo.row().perm >= Permission::Write;
    let row: Option<ReleaseRow> = sqlx::query_as(&format!(
        "SELECT {} FROM releases WHERE repo_id = $1 AND tag_name = $2 AND ($3 OR NOT draft)
          ORDER BY draft, id DESC LIMIT 1",
        ReleaseRow::COLUMNS
    ))
    .bind(repo.rid())
    .bind(tag)
    .bind(drafts)
    .fetch_optional(&g.state.db)
    .await
    .gql()?;
    Ok(row.map(|r| Release::new(repo.clone(), Arc::new(r))))
}
