//! `bgh`: the Better GitHub server binary.
//!
//! ```text
//! bgh [serve]                                   run migrations, HTTP server, job workers
//! bgh migrate                                   apply database migrations and exit
//! bgh admin create-user --login L --email E --password P [--site-admin]
//! bgh admin create-org --login L --admin USER [--name NAME]
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
}

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
    }
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
