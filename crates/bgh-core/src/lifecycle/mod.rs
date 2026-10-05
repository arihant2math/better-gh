//! Account and repository lifecycle shared by bgh-accounts, bgh-repos and
//! bgh-admin (P50): owner/repo resolution through redirects, account
//! renames with redirects and login reservation, repository soft delete and
//! restore, account deletion.
//!
//! Redirects: a renamed or transferred repository keeps answering on its old
//! `owner/name` (`repo_redirects`, inserted for every owned repository when
//! its owner is renamed too), and a renamed account's old login resolves to
//! it (`login_redirects`, reserved for [`LOGIN_RESERVATION_DAYS`] by a
//! trigger on `users`, migration 6200). Every lookup that maps a URL to a
//! repository or owner (REST, git over HTTP and SSH, LFS, the container
//! registry) goes through [`resolve_repo`] / [`resolve_owner`].

pub mod snapshot;

use serde_json::Value;

use crate::db::Tx;
use crate::error::{ApiError, ApiResult, FieldError, unique_violation};
use crate::events::Event;
use crate::models::db::{self, Repository, User};
use crate::sync::shapes::Model as SyncModel;
use crate::sync::{self, SyncAction};

/// Days an old login stays reserved for the renamed account.
pub const LOGIN_RESERVATION_DAYS: i64 = 90;
/// Days a deleted repository can be restored before it is purged.
pub const REPO_RETENTION_DAYS: i64 = 90;
/// Hours a pending transfer to a user stays acceptable.
pub const TRANSFER_TTL_HOURS: i64 = 24;
/// Self-service renames allowed per account per day.
pub const RENAMES_PER_DAY: u64 = 3;

/// An account by login, following `login_redirects` for renamed accounts.
pub async fn resolve_owner(
    db: impl sqlx::PgExecutor<'_>,
    login: &str,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {cols} FROM (
            SELECT {u}, 0 AS rank FROM users u WHERE lower(u.login) = lower($1)
            UNION ALL
            SELECT {u}, 1 AS rank FROM login_redirects r JOIN users u ON u.id = r.user_id
             WHERE lower(r.old_login) = lower($1)
         ) x ORDER BY rank LIMIT 1",
        cols = User::COLUMNS,
        u = db::prefixed("u", User::COLUMNS),
    ))
    .bind(login)
    .fetch_optional(db)
    .await
}

/// A repository by `owner/name`: the current name, else a repository
/// redirect (renamed / transferred repositories), else the same name under
/// a renamed owner.
pub async fn resolve_repo(
    pool: &sqlx::PgPool,
    owner: &str,
    name: &str,
) -> Result<Option<(Repository, User)>, sqlx::Error> {
    let name = name.strip_suffix(".git").unwrap_or(name);
    let direct = User::find_by_login(pool, owner).await?;
    if let Some(owner) = &direct
        && let Some(repo) = Repository::find_by_name(pool, owner.id, name).await?
    {
        return Ok(Some((repo, owner.clone())));
    }
    let redirected: Option<Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = (
            SELECT repo_id FROM repo_redirects
             WHERE lower(owner_login) = lower($1) AND lower(name) = lower($2))",
        db::prefixed("repositories", Repository::COLUMNS)
    ))
    .bind(owner)
    .bind(name)
    .fetch_optional(pool)
    .await?;
    if let Some(repo) = redirected {
        return Ok(User::find(pool, repo.owner_id)
            .await?
            .map(|owner| (repo, owner)));
    }
    if direct.is_none()
        && let Some(owner) = resolve_owner(pool, owner).await?
        && let Some(repo) = Repository::find_by_name(pool, owner.id, name).await?
    {
        return Ok(Some((repo, owner)));
    }
    Ok(None)
}

fn resource(account: &User) -> &'static str {
    if account.is_org() {
        "Organization"
    } else {
        "User"
    }
}

/// Rename `account` inside `tx`: every repository it owns gets a redirect
/// from `old_login/name`, the old login redirects to the account and is
/// reserved for [`LOGIN_RESERVATION_DAYS`], and clients get the new names.
/// A taken or reserved login is 422 `already_exists`. Validation of the
/// login's format and audit/events are the caller's job.
pub async fn rename_account_in(tx: &mut Tx, account: &User, new_login: &str) -> ApiResult<User> {
    let res = resource(account);
    let renamed: User = sqlx::query_as(&format!(
        "UPDATE users SET login = $2, updated_at = now() WHERE id = $1 RETURNING {}",
        User::COLUMNS
    ))
    .bind(account.id)
    .bind(new_login)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| match unique_violation(&e).as_deref() {
        Some("users_login_key") => {
            ApiError::invalid_field(FieldError::already_exists(res, "login"))
        }
        _ => e.into(),
    })?;
    // A case-only change needs no redirects (lookups ignore case).
    if !renamed.login.eq_ignore_ascii_case(&account.login) {
        sqlx::query(
            "INSERT INTO login_redirects (old_login, user_id, reserved_until)
             VALUES ($1, $2, now() + make_interval(days => $3))
             ON CONFLICT ((lower(old_login))) DO UPDATE
                SET user_id = EXCLUDED.user_id, reserved_until = EXCLUDED.reserved_until,
                    created_at = now()",
        )
        .bind(&account.login)
        .bind(account.id)
        .bind(LOGIN_RESERVATION_DAYS as i32)
        .execute(&mut **tx)
        .await?;
        // Redirects that would shadow the repositories' new names.
        sqlx::query(
            "DELETE FROM repo_redirects rr USING repositories r
              WHERE r.owner_id = $1 AND lower(rr.owner_login) = lower($2)
                AND lower(rr.name) = lower(r.name)",
        )
        .bind(account.id)
        .bind(&renamed.login)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO repo_redirects (owner_login, name, repo_id)
             SELECT $2, name, id FROM repositories WHERE owner_id = $1
             ON CONFLICT ((lower(owner_login)), (lower(name)))
             DO UPDATE SET repo_id = EXCLUDED.repo_id, created_at = now()",
        )
        .bind(account.id)
        .bind(&account.login)
        .execute(&mut **tx)
        .await?;
    }
    let repos: Vec<i64> = sqlx::query_scalar("SELECT id FROM repositories WHERE owner_id = $1")
        .bind(account.id)
        .fetch_all(&mut **tx)
        .await?;
    tx.sync_models(SyncModel::Repo, &repos, SyncAction::Update)
        .await?;
    if renamed.is_org() {
        tx.sync_model(SyncModel::Org, renamed.id, SyncAction::Update)
            .await?;
    } else {
        tx.sync_user(renamed.id).await?;
    }
    Ok(renamed)
}

/// Content-addressed blobs a snapshot references.
fn blob_refs(snap: &snapshot::Snapshot, table: &str, column: &str) -> Vec<String> {
    let mut out: Vec<String> = snap
        .rows(table)
        .iter()
        .filter_map(|r| r.get(column).and_then(Value::as_str).map(str::to_string))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Soft-delete `repo` inside `tx`: its rows (and everything cascading from
/// them) move into a `deleted_repositories` snapshot, so the name is free
/// at once; git storage stays until the purge (bgh-repos
/// `repos.purge_deleted`). Records the sync delete and emits
/// [`Event::RepositoryDeleted`]. Audit is the caller's job.
pub async fn soft_delete_repo_in(
    tx: &mut Tx,
    actor_id: i64,
    owner: &User,
    repo: &Repository,
) -> ApiResult<()> {
    let forks: Vec<i64> = sqlx::query_scalar("SELECT id FROM repositories WHERE parent_id = $1")
        .bind(repo.id)
        .fetch_all(&mut **tx)
        .await?;
    let snap = snapshot::capture(tx, repo.id).await?;
    let lfs = blob_refs(&snap, "lfs_objects", "oid");
    let blobs = blob_refs(&snap, "attachments", "sha256");
    sqlx::query(
        "INSERT INTO deleted_repositories
            (id, owner_id, owner_login, name, visibility, fork, deleted_by_id, purge_after,
             forks, lfs_oids, blob_shas, snapshot)
         VALUES ($1, $2, $3, $4, $5, $6, (SELECT id FROM users WHERE id = $7),
                 now() + make_interval(days => $8), $9, $10, $11, $12)
         ON CONFLICT (id) DO UPDATE SET snapshot = EXCLUDED.snapshot, deleted_at = now(),
             purge_after = EXCLUDED.purge_after",
    )
    .bind(repo.id)
    .bind(owner.id)
    .bind(&owner.login)
    .bind(&repo.name)
    .bind(&repo.visibility)
    .bind(repo.fork)
    .bind(actor_id)
    .bind(REPO_RETENTION_DAYS as i32)
    .bind(&forks)
    .bind(&lfs)
    .bind(&blobs)
    .bind(sqlx::types::Json(&snap))
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM repositories WHERE id = $1")
        .bind(repo.id)
        .execute(&mut **tx)
        .await?;
    if let Some(parent) = repo.parent_id {
        sqlx::query(
            "UPDATE repositories SET forks_count = greatest(forks_count - 1, 0) WHERE id = $1",
        )
        .bind(parent)
        .execute(&mut **tx)
        .await?;
    }
    tx.sync_delete(&sync::repo_scope(repo.id), SyncModel::Repo, repo.id)
        .await?;
    tx.emit(Event::RepositoryDeleted {
        repo_id: repo.id,
        owner_id: owner.id,
        full_name: format!("{}/{}", owner.login, repo.name),
        actor_id,
    });
    Ok(())
}

/// A soft-deleted repository (`deleted_repositories` without the snapshot).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeletedRepo {
    pub id: i64,
    pub owner_id: i64,
    pub owner_login: String,
    pub name: String,
    pub visibility: String,
    pub fork: bool,
    pub deleted_by_id: Option<i64>,
    pub deleted_at: chrono::DateTime<chrono::Utc>,
    pub purge_after: chrono::DateTime<chrono::Utc>,
}

impl DeletedRepo {
    pub const COLUMNS: &'static str =
        "id, owner_id, owner_login, name, visibility, fork, deleted_by_id, deleted_at, purge_after";

    pub async fn find(db: impl sqlx::PgExecutor<'_>, id: i64) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM deleted_repositories WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }
}

/// Why a deleted repository can't be restored right now (`None`: it can).
pub async fn restore_blocker(
    db: impl sqlx::PgExecutor<'_>,
    deleted: &DeletedRepo,
) -> Result<Option<&'static str>, sqlx::Error> {
    let (owner, taken): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM users WHERE id = $1),
                EXISTS (SELECT 1 FROM repositories WHERE owner_id = $1 AND lower(name) = lower($2))",
    )
    .bind(deleted.owner_id)
    .bind(&deleted.name)
    .fetch_one(db)
    .await?;
    Ok(if !owner {
        Some("owner")
    } else if taken {
        Some("name")
    } else {
        None
    })
}

/// Restore a soft-deleted repository inside `tx` with its original id,
/// rows and storage. 404 when unknown (or purged); 422 when the owner is
/// gone or the name is taken again. Records sync inserts; audit, jobs and
/// events are the caller's job.
pub async fn restore_repo_in(tx: &mut Tx, id: i64) -> ApiResult<(Repository, User)> {
    let deleted: DeletedRepo = sqlx::query_as(&format!(
        "SELECT {} FROM deleted_repositories WHERE id = $1 FOR UPDATE",
        DeletedRepo::COLUMNS
    ))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    match restore_blocker(&mut **tx, &deleted).await? {
        Some("owner") => {
            return Err(ApiError::invalid_field(FieldError::custom(
                "Repository",
                "owner",
                "The account that owned this repository no longer exists",
            )));
        }
        Some(_) => {
            return Err(ApiError::Validation {
                message: "Repository name already exists on this account".into(),
                errors: vec![FieldError::already_exists("Repository", "name")],
            });
        }
        None => {}
    }
    let snap: sqlx::types::Json<snapshot::Snapshot> =
        sqlx::query_scalar("SELECT snapshot FROM deleted_repositories WHERE id = $1")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    let stats = snapshot::restore(tx, &snap.0).await?;
    tracing::info!(
        repo_id = id,
        inserted = stats.inserted,
        skipped = stats.skipped,
        "repository restored"
    );
    let repo = Repository::find(&mut **tx, id).await?.ok_or_else(|| {
        ApiError::Internal(anyhow::anyhow!("restored snapshot has no repository row"))
    })?;
    let owner = User::find(&mut **tx, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Counters that other rows changed meanwhile.
    sqlx::query(
        "UPDATE repositories SET
            forks_count = (SELECT count(*) FROM repositories f WHERE f.parent_id = $1),
            stargazers_count = (SELECT count(*) FROM stars s WHERE s.repo_id = $1)
          WHERE id = $1",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    if let Some(parent) = repo.parent_id {
        sqlx::query("UPDATE repositories SET forks_count = forks_count + 1 WHERE id = $1")
            .bind(parent)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("DELETE FROM deleted_repositories WHERE id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    tx.sync_model(SyncModel::Repo, id, SyncAction::Insert)
        .await?;
    if !owner.is_org() {
        tx.sync_viewer_repo(owner.id, id).await?;
    }
    tx.emit(Event::AccessChanged {
        repo_id: Some(id),
        org_id: owner.is_org().then_some(owner.id),
        user_id: None,
    });
    Ok((repo, owner))
}

/// Delete `account` inside `tx`: owned repositories are soft-deleted,
/// authored content stays and renders as `ghost` (author FKs are `ON
/// DELETE SET NULL`). Sole-owner checks, audit, events and ending sessions
/// are the caller's job.
pub async fn delete_account_in(tx: &mut Tx, actor_id: i64, account: &User) -> ApiResult<usize> {
    let repos: Vec<Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE owner_id = $1 ORDER BY id FOR UPDATE",
        Repository::COLUMNS
    ))
    .bind(account.id)
    .fetch_all(&mut **tx)
    .await?;
    for repo in &repos {
        soft_delete_repo_in(tx, actor_id, account, repo).await?;
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(account.id)
        .execute(&mut **tx)
        .await?;
    if account.is_org() {
        tx.sync_delete(&sync::org_scope(account.id), SyncModel::Org, account.id)
            .await?;
    }
    Ok(repos.len())
}

/// Organizations where `user_id` is the only owner (blocks deleting the
/// user, like GitHub).
pub async fn sole_owned_orgs(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT o.login FROM org_members m JOIN users o ON o.id = m.org_id
          WHERE m.user_id = $1 AND m.role = 'admin'
            AND NOT EXISTS (SELECT 1 FROM org_members m2
                             WHERE m2.org_id = m.org_id AND m2.role = 'admin'
                               AND m2.user_id <> $1)
          ORDER BY lower(o.login)",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Purge repositories past their retention: the storage (repository,
/// wiki) goes in the owners' jobs (`repos.delete_storage`,
/// `wiki.delete_storage`), blobs only they referenced in the next LFS and
/// uploads GC runs. Also drops expired transfer requests. Returns how many
/// repositories were purged. Run hourly by bgh-repos `repos.purge_deleted`.
pub async fn purge_expired(state: &crate::AppState) -> anyhow::Result<usize> {
    use serde_json::json;
    let mut tx = Tx::begin(state).await?;
    let purged: Vec<(i64, Vec<i64>)> = sqlx::query_as(
        "DELETE FROM deleted_repositories WHERE id IN (
            SELECT id FROM deleted_repositories WHERE purge_after <= now()
             ORDER BY purge_after LIMIT 100 FOR UPDATE SKIP LOCKED)
         RETURNING id, forks",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (repo_id, forks) in &purged {
        crate::jobs::enqueue(
            &mut *tx,
            "repos.delete_storage",
            &json!({ "repo_id": repo_id, "forks": forks }),
        )
        .await?;
        crate::jobs::enqueue(
            &mut *tx,
            "wiki.delete_storage",
            &json!({ "repo_id": repo_id }),
        )
        .await?;
    }
    if !purged.is_empty() {
        crate::jobs::enqueue(&mut *tx, "repos.lfs_gc", &json!({})).await?;
        crate::jobs::enqueue(&mut *tx, "uploads.gc", &json!({})).await?;
    }
    sqlx::query("DELETE FROM repo_transfers WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(purged.len())
}
