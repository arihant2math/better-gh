//! Git work of the P51 steps: pull request heads (`refs/pull/{n}/head`),
//! base/head branch recreation for open pull requests, and the wiki.
//!
//! All fetches go through P11's pinned, SSRF-checked [`bgh_repos::import::remote`]
//! with the source token as basic auth (`x-access-token` on GitHub,
//! `oauth2` on GitLab). Refs are written directly (`force_ref`), so no push
//! event fires: like the rest of the import, this is history, not news.

use std::time::Duration;

use anyhow::Context;
use bgh_core::prelude::*;
use bgh_git::RepoStore;
use bgh_git::fetch::Remote;

use crate::pipeline::Ctx;
use crate::row;

const FETCH_TIMEOUT: Duration = Duration::from_secs(3600);
/// Where GitLab merge request heads are staged before they get their
/// `refs/pull/{n}/head` (numbers differ from MR iids). Hidden namespace.
pub const STAGING: &str = "refs/bgh/import/";

pub(crate) fn store(state: &AppState) -> RepoStore {
    RepoStore::from_config(&state.config)
}

impl Ctx {
    /// Source clone URL (GitHub `clone_url`, GitLab `http_url_to_repo`).
    pub(crate) async fn clone_url(&self) -> anyhow::Result<Option<String>> {
        let (src, _) = if self.row.is_gitlab() {
            self.gh
                .get(&crate::gitlab::project_path(self.src()))
                .await?
        } else {
            self.gh.get(&format!("/repos/{}", self.src())).await?
        };
        Ok(src["clone_url"]
            .as_str()
            .or(src["http_url_to_repo"].as_str())
            .map(str::to_string))
    }

    /// A pinned remote for `url` with the source token.
    pub(crate) async fn remote(&self, url: &str) -> anyhow::Result<Remote> {
        let (url, inline) = bgh_repos::import::parse_remote_url(&self.state, "source", url)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let user = if self.row.is_gitlab() {
            "oauth2"
        } else {
            "x-access-token"
        };
        let creds = match self.token.as_deref() {
            Some(t) => bgh_repos::import::Credentials::from_parts(Some(user), Some(t)),
            None => inline,
        };
        bgh_repos::import::remote(&self.state, &url, creds.as_ref(), FETCH_TIMEOUT)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    pub(crate) async fn has_commit(&self, sha: &str) -> bool {
        if sha.len() < 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        let Ok(repo_id) = self.repo_id() else {
            return false;
        };
        let sha = sha.to_string();
        matches!(
            store(&self.state)
                .read(repo_id, move |r| r.header(&sha))
                .await,
            Ok(Some(("commit", _)))
        )
    }

    pub(crate) async fn local_ref(&self, refname: &str) -> Option<String> {
        let repo_id = self.repo_id().ok()?;
        let name = refname.to_string();
        store(&self.state)
            .read(repo_id, move |r| Ok(r.find_ref(&name)?.map(|r| r.peeled)))
            .await
            .ok()
            .flatten()
    }

    /// One fetch of every pull request head on the source (GitHub
    /// `refs/pull/*/head` → same; GitLab `refs/merge-requests/*/head` →
    /// [`STAGING`]). Failures are logged: PRs still import, each falling
    /// back to fetching its head by name or SHA.
    pub(crate) async fn prefetch_pull_heads(&mut self) -> anyhow::Result<()> {
        if self.row.cursor["prefetched"].as_bool() == Some(true) {
            return Ok(());
        }
        let refspec = if self.row.is_gitlab() {
            format!("+refs/merge-requests/*/head:{STAGING}mr/*")
        } else {
            "+refs/pull/*/head:refs/pull/*/head".to_string()
        };
        match self.fetch(&[refspec]).await {
            Ok(()) => {}
            Err(e) => {
                row::log(
                    &self.state.db,
                    self.row.id,
                    "warn",
                    &format!("fetching pull request heads: {e:#}"),
                )
                .await
            }
        }
        self.row.cursor = serde_json::json!({"prefetched": true});
        sqlx::query("UPDATE imports SET cursor = $2 WHERE id = $1")
            .bind(self.row.id)
            .bind(&self.row.cursor)
            .execute(&self.state.db)
            .await?;
        Ok(())
    }

    async fn fetch(&self, refspecs: &[String]) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        let url = self
            .clone_url()
            .await?
            .context("the source has no clone URL")?;
        let remote = self.remote(&url).await?;
        bgh_git::fetch::fetch_refspecs(&store(&self.state), repo_id, &remote, refspecs)
            .await
            .map_err(|e| anyhow::anyhow!(bgh_repos::import::describe_error(&e)))
    }

    /// Make `refs/pull/{number}/head` exist at the source head: already
    /// fetched, staged (GitLab), present as an object (same-repo branch),
    /// or fetched by `source_ref` / SHA. False when the commit is
    /// unavailable (logged); the PR then imports without a diff.
    pub(crate) async fn ensure_pull_head(
        &self,
        number: i64,
        head_sha: &str,
        source_ref: &str,
        staged: Option<&str>,
    ) -> anyhow::Result<bool> {
        let pull_ref = format!("refs/pull/{number}/head");
        let repo_id = self.repo_id()?;
        let store = store(&self.state);
        if let Some(sha) = self.local_ref(&pull_ref).await
            && (sha == head_sha || head_sha.is_empty())
        {
            return Ok(true);
        }
        if let Some(staged) = staged
            && self.local_ref(staged).await.as_deref() == Some(head_sha)
        {
            bgh_git::merge::force_ref(&store, repo_id, &pull_ref, head_sha).await?;
            return Ok(true);
        }
        if !self.has_commit(head_sha).await {
            let by_name = format!("+{source_ref}:{pull_ref}");
            let by_sha = format!("+{head_sha}:{pull_ref}");
            if self.fetch(&[by_name]).await.is_err() {
                let _ = self.fetch(&[by_sha]).await;
            }
        }
        if self.has_commit(head_sha).await {
            bgh_git::merge::force_ref(&store, repo_id, &pull_ref, head_sha).await?;
            return Ok(true);
        }
        row::log(
            &self.state.db,
            self.row.id,
            "warn",
            &format!("#{number}: head commit {head_sha} not available; imported without a diff"),
        )
        .await;
        Ok(false)
    }

    /// Open pull requests need their branches: recreate a missing base
    /// branch (and the head branch of a same-repository PR) at the
    /// source SHA. Closed ones keep only `refs/pull/{n}/head`.
    pub(crate) async fn ensure_branch(&self, branch: &str, sha: &str) -> anyhow::Result<()> {
        if branch.is_empty() || !bgh_git::is_valid_ref_name(branch) {
            return Ok(());
        }
        let refname = format!("refs/heads/{branch}");
        if self.local_ref(&refname).await.is_some() || !self.has_commit(sha).await {
            return Ok(());
        }
        bgh_git::merge::force_ref(&store(&self.state), self.repo_id()?, &refname, sha).await?;
        row::log(
            &self.state.db,
            self.row.id,
            "info",
            &format!("recreated branch {branch} at {}", &sha[..sha.len().min(12)]),
        )
        .await;
        Ok(())
    }

    /// Remove the GitLab staging refs.
    pub(crate) async fn drop_staging(&self) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        let store = store(&self.state);
        let refs = store
            .read(repo_id, |r| r.refs(STAGING))
            .await
            .unwrap_or_default();
        for r in refs {
            bgh_git::merge::remove_ref(&store, repo_id, &r.name).await?;
        }
        Ok(())
    }

    /// `wiki` step: fetch `{repo}.wiki.git` into this repository's wiki.
    /// A source without a wiki (not found) is skipped.
    pub(crate) async fn wiki(&mut self) -> anyhow::Result<()> {
        let repo_id = self.repo_id()?;
        let Some(clone) = self.clone_url().await? else {
            return Ok(());
        };
        let base = clone.trim_end_matches('/');
        let base = base.strip_suffix(".git").unwrap_or(base);
        let url = format!("{base}.wiki.git");
        let wiki = store(&self.state).wiki();
        let created = !wiki.exists(repo_id);
        if created {
            wiki.init(repo_id, bgh_wiki::pages::DEFAULT_BRANCH).await?;
        }
        let remote = self.remote(&url).await?;
        let result = bgh_git::fetch::fetch_refspecs(
            &wiki,
            repo_id,
            &remote,
            &["+refs/heads/*:refs/heads/*".to_string()],
        )
        .await;
        if let Err(e) = result {
            if created {
                let _ = wiki.delete(repo_id).await;
            }
            row::log(
                &self.state.db,
                self.row.id,
                "info",
                &format!(
                    "wiki: not imported ({})",
                    bgh_repos::import::describe_error(&e)
                ),
            )
            .await;
            return Ok(());
        }
        let branches = wiki
            .read(repo_id, |r| r.branches())
            .await
            .unwrap_or_default();
        if branches.is_empty() {
            if created {
                let _ = wiki.delete(repo_id).await;
            }
            row::log(&self.state.db, self.row.id, "info", "wiki: empty, skipped").await;
            return Ok(());
        }
        let default = bgh_wiki::pages::DEFAULT_BRANCH;
        if !branches.iter().any(|b| b.short_name() == default) {
            // Pages are read from `master`; point it at the source's branch.
            let first = &branches[0];
            bgh_git::merge::force_ref(
                &wiki,
                repo_id,
                &format!("refs/heads/{default}"),
                &first.peeled,
            )
            .await?;
        }
        bgh_git::write::set_head(&wiki, repo_id, default).await?;
        let mut tx = Tx::begin(&self.state).await?;
        sqlx::query("UPDATE repositories SET has_wiki = true WHERE id = $1")
            .bind(repo_id)
            .execute(&mut *tx)
            .await?;
        tx.sync_model(SyncModel::Repo, repo_id, SyncAction::Update)
            .await?;
        // Counted once; a rerun fetches new wiki commits again.
        if self.mapped("wiki", "wiki").await?.is_none() {
            self.map(&mut tx, "wiki", "wiki", repo_id).await?;
            row::bump(&mut tx, self.row.id, "wiki", 1).await?;
        }
        tx.commit().await?;
        row::log(&self.state.db, self.row.id, "info", "wiki: imported").await;
        Ok(())
    }
}
