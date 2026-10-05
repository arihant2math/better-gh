//! Running an exec request on an SSH channel.

use bgh_core::prelude::*;
use bgh_git::smart_http::{self, Service};
use bytes::Bytes;
use futures::StreamExt;
use russh::server::Msg;
use russh::{Channel, ChannelMsg};
use tokio::io::AsyncWriteExt;
use tokio_util::io::StreamReader;

use super::command::{self, Command};
use super::keys::{self, Principal};
use crate::jobs::PostReceive;
use crate::protection;

/// Result of authorizing a command.
struct Authorized {
    access: RepoAccess,
    /// Acting user (for protection rules and post-receive).
    user: Option<AuthContext>,
    deploy_key_id: Option<i64>,
}

/// GitHub-style messages for the client (printed by git as `ERROR: ...`).
fn not_found() -> String {
    "ERROR: Repository not found.\n".into()
}

async fn authorize(
    state: &AppState,
    principal: &Principal,
    owner: &str,
    repo: &str,
    write: bool,
) -> Result<Authorized, String> {
    let denied = |who: &str| format!("ERROR: Permission to {owner}/{repo}.git denied to {who}.\n");
    let authz = match principal {
        Principal::User { ctx, .. } => {
            let access = RepoAccess::load(state, Some(ctx), owner, repo)
                .await
                .map_err(|e| match e {
                    // e.g. "Repository access blocked." for disabled repos
                    ApiError::Forbidden(m) => format!("ERROR: {m}\n"),
                    _ => not_found(),
                })?;
            if write && access.permission < Permission::Write {
                return Err(denied(&ctx.user.login));
            }
            Authorized {
                access,
                user: Some((**ctx).clone()),
                deploy_key_id: None,
            }
        }
        Principal::Deploy(deploy) => {
            let (repo_row, owner_row) = bgh_core::lifecycle::resolve_repo(&state.db, owner, repo)
                .await
                .ok()
                .flatten()
                .ok_or_else(not_found)?;
            let key = deploy
                .iter()
                .find(|k| k.repo_id == repo_row.id)
                .ok_or_else(not_found)?;
            if write && key.read_only {
                return Err(
                    "ERROR: The key you are authenticating with has been marked as read only.\n"
                        .into(),
                );
            }
            Authorized {
                access: RepoAccess {
                    repo: repo_row,
                    owner: owner_row,
                    permission: if key.read_only {
                        Permission::Read
                    } else {
                        Permission::Write
                    },
                    authenticated: true,
                },
                user: None,
                deploy_key_id: Some(key.id),
            }
        }
    };
    let site_admin = authz.user.as_ref().is_some_and(|u| u.user.site_admin);
    if authz.access.repo.disabled && !site_admin {
        return Err("ERROR: Repository access blocked.\n".into());
    }
    if write && authz.access.repo.archived {
        return Err("ERROR: This repository was archived so it is read-only.\n".into());
    }
    if write && authz.access.repo.mirror_url.is_some() {
        return Err(format!("ERROR: {}.\n", bgh_core::perms::MIRROR_READ_ONLY));
    }
    if write && let Err(e) = crate::import::require_not_importing(state, authz.access.repo.id).await
    {
        return Err(format!("ERROR: {e}\n"));
    }
    Ok(authz)
}

/// Client → server bytes of a channel as an `AsyncRead` (ends at EOF).
fn channel_reader(
    rx: russh::ChannelReadHalf,
) -> impl tokio::io::AsyncRead + Send + Unpin + 'static {
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    return Some((Ok::<Bytes, std::io::Error>(data), rx));
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => return None,
                Some(_) => continue,
            }
        }
    });
    StreamReader::new(stream.boxed())
}

/// Execute `line` for `principal` on `channel`, then send the exit status
/// and close the channel.
pub async fn run(
    state: AppState,
    principal: Principal,
    line: String,
    channel: Channel<Msg>,
    protocol: Option<String>,
) {
    let (rx, tx) = channel.split();
    let mut out = tx.make_writer();
    let mut err = tx.make_writer_ext(Some(1));
    let code = match execute(&state, &principal, &line, rx, &mut out, &mut err, protocol).await {
        Ok(code) => code,
        Err(msg) => {
            let _ = err.write_all(msg.as_bytes()).await;
            1
        }
    };
    let _ = out.flush().await;
    let _ = err.flush().await;
    let _ = tx.exit_status(code as u32).await;
    let _ = tx.eof().await;
    let _ = tx.close().await;
}

async fn execute<W, E>(
    state: &AppState,
    principal: &Principal,
    line: &str,
    rx: russh::ChannelReadHalf,
    out: &mut W,
    err: &mut E,
    protocol: Option<String>,
) -> Result<i32, String>
where
    W: tokio::io::AsyncWrite + Send + Unpin,
    E: tokio::io::AsyncWrite + Send + Unpin,
{
    let cmd = command::parse(line).map_err(|m| format!("ERROR: {m}\n"))?;
    let (owner, repo) = cmd.repo();
    let authz = authorize(state, principal, owner, repo, cmd.is_write()).await?;
    keys::touch(state, principal, authz.access.repo.id).await;
    let store = crate::store(state);
    let repo_id = authz.access.repo.id;
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "ssh command failed");
        "ERROR: Internal server error.\n".to_string()
    };
    match cmd {
        Command::UploadPack { .. } => smart_http::upload_pack_duplex(
            &store,
            repo_id,
            protocol.as_deref(),
            crate::traffic::observe_clone(state, repo_id, principal.visitor(), channel_reader(rx)),
            out,
            err,
        )
        .await
        .map_err(|e| internal(&e)),
        Command::ReceivePack { .. } => {
            // Same storage quota as git over HTTP (bgh_core::settings).
            if let Err(e) = bgh_core::settings::check_push_quota(state, &authz.access.repo).await {
                return Err(match e {
                    ApiError::Forbidden(m) => format!("ERROR: {m}\n"),
                    other => internal(&other),
                });
            }
            let limits = crate::git_http::push_limits(state, &authz.access.repo)
                .await
                .map_err(|e| internal(&e))?;
            let adv = smart_http::advertise_refs(
                &store,
                repo_id,
                Service::ReceivePack,
                protocol.as_deref(),
            )
            .await
            .map_err(|e| internal(&e))?;
            out.write_all(&adv).await.map_err(|e| internal(&e))?;
            out.flush().await.map_err(|e| internal(&e))?;
            let access = &authz.access;
            let rules = protection::RepoRules::load(&state.db, &access.repo)
                .await
                .map_err(|e| internal(&e))?;
            let pusher_id = authz.user.as_ref().map(|u| u.user.id);
            let actor = match (&authz.user, rules.is_empty()) {
                (_, true) => None,
                (Some(u), false) => Some(
                    protection::Actor::load(state, access, &u.user)
                        .await
                        .map_err(|e| internal(&e))?,
                ),
                (None, false) => Some(protection::Actor::deploy_key(access)),
            };
            let result = smart_http::receive_pack_stream_with_policy(
                &store,
                repo_id,
                channel_reader(rx),
                |updates| {
                    let rules = &rules;
                    async move {
                        crate::git_http::deny_hidden_refs(&updates)?;
                        let policy = match &actor {
                            None => smart_http::PushPolicy::default(),
                            Some(actor) => {
                                protection::authorize_push(state, rules, actor, &updates).await?
                            }
                        };
                        Ok(policy.with_limits(limits))
                    }
                },
            )
            .await
            .map_err(|e| match e {
                bgh_git::GitError::InvalidInput(m) => format!("ERROR: {m}\n"),
                other => internal(&other),
            })?;
            if !result.applied.is_empty() {
                bgh_core::jobs::enqueue_job(
                    &state.db,
                    &PostReceive {
                        repo_id,
                        pusher_id,
                        updates: result.applied.clone(),
                    },
                )
                .await
                .map_err(|e| internal(&e))?;
            }
            out.write_all(&result.output)
                .await
                .map_err(|e| internal(&e))?;
            Ok(0)
        }
        Command::LfsAuthenticate { upload, .. } => {
            let token = crate::lfs::issue_grant(
                state,
                &crate::lfs::LfsGrant {
                    repo_id,
                    user_id: authz.user.as_ref().map(|u| u.user.id),
                    deploy_key_id: authz.deploy_key_id,
                    write: upload,
                },
            )
            .await
            .map_err(|e| internal(&e))?;
            let body = serde_json::json!({
                "href": state.urls.html(&format!(
                    "/{}/{}.git/info/lfs",
                    authz.access.owner.login, authz.access.repo.name
                )),
                "header": { "Authorization": format!("RemoteAuth {token}") },
                "expires_in": crate::lfs::TOKEN_TTL_SECS,
            });
            out.write_all(body.to_string().as_bytes())
                .await
                .map_err(|e| internal(&e))?;
            out.write_all(b"\n").await.map_err(|e| internal(&e))?;
            Ok(0)
        }
    }
}
