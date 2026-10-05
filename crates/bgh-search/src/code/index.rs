//! Background indexing of each repository's default branch: file contents
//! (`code_files` / `code_blobs`, incremental by blob SHA) and commits
//! (`commit_index`, incremental from the previously indexed head).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bgh_core::events::Event;
use bgh_core::jobs::{self, JobPayload};
use bgh_core::prelude::*;
use bgh_git::{GitRepo, GitResult, RepoStore, TreeEntryKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::lang;

/// Files larger than this are indexed by path only (GitHub: 384 KB).
pub const MAX_FILE_SIZE: u64 = 384 * 1024;
/// Files per repository considered for indexing.
pub const MAX_FILES: usize = 100_000;
/// Commits indexed per run.
pub const MAX_COMMITS: usize = 10_000;
/// Blobs read and inserted per batch.
const BATCH: usize = 256;

/// Re-index a repository's default branch (idempotent; no-op when the
/// branch tip is already indexed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexRepo {
    pub repo_id: i64,
}

impl JobPayload for IndexRepo {
    const KIND: &'static str = "search.index_repo";
    const MAX_ATTEMPTS: i32 = 5;
}

/// Remove blobs no indexed file references anymore (admin/maintenance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcCodeBlobs {}

impl JobPayload for GcCodeBlobs {
    const KIND: &'static str = "search.gc_code_blobs";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Delay before indexing after a change: bursts of pushes coalesce into
/// one run (the pending job is reused).
pub const INDEX_DELAY: chrono::Duration = chrono::Duration::seconds(2);

/// Enqueue `job` to run after `delay` unless an identical one is already
/// waiting.
pub async fn enqueue_once<J: JobPayload>(
    state: &AppState,
    job: &J,
    delay: chrono::Duration,
) -> anyhow::Result<()> {
    let payload = serde_json::to_value(job)?;
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND payload = $2
                          AND locked_at IS NULL AND failed_at IS NULL)",
    )
    .bind(J::KIND)
    .bind(&payload)
    .fetch_one(&state.db)
    .await?;
    if !pending {
        jobs::enqueue_at(
            &state.db,
            J::KIND,
            &payload,
            Utc::now() + delay,
            J::MAX_ATTEMPTS,
        )
        .await?;
    }
    Ok(())
}

/// Event listener: schedule indexing after pushes to the default branch,
/// repository creation/updates, and blob GC after deletions.
pub async fn on_event(state: AppState, event: Arc<Event>) -> anyhow::Result<()> {
    match &*event {
        Event::Push(p) => {
            let default: Option<String> =
                sqlx::query_scalar("SELECT default_branch FROM repositories WHERE id = $1")
                    .bind(p.repo_id)
                    .fetch_optional(&state.db)
                    .await?;
            let Some(default) = default else {
                return Ok(());
            };
            if p.updates
                .iter()
                .any(|u| u.branch() == Some(default.as_str()))
            {
                enqueue_once(&state, &IndexRepo { repo_id: p.repo_id }, INDEX_DELAY).await?;
            }
        }
        Event::RepositoryCreated { repo_id, .. } | Event::RepositoryUpdated { repo_id, .. } => {
            if has_default_branch(&state, *repo_id).await? {
                enqueue_once(&state, &IndexRepo { repo_id: *repo_id }, INDEX_DELAY).await?;
            }
        }
        Event::RepositoryForked { fork_id, .. } => {
            if has_default_branch(&state, *fork_id).await? {
                enqueue_once(&state, &IndexRepo { repo_id: *fork_id }, INDEX_DELAY).await?;
            }
        }
        Event::RepositoryDeleted { .. } => {
            // Files cascade away with the repository; their blobs are
            // collected by the next indexing run (see `collect_orphans`).
            sqlx::query("UPDATE code_index_gc SET pending = true")
                .execute(&state.db)
                .await?;
        }
        _ => {}
    }
    Ok(())
}

/// Whether the repository's default branch exists (nothing to index otherwise).
async fn has_default_branch(state: &AppState, repo_id: i64) -> anyhow::Result<bool> {
    let Some(repo) = db::Repository::find(&state.db, repo_id).await? else {
        return Ok(false);
    };
    let store = RepoStore::from_config(&state.config);
    if !store.exists(repo_id) {
        return Ok(false);
    }
    let refname = format!("refs/heads/{}", repo.default_branch);
    Ok(store
        .read(repo_id, move |r| r.find_ref(&refname))
        .await?
        .is_some())
}

pub async fn index_repo_job(state: AppState, job: IndexRepo) -> anyhow::Result<()> {
    index_repo(&state, job.repo_id).await?;
    collect_orphans(&state, false).await
}

/// Delete blobs no indexed file references, when repositories were deleted
/// since the last collection (or always with `force`).
pub async fn collect_orphans(state: &AppState, force: bool) -> anyhow::Result<()> {
    let pending: bool = sqlx::query_scalar(
        "UPDATE code_index_gc SET pending = false WHERE pending OR $1 RETURNING true",
    )
    .bind(force)
    .fetch_optional(&state.db)
    .await?
    .unwrap_or(false);
    if pending {
        sqlx::query(
            "DELETE FROM code_blobs b
              WHERE NOT EXISTS (SELECT 1 FROM code_files f WHERE f.blob_sha = b.sha)",
        )
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

pub async fn gc_job(state: AppState, _job: GcCodeBlobs) -> anyhow::Result<()> {
    collect_orphans(&state, true).await
}

/// What an indexing run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub files: usize,
    pub changed: usize,
    pub deleted: usize,
    pub blobs_read: usize,
    pub commits: usize,
    pub skipped: bool,
}

#[derive(sqlx::FromRow)]
struct StateRow {
    commit_sha: String,
}

fn walk(r: &GitRepo, tree: &str, prefix: &str, out: &mut Vec<(String, String)>) -> GitResult<()> {
    for e in r.tree(tree)? {
        if out.len() >= MAX_FILES {
            return Ok(());
        }
        let path = if prefix.is_empty() {
            e.name.clone()
        } else {
            format!("{prefix}/{}", e.name)
        };
        match e.kind {
            TreeEntryKind::Tree => {
                if e.name == "node_modules" || e.name == ".git" {
                    continue;
                }
                walk(r, &e.sha, &path, out)?;
            }
            TreeEntryKind::Blob | TreeEntryKind::Executable => out.push((path, e.sha)),
            _ => {}
        }
    }
    Ok(())
}

/// Index the default branch of `repo_id`. Safe to run concurrently with
/// itself (serialized by an advisory lock).
pub async fn index_repo(state: &AppState, repo_id: i64) -> anyhow::Result<IndexStats> {
    let mut stats = IndexStats::default();
    let Some(repo) = db::Repository::find(&state.db, repo_id).await? else {
        return Ok(stats);
    };
    let store = RepoStore::from_config(&state.config);
    if !store.exists(repo_id) {
        return Ok(stats);
    }
    let branch = repo.default_branch.clone();
    let refname = format!("refs/heads/{branch}");
    let head = store
        .read(repo_id, move |r| {
            Ok(r.find_ref(&refname)?.map(|r| r.peeled))
        })
        .await?;
    let previous: Option<StateRow> =
        sqlx::query_as("SELECT commit_sha FROM code_index_state WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_optional(&state.db)
            .await?;
    let Some(head) = head else {
        // Empty repository (or branch deleted): drop the index.
        let mut tx = state.db.begin().await?;
        sqlx::query("DELETE FROM code_files WHERE repo_id = $1")
            .bind(repo_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM code_index_state WHERE repo_id = $1")
            .bind(repo_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        stats.skipped = true;
        return Ok(stats);
    };
    if previous.as_ref().is_some_and(|p| p.commit_sha == head) {
        stats.skipped = true;
        return Ok(stats);
    }

    // Current tree listing with sizes.
    let h = head.clone();
    let files: Vec<(String, String, u64)> = store
        .read(repo_id, move |r| {
            let commit = r.commit(&h)?;
            let mut out = Vec::new();
            walk(r, &commit.tree, "", &mut out)?;
            let mut sized = Vec::with_capacity(out.len());
            for (path, sha) in out {
                let size = r.header(&sha)?.map(|(_, s)| s).unwrap_or(0);
                sized.push((path, sha, size));
            }
            Ok(sized)
        })
        .await?;
    stats.files = files.len();

    let existing: Vec<(String, String)> =
        sqlx::query_as("SELECT path, blob_sha FROM code_files WHERE repo_id = $1")
            .bind(repo_id)
            .fetch_all(&state.db)
            .await?;
    let existing: HashMap<String, String> = existing.into_iter().collect();
    let current: HashSet<&str> = files.iter().map(|(p, _, _)| p.as_str()).collect();
    let deleted: Vec<String> = existing
        .keys()
        .filter(|p| !current.contains(p.as_str()))
        .cloned()
        .collect();
    let changed: Vec<&(String, String, u64)> = files
        .iter()
        .filter(|(p, sha, _)| existing.get(p) != Some(sha))
        .collect();
    stats.deleted = deleted.len();
    stats.changed = changed.len();
    let old_shas: Vec<String> = deleted
        .iter()
        .filter_map(|p| existing.get(p).cloned())
        .chain(
            changed
                .iter()
                .filter_map(|(p, _, _)| existing.get(p).cloned()),
        )
        .collect();

    // Blobs we need to read: changed, small enough, not stored yet.
    let mut wanted: Vec<String> = changed
        .iter()
        .filter(|(_, _, size)| *size <= MAX_FILE_SIZE)
        .map(|(_, sha, _)| sha.clone())
        .collect();
    wanted.sort();
    wanted.dedup();
    let present: Vec<String> = if wanted.is_empty() {
        vec![]
    } else {
        sqlx::query_scalar("SELECT sha FROM code_blobs WHERE sha = ANY($1)")
            .bind(&wanted)
            .fetch_all(&state.db)
            .await?
    };
    let present: HashSet<String> = present.into_iter().collect();
    let to_read: Vec<String> = wanted
        .into_iter()
        .filter(|s| !present.contains(s))
        .collect();
    for chunk in to_read.chunks(BATCH) {
        let shas = chunk.to_vec();
        let blobs: Vec<(String, String, i32)> = store
            .read(repo_id, move |r| {
                let mut out = Vec::new();
                for sha in shas {
                    let blob = match r.blob_with_limit(&sha, MAX_FILE_SIZE) {
                        Ok(b) => b,
                        Err(bgh_git::GitError::TooLarge { .. }) => continue,
                        Err(e) => return Err(e),
                    };
                    if blob.is_binary() {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&blob.data).replace('\0', "");
                    out.push((sha, text, blob.size as i32));
                }
                Ok(out)
            })
            .await?;
        stats.blobs_read += blobs.len();
        if blobs.is_empty() {
            continue;
        }
        let (shas, rest): (Vec<String>, Vec<(String, i32)>) =
            blobs.into_iter().map(|(s, t, n)| (s, (t, n))).unzip();
        let (texts, sizes): (Vec<String>, Vec<i32>) = rest.into_iter().unzip();
        sqlx::query(
            "INSERT INTO code_blobs (sha, content, size)
             SELECT * FROM unnest($1::text[], $2::text[], $3::int[])
             ON CONFLICT (sha) DO NOTHING",
        )
        .bind(&shas)
        .bind(&texts)
        .bind(&sizes)
        .execute(&state.db)
        .await?;
    }

    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(7001, $1::int)")
        .bind((repo_id % i64::from(i32::MAX)) as i32)
        .execute(&mut *tx)
        .await?;
    if !deleted.is_empty() {
        sqlx::query("DELETE FROM code_files WHERE repo_id = $1 AND path = ANY($2)")
            .bind(repo_id)
            .bind(&deleted)
            .execute(&mut *tx)
            .await?;
    }
    for chunk in changed.chunks(1000) {
        let paths: Vec<&str> = chunk.iter().map(|(p, _, _)| p.as_str()).collect();
        let shas: Vec<&str> = chunk.iter().map(|(_, s, _)| s.as_str()).collect();
        let names: Vec<&str> = paths
            .iter()
            .map(|p| p.rsplit('/').next().unwrap_or(p))
            .collect();
        let exts: Vec<Option<String>> = names.iter().map(|n| lang::extension(n)).collect();
        let langs: Vec<Option<&str>> = names.iter().map(|n| lang::detect(n)).collect();
        let sizes: Vec<i32> = chunk
            .iter()
            .map(|(_, _, s)| (*s).min(i32::MAX as u64) as i32)
            .collect();
        sqlx::query(
            "INSERT INTO code_files (repo_id, path, blob_sha, name, extension, language, size)
             SELECT $1, * FROM unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::int[])
             ON CONFLICT (repo_id, path) DO UPDATE
                SET blob_sha = EXCLUDED.blob_sha, name = EXCLUDED.name,
                    extension = EXCLUDED.extension, language = EXCLUDED.language,
                    size = EXCLUDED.size",
        )
        .bind(repo_id)
        .bind(&paths)
        .bind(&shas)
        .bind(&names)
        .bind(&exts)
        .bind(&langs)
        .bind(&sizes)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO code_index_state (repo_id, ref, commit_sha, file_count, indexed_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (repo_id) DO UPDATE
            SET ref = EXCLUDED.ref, commit_sha = EXCLUDED.commit_sha,
                file_count = EXCLUDED.file_count, indexed_at = now()",
    )
    .bind(repo_id)
    .bind(format!("refs/heads/{branch}"))
    .bind(&head)
    .bind(files.len() as i32)
    .execute(&mut *tx)
    .await?;
    if !old_shas.is_empty() {
        sqlx::query(
            "DELETE FROM code_blobs b WHERE b.sha = ANY($1)
                AND NOT EXISTS (SELECT 1 FROM code_files f WHERE f.blob_sha = b.sha)",
        )
        .bind(&old_shas)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    stats.commits = index_commits(
        state,
        &store,
        repo_id,
        &head,
        previous.map(|p| p.commit_sha),
    )
    .await?;
    Ok(stats)
}

struct CommitData {
    sha: String,
    tree: String,
    parents: String,
    message: String,
    author_name: String,
    author_email: String,
    author_date: DateTime<Utc>,
    committer_name: String,
    committer_email: String,
    committer_date: DateTime<Utc>,
}

/// Index commits reachable from `head` and not from `previous`.
async fn index_commits(
    state: &AppState,
    store: &RepoStore,
    repo_id: i64,
    head: &str,
    previous: Option<String>,
) -> anyhow::Result<usize> {
    let head = head.to_string();
    let commits: Vec<CommitData> = store
        .read(repo_id, move |r| {
            // A previous head that no longer exists (gc'd after a force
            // push) is ignored.
            let exclude: Vec<String> = previous
                .into_iter()
                .filter(|p| r.header(p).ok().flatten().is_some())
                .collect();
            let ex: Vec<&str> = exclude.iter().map(String::as_str).collect();
            let shas = r.rev_list(&head, &ex, MAX_COMMITS)?;
            let mut out = Vec::with_capacity(shas.len());
            for sha in shas {
                let c = r.commit(&sha)?;
                out.push(CommitData {
                    sha,
                    tree: c.tree.clone(),
                    parents: c.parents.join(" "),
                    message: c.message.replace('\0', ""),
                    author_name: c.author.name.clone(),
                    author_email: c.author.email.clone(),
                    author_date: c.author.when.with_timezone(&Utc),
                    committer_name: c.committer.name.clone(),
                    committer_email: c.committer.email.clone(),
                    committer_date: c.committer.when.with_timezone(&Utc),
                });
            }
            Ok(out)
        })
        .await?;
    if commits.is_empty() {
        return Ok(0);
    }
    let n = commits.len();
    for chunk in commits.chunks(1000) {
        let col = |f: fn(&CommitData) -> String| chunk.iter().map(f).collect::<Vec<String>>();
        let dates = |f: fn(&CommitData) -> DateTime<Utc>| chunk.iter().map(f).collect::<Vec<_>>();
        sqlx::query(
            "INSERT INTO commit_index (repo_id, sha, tree_sha, parents, message, author_name,
                                       author_email, author_date, committer_name,
                                       committer_email, committer_date, author_id, committer_id)
             SELECT $1, x.sha, x.tree, coalesce(string_to_array(nullif(x.parents, ''), ' '), '{}'), x.message,
                    x.an, x.ae, x.ad, x.cn, x.ce, x.cd,
                    (SELECT e.user_id FROM user_emails e WHERE lower(e.email) = lower(x.ae) AND e.verified LIMIT 1),
                    (SELECT e.user_id FROM user_emails e WHERE lower(e.email) = lower(x.ce) AND e.verified LIMIT 1)
               FROM unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[],
                           $8::timestamptz[], $9::text[], $10::text[], $11::timestamptz[])
                    AS x(sha, tree, parents, message, an, ae, ad, cn, ce, cd)
             ON CONFLICT (repo_id, sha) DO NOTHING",
        )
        .bind(repo_id)
        .bind(col(|c| c.sha.clone()))
        .bind(col(|c| c.tree.clone()))
        .bind(col(|c| c.parents.clone()))
        .bind(col(|c| c.message.clone()))
        .bind(col(|c| c.author_name.clone()))
        .bind(col(|c| c.author_email.clone()))
        .bind(dates(|c| c.author_date))
        .bind(col(|c| c.committer_name.clone()))
        .bind(col(|c| c.committer_email.clone()))
        .bind(dates(|c| c.committer_date))
        .execute(&state.db)
        .await?;
    }
    Ok(n)
}
