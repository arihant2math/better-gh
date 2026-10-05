//! `bgh`: the Better GitHub server binary.
//!
//! ```text
//! bgh [serve]                                   run migrations, HTTP server, job workers
//! bgh migrate                                   apply database migrations and exit
//! bgh admin create-user --login L --email E --password P [--site-admin]
//! bgh admin create-org --login L --admin USER [--name NAME]
//! bgh admin create-token --user L [--scopes repo,read:org] [--name N] [--expires-in-days D]
//! bgh healthcheck                               exit 0 if the local server is healthy
//! ```
//! Configuration comes from environment variables (see `bgh_core::config`).

use std::sync::Arc;

use anyhow::Context;
use bgh_core::Config;
use bgh_core::registry::{Registry, spawn_listeners};
use bgh_core::state::AppState;
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "bgh", version, about = "Better GitHub server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server and background workers (default).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Administrative tasks.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Probe `GET /healthz` on the local server (BGH_LISTEN); exit status 0
    /// when it answers 200. For container and service-manager health checks.
    Healthcheck {
        /// Seconds to wait for the answer.
        #[arg(long, default_value_t = 5)]
        timeout: u64,
    },
}

// Variant names are the CLI's subcommand names (`create-user`, ...).
#[allow(clippy::enum_variant_names)]
#[derive(Subcommand)]
enum AdminCommand {
    /// Create a user account.
    CreateUser {
        #[arg(long)]
        login: String,
        #[arg(long)]
        email: String,
        #[arg(long, env = "BGH_ADMIN_PASSWORD")]
        password: String,
        /// Make the user a site administrator.
        #[arg(long)]
        site_admin: bool,
    },
    /// Create an organization administered by an existing user.
    CreateOrg {
        #[arg(long)]
        login: String,
        /// Login of the organization's first admin.
        #[arg(long)]
        admin: String,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Create a personal access token for a user and print it to stdout.
    CreateToken {
        /// Login of the token's owner.
        #[arg(long)]
        user: String,
        /// Comma-separated classic scopes.
        #[arg(long, default_value = "repo,read:org")]
        scopes: String,
        /// Token name (GitHub's "note").
        #[arg(long, default_value = "bgh admin create-token")]
        name: String,
        /// Lifetime in days (default: never expires).
        #[arg(long)]
        expires_in_days: Option<i64>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,tower_http=info".into()),
        )
        .init();
    let cli = Cli::parse();
    let config = Config::from_env()?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(config).await,
        Command::Migrate => {
            let state = AppState::connect(config).await?;
            bgh_core::db::migrate(&state.db).await?;
            println!("migrations applied");
            Ok(())
        }
        Command::Admin { command } => admin(config, command).await,
        Command::Healthcheck { timeout } => healthcheck(&config, timeout).await,
    }
}

async fn admin(config: Config, command: AdminCommand) -> anyhow::Result<()> {
    let state = AppState::connect(config).await?;
    bgh_core::db::migrate(&state.db).await?;
    let api = |e: bgh_core::ApiError| anyhow::anyhow!("{e}: {e:?}");
    match command {
        AdminCommand::CreateUser {
            login,
            email,
            password,
            site_admin,
        } => {
            let user = bgh_accounts::create_user(
                &state,
                bgh_accounts::NewAccount {
                    login: &login,
                    email: &email,
                    password: &password,
                    name: None,
                    site_admin: Some(site_admin),
                },
                None,
            )
            .await
            .map_err(api)?;
            println!(
                "created user {} (id {}, site_admin={})",
                user.login, user.id, user.site_admin
            );
        }
        AdminCommand::CreateOrg { login, admin, name } => {
            let admin_user = bgh_core::models::db::User::find_by_login(&state.db, &admin)
                .await?
                .with_context(|| format!("no user {admin:?}"))?;
            let org =
                bgh_accounts::create_org(&state, &login, name.as_deref(), &admin_user, &admin_user)
                    .await
                    .map_err(api)?;
            println!(
                "created organization {} (id {}) with admin {}",
                org.login, org.id, admin_user.login
            );
        }
        AdminCommand::CreateToken {
            user,
            scopes,
            name,
            expires_in_days,
        } => {
            let owner = bgh_core::models::db::User::find_by_login(&state.db, &user)
                .await?
                .with_context(|| format!("no user {user:?}"))?;
            let scopes: Vec<String> = scopes
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            for s in &scopes {
                anyhow::ensure!(
                    bgh_accounts::tokens::KNOWN_SCOPES.contains(&s.as_str()),
                    "unknown scope {s:?}"
                );
                anyhow::ensure!(
                    s != "site_admin" || owner.site_admin,
                    "site_admin scope requires a site administrator"
                );
            }
            let expires_at =
                expires_in_days.map(|d| chrono::Utc::now() + chrono::Duration::days(d));
            let mut tx = state.db.begin().await?;
            let (row, token) =
                bgh_core::auth::create_access_token(&mut *tx, owner.id, &name, &scopes, expires_at)
                    .await
                    .map_err(api)?;
            bgh_core::audit::log(
                &mut *tx,
                None,
                "personal_access_token.create",
                bgh_core::audit::Target::Token(row.id),
                serde_json::json!({ "scopes": scopes, "user": owner.login, "via": "cli" }),
            )
            .await?;
            tx.commit().await?;
            eprintln!(
                "created token {} for {} (scopes: {})",
                row.id,
                owner.login,
                scopes.join(",")
            );
            println!("{token}");
        }
    }
    Ok(())
}

/// `bgh healthcheck`: HTTP/1.1 `GET /healthz` against the configured listen
/// address (loopback when it binds all interfaces), without a DB connection.
async fn healthcheck(config: &Config, timeout: u64) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut addr = config.listen_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(match addr {
            std::net::SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            std::net::SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
    }
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        anyhow::Ok(String::from_utf8_lossy(&buf).into_owned())
    };
    let response = tokio::time::timeout(std::time::Duration::from_secs(timeout), probe)
        .await
        .with_context(|| format!("no answer from {addr} within {timeout}s"))?
        .with_context(|| format!("probing {addr}"))?;
    let status = response.lines().next().unwrap_or_default();
    let body = response.split("\r\n\r\n").nth(1).unwrap_or_default().trim();
    anyhow::ensure!(
        status.split_whitespace().nth(1) == Some("200"),
        "unhealthy: {status} {body}"
    );
    println!("{body}");
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let listen = config.listen_addr;
    let workers = config.job_workers;
    std::fs::create_dir_all(config.repos_dir())
        .with_context(|| format!("creating {}", config.repos_dir().display()))?;
    let state = AppState::connect(config).await?;
    bgh_core::db::migrate(&state.db)
        .await
        .context("running migrations")?;

    let mut registry = Registry::new();
    bgh_server::register(&mut registry);
    let shutdown = CancellationToken::new();

    let listeners = spawn_listeners(&state, &registry.listeners, shutdown.clone());
    let worker_task = tokio::spawn(bgh_core::jobs::run_workers(
        state.clone(),
        Arc::new(registry.jobs.clone()),
        workers,
        shutdown.clone(),
    ));

    let app = bgh_server::app(state.clone());
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(addr = %listen, base_url = %state.config.base_url, "listening");

    let http_shutdown = shutdown.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        wait_for_signal().await;
        tracing::info!("shutting down");
        http_shutdown.cancel();
    })
    .await?;

    shutdown.cancel();
    let _ = worker_task.await;
    for l in listeners {
        let _ = l.await;
    }
    tracing::info!("bye");
    Ok(())
}

async fn wait_for_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
