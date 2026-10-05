//! Built-in SSH server for git (russh).
//!
//! * Public-key auth only: users' keys (`ssh_keys`) act with the user's
//!   permissions; deploy keys (`deploy_keys`) get read, or write unless
//!   `read_only`, on their repository only. Fingerprints are OpenSSH
//!   `SHA256:<base64>` strings.
//! * Exec commands: `git-upload-pack`, `git-receive-pack` (same permission
//!   checks, branch protection and `repos.post_receive` job as HTTP) and
//!   `git-lfs-authenticate` (issues a `RemoteAuth` token for the LFS HTTP
//!   API). Protocol v2 is honored via the `GIT_PROTOCOL` env request.
//! * The Ed25519 host key is generated once and persisted at
//!   `{data_dir}/ssh/host_ed25519_key`.
//!
//! Started by `bgh serve` as the `ssh` service when `BGH_SSH_ENABLED`
//! (port `BGH_SSH_PORT`); tests call [`spawn`] on an ephemeral port.

pub mod command;
pub mod exec;
pub mod keys;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bgh_core::prelude::*;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio_util::sync::CancellationToken;

use keys::Principal;

/// `{data_dir}/ssh/host_ed25519_key`
pub fn host_key_path(state: &AppState) -> std::path::PathBuf {
    state.config.data_dir.join("ssh").join("host_ed25519_key")
}

fn server_config(state: &AppState) -> anyhow::Result<russh::server::Config> {
    let key = keys::load_or_generate_host_key(&host_key_path(state))?;
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);
    Ok(russh::server::Config {
        server_id: russh::SshId::Standard("SSH-2.0-BetterGitHub".into()),
        methods,
        auth_rejection_time: Duration::from_millis(300),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        inactivity_timeout: Some(Duration::from_secs(3600)),
        keepalive_interval: Some(Duration::from_secs(60)),
        nodelay: true,
        ..Default::default()
    })
}

/// The `ssh` service for [`bgh_core::Registry::service`].
pub async fn service(state: AppState, shutdown: CancellationToken) -> anyhow::Result<()> {
    if !state.config.ssh_enabled {
        return Ok(());
    }
    let addr = SocketAddr::new(state.config.listen_addr.ip(), state.config.ssh_port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "ssh listening");
    run(state, listener, shutdown).await
}

/// Serve SSH on an already bound listener until `shutdown`.
pub async fn run(
    state: AppState,
    listener: tokio::net::TcpListener,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let config = Arc::new(server_config(&state)?);
    let mut server = SshServer { state };
    let running = server.run_on_socket(config, &listener);
    let handle = running.handle();
    tokio::select! {
        r = running => r?,
        _ = shutdown.cancelled() => handle.shutdown("server shutting down".into()),
    }
    Ok(())
}

/// Bind `addr` (use port 0 in tests) and serve in the background. Returns
/// the bound address; the server stops when `shutdown` is cancelled.
pub async fn spawn(
    state: AppState,
    addr: SocketAddr,
    shutdown: CancellationToken,
) -> anyhow::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    // Generate the host key before accepting connections.
    keys::load_or_generate_host_key(&host_key_path(&state))?;
    tokio::spawn(async move {
        if let Err(err) = run(state, listener, shutdown).await {
            tracing::error!(?err, "ssh server failed");
        }
    });
    Ok(local)
}

struct SshServer {
    state: AppState,
}

impl Server for SshServer {
    type Handler = Connection;

    fn new_client(&mut self, peer: Option<SocketAddr>) -> Connection {
        Connection {
            state: self.state.clone(),
            principal: None,
            channels: HashMap::new(),
            protocol: HashMap::new(),
            peer,
        }
    }

    fn handle_session_error(&mut self, error: anyhow::Error) {
        tracing::debug!(?error, "ssh session error");
    }
}

/// Per-connection handler.
pub struct Connection {
    state: AppState,
    principal: Option<Principal>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    /// `GIT_PROTOCOL` sent via env requests, per channel.
    protocol: HashMap<ChannelId, String>,
    peer: Option<SocketAddr>,
}

impl Connection {
    async fn principal_for(&self, key: &russh::keys::PublicKey) -> Option<Principal> {
        match keys::lookup(&self.state, key).await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(?err, "ssh key lookup failed");
                None
            }
        }
    }
}

impl Handler for Connection {
    type Error = anyhow::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(match self.principal_for(key).await {
            Some(_) => Auth::Accept,
            None => Auth::reject(),
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        match self.principal_for(key).await {
            Some(p) => {
                tracing::debug!(peer = ?self.peer, who = %p.name(), "ssh authenticated");
                self.principal = Some(p);
                Ok(Auth::Accept)
            }
            None => Ok(Auth::reject()),
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "GIT_PROTOCOL" && bgh_git::smart_http::valid_protocol(value) {
            self.protocol.insert(channel, value.to_string());
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (Some(ch), Some(p)) = (self.channels.remove(&channel), self.principal.clone()) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let site = self.state.config.site_name.clone();
        tokio::spawn(async move {
            let msg = match &p {
                Principal::User { ctx, .. } => format!(
                    "Hi {}! You've successfully authenticated, but {site} does not provide shell access.\r\n",
                    ctx.user.login
                ),
                Principal::Deploy(_) => format!(
                    "Hi deploy key! You've successfully authenticated, but {site} does not provide shell access.\r\n"
                ),
            };
            let _ = ch.extended_data_bytes(1, msg.into_bytes()).await;
            let _ = ch.exit_status(1).await;
            let _ = ch.eof().await;
            let _ = ch.close().await;
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let (Some(ch), Some(principal)) = (self.channels.remove(&channel), self.principal.clone())
        else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;
        let line = String::from_utf8_lossy(data).into_owned();
        let protocol = self.protocol.remove(&channel);
        let state = self.state.clone();
        tokio::spawn(exec::run(state, principal, line, ch, protocol));
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        self.protocol.remove(&channel);
        Ok(())
    }
}
